//! Realtime delivery (spec Part E §11–§16). PostgreSQL is authority; realtime is
//! delivery only.
//!
//! - [`RealtimeEnvelope`] is the stable, provider-neutral client contract. It is built
//!   only from `REALTIME` outbox rows; `PROVIDER_COMMAND` rows can never become an
//!   envelope, and payloads are scrubbed of provider identities and secrets.
//! - [`RedisPublisher`] fans committed events out over Redis pub/sub
//!   (`cfmeet:rt:conference:{id}`). Redis is ephemeral: when it is down events stay
//!   PENDING in the outbox and subscribers fall back to tailing PostgreSQL.
//! - Subscriptions are authorized server-side per conference ([`Subscriber`]) and
//!   re-checked during the connection; knowing a conference id grants nothing.
//! - Recovery: every conference event carries a gap-free `sequence` (commit order).
//!   Clients reconnect with their last cursor; if it cannot be served they are told to
//!   refetch the canonical state from `GET /api/v2/conferences/{id}`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, OutboxChannel, RecordOrigin, Role},
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_participants, conference_sessions, outbox_events},
    services::{
        actor::{ActorContext, ActorKind},
        meeting_core::{
            conference_authority, find_conference, find_space, not_found_conference,
            own_participant, permission_denied, Core,
        },
        meeting_events::{event, Audience},
        meeting_permissions::{Permission, PermissionContext},
        rtc_metrics::{inc, RTC_METRICS as M},
        RedisService,
    },
    workers::outbox_relay::EventPublisher,
};

pub const SCHEMA_VERSION: u32 = 1;
/// Maximum events served from one replay request before a resync is required.
pub const MAX_REPLAY: u64 = 500;

pub fn conference_channel(conference_id: Uuid) -> String {
    format!("cfmeet:rt:conference:{conference_id}")
}

/// Stable provider-neutral realtime envelope.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeEnvelope {
    pub event_id: Uuid,
    pub event_type: String,
    pub schema_version: u32,
    pub conference_id: Option<Uuid>,
    pub meeting_space_id: Option<Uuid>,
    /// Gap-free per-conference sequence (commit order) — the replay cursor.
    pub sequence: Option<i64>,
    pub occurred_at: DateTime<Utc>,
    pub correlation_id: Option<String>,
    #[schema(value_type = Object)]
    pub payload: Value,
}

/// Keys never delivered to clients, at any depth (defense in depth: business payloads
/// do not contain them, but provider/secret material must never leak).
const FORBIDDEN_KEYS: &[&str] = &[
    "mediaIdentity",
    "mediaIdentities",
    "credential",
    "credentialHash",
    "token",
    "tokenHash",
    "participantToken",
    "passcode",
    "passcodeHash",
    "rtcRoomName",
    "rtcRoomId",
    "providerRoomId",
    "providerParticipantId",
    "internalNote",
    "denialInternalNote",
    "apiSecret",
    "secret",
];

pub fn scrub(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| !FORBIDDEN_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), scrub(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(scrub).collect()),
        other => other.clone(),
    }
}

/// Client envelope for a committed outbox row. `None` for provider commands.
pub fn envelope(row: &outbox_events::Model) -> Option<RealtimeEnvelope> {
    if row.channel != OutboxChannel::Realtime || event::is_provider_command(&row.event_type) {
        return None;
    }
    Some(RealtimeEnvelope {
        event_id: row.id,
        event_type: row.event_type.clone(),
        schema_version: SCHEMA_VERSION,
        conference_id: row.conference_id,
        meeting_space_id: row.meeting_space_id,
        sequence: row.conference_seq,
        occurred_at: row.created_at,
        correlation_id: row.request_id.clone(),
        payload: scrub(&row.payload),
    })
}

/// Redis pub/sub publisher for client realtime events.
pub struct RedisPublisher {
    redis: Arc<RedisService>,
}

impl RedisPublisher {
    pub fn new(redis: Arc<RedisService>) -> Self {
        Self { redis }
    }
}

#[async_trait]
impl EventPublisher for RedisPublisher {
    async fn publish(&self, event: &outbox_events::Model) -> Result<(), String> {
        let Some(env) = envelope(event) else {
            return Err("provider commands are never published to clients".into());
        };
        let channel = match env.conference_id {
            Some(c) => conference_channel(c),
            None => format!(
                "cfmeet:rt:space:{}",
                env.meeting_space_id.unwrap_or_default()
            ),
        };
        let body = serde_json::to_string(&env).map_err(|e| e.to_string())?;
        self.redis.publish(&channel, &body).await.map_err(|e| {
            inc(&M.realtime_publish_failures);
            e.to_string()
        })?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Subscription authorization
// ---------------------------------------------------------------------------

/// What a subscriber may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionScope {
    /// Admitted participants, members and owners: every conference event.
    Full,
    /// Waiting participants: conference lifecycle + events about themselves only.
    SelfOnly(Uuid),
}

/// Server-resolved principal of a subscription.
#[derive(Debug, Clone)]
pub enum SubscriberPrincipal {
    User(ActorContext),
    Participant(Uuid),
}

#[derive(Debug, Clone)]
pub struct Subscriber {
    pub conference_id: Uuid,
    pub meeting_space_id: Uuid,
    pub principal: SubscriberPrincipal,
    pub scope: SubscriptionScope,
    /// The subscriber's own participant row, if any.
    pub participant_id: Option<Uuid>,
    /// Part F: effective role (for permission-scoped audiences).
    pub role: Option<Role>,
    pub permissions: PermissionContext,
    /// Part F: current breakout context (assignment or host visit).
    pub breakout: Option<Uuid>,
    /// Policy: participants in a breakout still see main-room traffic.
    pub main_visible_in_breakouts: bool,
}

/// Parse a row's server-side audience. Unreadable audience = deliver to nobody.
pub fn row_audience(row: &outbox_events::Model) -> Result<Option<Audience>, serde_json::Error> {
    match &row.audience {
        None => Ok(None),
        Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some),
    }
}

impl Subscriber {
    fn holds(&self, permission: &str) -> bool {
        match (self.role, Permission::from_key(permission)) {
            (Some(role), Some(p)) => self.permissions.allows(role, p),
            _ => false,
        }
    }

    /// Part F §57: server-side audience evaluation.
    pub fn audience_allows(&self, audience: &Audience) -> bool {
        match audience {
            Audience::Participants {
                ids,
                moderator_permission,
            } => {
                self.participant_id
                    .map(|p| ids.contains(&p))
                    .unwrap_or(false)
                    || moderator_permission
                        .as_deref()
                        .map(|perm| self.holds(perm))
                        .unwrap_or(false)
            }
            Audience::Permission { permission } => self.holds(permission),
            Audience::Breakout { breakout_id } => {
                self.breakout == Some(*breakout_id)
                    || self.holds(Permission::BreakoutEnterAny.key())
            }
            Audience::MainRoom => {
                self.breakout.is_none()
                    || self.main_visible_in_breakouts
                    || self.holds(Permission::ChatModerate.key())
            }
            Audience::Breakouts => {
                self.breakout.is_some() || self.holds(Permission::BreakoutEnterAny.key())
            }
        }
    }

    /// Whether this subscriber may receive an event with this audience.
    pub fn may_receive_scoped(&self, env: &RealtimeEnvelope, audience: Option<&Audience>) -> bool {
        if let Some(a) = audience {
            if !self.audience_allows(a) {
                return false;
            }
        }
        self.may_receive(env)
    }

    /// Scope rule (Part E): waiting participants only see lifecycle + their own events.
    pub fn may_receive(&self, env: &RealtimeEnvelope) -> bool {
        match self.scope {
            SubscriptionScope::Full => true,
            SubscriptionScope::SelfOnly(pid) => {
                env.event_type.starts_with("conference.")
                    || env.payload["participantId"].as_str() == Some(&pid.to_string())
            }
        }
    }

    /// May this subscriber receive a committed outbox row?
    pub fn may_receive_row(&self, row: &outbox_events::Model, env: &RealtimeEnvelope) -> bool {
        match row_audience(row) {
            Ok(a) => self.may_receive_scoped(env, a.as_ref()),
            Err(_) => false,
        }
    }

    /// Events after which the subscription must be re-authorized.
    pub fn requires_recheck(&self, env: &RealtimeEnvelope) -> bool {
        // Part F: context / policy changes re-scope every subscriber.
        if matches!(
            env.event_type.as_str(),
            event::BREAKOUT_OPENED
                | event::BREAKOUT_CLOSED
                | event::COLLABORATION_POLICY_CHANGED
                | event::CHAT_POLICY_CHANGED
        ) {
            return true;
        }
        if env.event_type == event::BREAKOUT_PARTICIPANT_MOVED {
            return self
                .participant_id
                .map(|p| env.payload["participantId"].as_str() == Some(&p.to_string()))
                .unwrap_or(false);
        }
        matches!(
            env.event_type.as_str(),
            event::PARTICIPANT_REMOVED
                | event::PARTICIPANT_BANNED
                | event::PARTICIPANT_DENIED
                | event::PARTICIPANT_RETURNED_TO_WAITING
                | event::PARTICIPANT_ADMITTED
                | event::PARTICIPANT_ROLE_CHANGED
        ) && self
            .participant_id
            .map(|p| env.payload["participantId"].as_str() == Some(&p.to_string()))
            .unwrap_or(true)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TicketClaims {
    /// `user:<id>` or `participant:<id>`.
    sub: String,
    conf: Uuid,
    typ: String,
    exp: i64,
    iat: i64,
    /// Unique ticket id: consumed on first use (one-shot).
    #[serde(default)]
    jti: Option<String>,
}

pub struct RealtimeService {
    core: Core,
    redis: Option<Arc<RedisService>>,
    ticket_key: Vec<u8>,
    require_email_verification: bool,
    /// Consumed ticket ids (with expiry) when no Redis is configured: one-shot per
    /// instance. With Redis the consumption is shared by every instance.
    consumed: Arc<std::sync::Mutex<std::collections::HashMap<String, i64>>>,
}

/// Lifetime of a one-shot WebSocket subscription ticket.
pub const TICKET_TTL_SECS: i64 = 60;

impl RealtimeService {
    pub fn new(
        core: Core,
        redis: Option<Arc<RedisService>>,
        signing_secret: &str,
        require_email_verification: bool,
    ) -> Self {
        // Domain-separated key: a ticket is never an access token and vice versa.
        let ticket_key = format!("{signing_secret}::cfmeet-realtime-ticket-v1").into_bytes();
        Self {
            core,
            redis,
            ticket_key,
            require_email_verification,
            consumed: Arc::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.core.flags.conference_realtime_enabled
    }

    pub fn redis(&self) -> Option<Arc<RedisService>> {
        self.redis.clone()
    }

    pub fn require_enabled(&self) -> ApiResult<()> {
        if self.enabled() {
            Ok(())
        } else {
            Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Conference realtime is not enabled",
            ))
        }
    }

    async fn conference(&self, conference_id: Uuid) -> ApiResult<conference_sessions::Model> {
        let c = find_conference(&self.core.db, conference_id).await?;
        if c.origin == RecordOrigin::Legacy {
            return Err(not_found_conference());
        }
        Ok(c)
    }

    /// Authorize a subscription for the request's actor.
    pub async fn authorize(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<Subscriber> {
        let conference = self.conference(conference_id).await?;
        let principal = match &ctx.kind {
            ActorKind::RegisteredUser(_) => SubscriberPrincipal::User(ctx.clone()),
            ActorKind::ParticipantCredential(_) => {
                let p = own_participant(&self.core.db, conference_id, ctx, false)
                    .await?
                    .ok_or_else(|| {
                        ApiError::domain(
                            ErrorCode::MeetingAccessDenied,
                            "This participant credential is not valid for this conference",
                        )
                    })?;
                SubscriberPrincipal::Participant(p.id)
            }
            _ => {
                return Err(ApiError::domain(
                    ErrorCode::AuthenticationRequired,
                    "Sign in or present your participant credential to subscribe",
                ))
            }
        };
        self.resolve(&conference, principal).await
    }

    /// (Re)evaluate a principal against current authoritative state.
    pub async fn resolve(
        &self,
        conference: &conference_sessions::Model,
        principal: SubscriberPrincipal,
    ) -> ApiResult<Subscriber> {
        let denied = || permission_denied("You are not permitted to follow this conference");
        let space = find_space(&self.core.db, conference.meeting_space_id).await?;
        let (scope, participant, role) = match &principal {
            SubscriberPrincipal::User(ctx) => {
                match conference_authority(&self.core.db, &space, conference.id, ctx).await? {
                    Some(a) => (SubscriptionScope::Full, a.participant, Some(a.role)),
                    None => {
                        match own_participant(&self.core.db, conference.id, ctx, false).await? {
                            Some(p) if p.admission_state == AdmissionState::Waiting => {
                                (SubscriptionScope::SelfOnly(p.id), Some(p), None)
                            }
                            _ => return Err(denied()),
                        }
                    }
                }
            }
            SubscriberPrincipal::Participant(pid) => {
                let p = conference_participants::Entity::find_by_id(*pid)
                    .filter(conference_participants::Column::ConferenceId.eq(conference.id))
                    .one(&self.core.db)
                    .await?
                    .ok_or_else(denied)?;
                match p.admission_state {
                    AdmissionState::Admitted => {
                        let role = Some(p.role);
                        (SubscriptionScope::Full, Some(p), role)
                    }
                    AdmissionState::Waiting => (SubscriptionScope::SelfOnly(p.id), Some(p), None),
                    _ => return Err(denied()),
                }
            }
        };
        let breakout = match participant
            .as_ref()
            .filter(|p| p.admission_state == AdmissionState::Admitted)
        {
            Some(p) => crate::services::collaboration::breakouts::media_context(
                &self.core.db,
                conference,
                p,
            )
            .await?
            .breakout_id(),
            None => None,
        };
        let policy = crate::services::collaboration::effective_policy(&space, conference);
        Ok(Subscriber {
            conference_id: conference.id,
            meeting_space_id: conference.meeting_space_id,
            principal,
            scope,
            participant_id: participant.map(|p| p.id),
            role,
            permissions: PermissionContext::for_space(&space),
            breakout,
            main_visible_in_breakouts: policy.main_chat_visible_in_breakouts,
        })
    }

    /// Re-check a live subscription. `Err` = revoke.
    pub async fn recheck(&self, sub: &Subscriber) -> ApiResult<Subscriber> {
        let conference = self.conference(sub.conference_id).await?;
        self.resolve(&conference, sub.principal.clone()).await
    }

    /// One-shot ticket for browser WebSocket clients (cannot set headers).
    pub fn issue_ticket(&self, sub: &Subscriber) -> ApiResult<(String, DateTime<Utc>)> {
        let now = Utc::now();
        let exp = now + Duration::seconds(TICKET_TTL_SECS);
        let subject = match &sub.principal {
            SubscriberPrincipal::User(ctx) => format!(
                "user:{}",
                ctx.user_id()
                    .ok_or_else(|| ApiError::InternalError("user principal without id".into()))?
            ),
            SubscriberPrincipal::Participant(p) => format!("participant:{p}"),
        };
        let token = encode(
            &Header::default(),
            &TicketClaims {
                sub: subject,
                conf: sub.conference_id,
                typ: "rt".into(),
                exp: exp.timestamp(),
                iat: now.timestamp(),
                jti: Some(Uuid::new_v4().simple().to_string()),
            },
            &EncodingKey::from_secret(&self.ticket_key),
        )
        .map_err(|e| ApiError::InternalError(format!("ticket signing failed: {e}")))?;
        Ok((token, exp))
    }

    /// Verify a ticket for `conference_id`, CONSUME it (a ticket opens one subscription:
    /// a replay is refused like an invalid ticket) and return its principal's subject.
    /// The consumption store failing refuses the ticket (fail closed).
    pub async fn verify_ticket(
        &self,
        ticket: &str,
        conference_id: Uuid,
    ) -> ApiResult<TicketSubject> {
        let invalid = || {
            ApiError::domain(
                ErrorCode::AuthenticationRequired,
                "Invalid or expired realtime ticket",
            )
        };
        let claims = decode::<TicketClaims>(
            ticket,
            &DecodingKey::from_secret(&self.ticket_key),
            &Validation::default(),
        )
        .map_err(|_| invalid())?
        .claims;
        if claims.typ != "rt" || claims.conf != conference_id {
            return Err(invalid());
        }
        let jti = claims
            .jti
            .as_deref()
            .filter(|j| j.len() == 32 && j.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(invalid)?;
        if !self.consume_ticket(jti, claims.exp).await? {
            tracing::warn!(conference_id = %conference_id, "realtime ticket replay refused");
            return Err(invalid());
        }
        let parse = |s: &str| Uuid::parse_str(s).map_err(|_| invalid());
        match claims.sub.split_once(':') {
            Some(("user", id)) => Ok(TicketSubject::User(parse(id)?)),
            Some(("participant", id)) => Ok(TicketSubject::Participant(parse(id)?)),
            _ => Err(invalid()),
        }
    }

    /// `true` when `jti` was not consumed before (and is now). Shared through Redis;
    /// while Redis is unavailable realtime degrades (as the event fan-out does) to the
    /// per-instance store, so a ticket stays single-use on the instance that took it.
    async fn consume_ticket(&self, jti: &str, exp: i64) -> ApiResult<bool> {
        let ttl = (exp - Utc::now().timestamp()).clamp(1, TICKET_TTL_SECS + 60) as u64;
        if let Some(redis) = &self.redis {
            let shared = async {
                let mut conn = redis.get_conn().await.ok()?;
                redis::cmd("SET")
                    .arg(format!("rt:ticket:{jti}"))
                    .arg("1")
                    .arg("NX")
                    .arg("EX")
                    .arg(ttl)
                    .query_async::<_, Option<String>>(&mut *conn)
                    .await
                    .ok()
            };
            match tokio::time::timeout(std::time::Duration::from_secs(2), shared).await {
                Ok(Some(set)) => {
                    // Also remember it locally: a later degraded check on this instance
                    // must not accept it again.
                    self.consume_locally(jti, exp);
                    return Ok(set.is_some());
                }
                _ => tracing::warn!(
                    "realtime ticket store (Redis) unavailable: per-instance one-shot check"
                ),
            }
        }
        Ok(self.consume_locally(jti, exp))
    }

    fn consume_locally(&self, jti: &str, exp: i64) -> bool {
        let now = Utc::now().timestamp();
        let mut consumed = self.consumed.lock().unwrap_or_else(|p| p.into_inner());
        consumed.retain(|_, e| *e >= now);
        consumed.insert(jti.to_string(), exp).is_none()
    }

    pub async fn current_sequence(&self, conference_id: Uuid) -> ApiResult<i64> {
        Ok(self.conference(conference_id).await?.event_seq)
    }

    /// Committed client events after `cursor`, in sequence order (gap-free per
    /// conference; never provider commands).
    pub async fn events_after(
        &self,
        conference_id: Uuid,
        cursor: i64,
        limit: u64,
    ) -> ApiResult<Vec<outbox_events::Model>> {
        Ok(outbox_events::Entity::find()
            .filter(outbox_events::Column::ConferenceId.eq(conference_id))
            .filter(outbox_events::Column::Channel.eq(OutboxChannel::Realtime))
            .filter(outbox_events::Column::ConferenceSeq.gt(cursor))
            .order_by_asc(outbox_events::Column::ConferenceSeq)
            .limit(limit)
            .all(&self.core.db)
            .await?)
    }

    /// Replay for recovery. `None` = cursor cannot be served (client must refetch
    /// canonical state from `GET /api/v2/conferences/{id}`).
    pub async fn replay(
        &self,
        sub: &Subscriber,
        cursor: i64,
    ) -> ApiResult<Option<(Vec<RealtimeEnvelope>, i64)>> {
        let current = self.current_sequence(sub.conference_id).await?;
        if cursor < 0 || cursor > current {
            return Ok(None);
        }
        if current - cursor > MAX_REPLAY as i64 {
            return Ok(None);
        }
        let rows = self
            .events_after(sub.conference_id, cursor, MAX_REPLAY)
            .await?;
        // Gap check: sequences are gap-free; a gap means rows were purged → resync.
        let gap_free = rows
            .iter()
            .zip(cursor + 1..)
            .all(|(r, expected)| r.conference_seq == Some(expected));
        if !gap_free {
            return Ok(None);
        }
        let last = rows.last().and_then(|r| r.conference_seq).unwrap_or(cursor);
        Ok(Some((
            rows.iter()
                .filter_map(|r| envelope(r).map(|e| (r, e)))
                .filter(|(r, e)| sub.may_receive_row(r, e))
                .map(|(_, e)| e)
                .collect(),
            last,
        )))
    }

    pub fn db(&self) -> &sea_orm::DatabaseConnection {
        &self.core.db
    }

    /// Load the registered actor behind a user ticket (fresh from PostgreSQL).
    pub async fn user_actor(&self, user_id: Uuid) -> ApiResult<ActorContext> {
        use crate::models::users::{self, AuthType, UserRole, UserStatus};
        let user = users::Entity::find_by_id(user_id)
            .one(&self.core.db)
            .await?
            .filter(|u| u.status != UserStatus::Inactive && u.role != UserRole::Guest)
            .ok_or_else(|| {
                ApiError::domain(ErrorCode::AuthenticationRequired, "Account is not active")
            })?;
        let verified_organizations = self
            .core
            .organizations
            .organizations_for_user(user.id)
            .await;
        // Same verification rule as `resolve_actor`.
        let email_verified = user.auth_type == AuthType::Google
            || (user.status == UserStatus::Active && self.require_email_verification);
        Ok(ActorContext {
            kind: ActorKind::RegisteredUser(crate::services::actor::RegisteredActor {
                user_id: user.id,
                display_name: user.username.clone(),
                verified_email: email_verified.then(|| user.email.to_ascii_lowercase()),
                platform_admin: user.role == UserRole::Admin,
                // The ticket was issued to an authenticated, revocation-checked request.
                revocation_checked: true,
            }),
            verified_organizations,
            request_id: None,
            client_kind: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketSubject {
    User(Uuid),
    Participant(Uuid),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scrub_removes_provider_and_secret_material() {
        let v = json!({
            "participantId": "p",
            "mediaIdentity": "ps_1",
            "nested": { "credential": "x", "ok": 1, "list": [{ "tokenHash": "h", "role": "HOST" }] },
            "rtcRoomName": "cf-1"
        });
        let s = scrub(&v);
        assert_eq!(
            s,
            json!({ "participantId": "p", "nested": { "ok": 1, "list": [{ "role": "HOST" }] } })
        );
    }

    #[test]
    fn provider_commands_never_become_envelopes() {
        let now = Utc::now();
        let mut row = outbox_events::Model {
            id: Uuid::new_v4(),
            aggregate_type: "conference".into(),
            aggregate_id: Uuid::new_v4(),
            event_type: event::RTC_ROOM_TERMINATE_REQUESTED.into(),
            payload: json!({}),
            meeting_space_id: None,
            conference_id: Some(Uuid::new_v4()),
            status: crate::domain::OutboxStatus::Pending,
            attempts: 0,
            available_at: now,
            published_at: None,
            last_error: None,
            request_id: None,
            created_at: now,
            channel: OutboxChannel::ProviderCommand,
            conference_seq: None,
            locked_by: None,
            audience: None,
        };
        assert!(envelope(&row).is_none());
        // Even if mislabelled, an rtc.* type is never exposed.
        row.channel = OutboxChannel::Realtime;
        assert!(envelope(&row).is_none());
        row.event_type = event::CONFERENCE_ENDED.into();
        row.conference_seq = Some(3);
        let env = envelope(&row).unwrap();
        assert_eq!(env.sequence, Some(3));
        assert_eq!(env.schema_version, SCHEMA_VERSION);
    }
}
