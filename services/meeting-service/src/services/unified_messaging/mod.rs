//! Part V H — Meeting Service ↔ Unified Messaging integration (§§97–101, §104).
//!
//! Meeting keeps what it owns — accounts, meeting admission, roles and policy — and
//! Messaging becomes the authority for main-room chat once the operator cuts over
//! ([`migration`]). Everything crosses the boundary through the signed
//! [`gateway::MessagingGateway`]; no Messaging table is ever read or written here.
//!
//! | piece | module |
//! |---|---|
//! | CF Meet account / guest → Platform Access principal (§104) | [`identity`] |
//! | identity bridge for clients: devices, client tokens, logout (§104) | [`identity`] |
//! | conference → MEETING conversation, membership and lifecycle sync (§98) | this module |
//! | migration state machine with gates and rollback (§100) | [`migration`] |
//! | shadow mirroring as LEGACY_MANAGED, parity, rollback copies (§§100–101) | [`mirror`] |
//! | compatibility adapter behind `MessageStore` (§99) | [`store`] |
//! | breakout rooms and private chats: their own conversations, membership, lifecycle | [`scoped`] |

pub mod config;
pub mod gateway;
pub mod identity;
pub mod migration;
pub mod mirror;
pub mod relay;
pub mod scoped;
pub mod store;

use std::sync::Arc;

use axum::http::Method;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, QueryResult, Statement, TransactionTrait,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, ConferenceStatus, Role},
    error::{ApiError, ApiResult, ErrorCode},
    models::conference_participants,
    services::ops_metrics::{inc, OPS_METRICS as OM},
};

pub use gateway::{Acting, GatewayError, MessagingGateway, Peer, PrincipalKind, SharedGateway};
pub use migration::ChatMigrationState;

/// Opaque `meetingRef` of a conference's conversation (no meeting title or account data).
pub fn meeting_ref(conference_id: Uuid) -> String {
    format!("conference:{conference_id}")
}

pub(crate) fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

pub(crate) async fn one<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> ApiResult<Option<QueryResult>> {
    Ok(db.query_one(stmt(sql, values)).await?)
}

pub(crate) async fn all<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> ApiResult<Vec<QueryResult>> {
    Ok(db.query_all(stmt(sql, values)).await?)
}

pub(crate) async fn exec<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> ApiResult<u64> {
    Ok(db.execute(stmt(sql, values)).await?.rows_affected())
}

/// Maps a peer failure to a Meeting error. Unavailability is a typed, retryable chat
/// error; it never fails the meeting itself.
pub fn chat_error(e: &GatewayError) -> ApiError {
    match e {
        GatewayError::Unavailable(_) => ApiError::domain(
            ErrorCode::ChatUnavailable,
            "Chat is temporarily unavailable; the meeting continues",
        ),
        GatewayError::Refused { code, reason, .. } => match code.as_str() {
            "RATE_LIMITED" | "MEMBER_SEND_RATE_LIMITED" => {
                ApiError::domain(ErrorCode::RateLimited, "Too many messages; slow down")
            }
            "NOT_FOUND" => ApiError::domain(ErrorCode::MessageNotFound, "Message not found"),
            "VALIDATION_ERROR" | "CONTENT_REJECTED" => {
                ApiError::domain(ErrorCode::ValidationError, "The message was not accepted")
            }
            "CONVERSATION_NOT_ACTIVE" => ApiError::domain(
                ErrorCode::ConferenceNotLive,
                "Chat of this conference is closed",
            ),
            "MEMBER_MUTED"
            | "PRINCIPAL_NOT_AUTHORIZED"
            | "PRINCIPAL_BANNED"
            | "INSUFFICIENT_SCOPE" => ApiError::domain(
                ErrorCode::PermissionDenied,
                "Chat is not available to you in this meeting",
            ),
            _ => {
                tracing::warn!(code = %code, reason = ?reason, "unified messaging refused a chat operation");
                ApiError::domain(
                    ErrorCode::ChatUnavailable,
                    "Chat is temporarily unavailable; the meeting continues",
                )
            }
        },
    }
}

/// Messaging role of a meeting role: meeting moderators administer the conversation.
pub fn conversation_role(role: Role) -> &'static str {
    match role {
        Role::Owner | Role::Host | Role::CoHost | Role::Moderator => "ADMIN",
        Role::Presenter | Role::Participant | Role::Viewer => "MEMBER",
    }
}

/// Membership a participant's admission state maps to (`None`: never a member).
pub fn conversation_state(state: AdmissionState) -> Option<&'static str> {
    match state {
        AdmissionState::Admitted => Some("ACTIVE"),
        AdmissionState::Removed => Some("REMOVED"),
        AdmissionState::Banned => Some("BANNED"),
        AdmissionState::Waiting | AdmissionState::Denied => None,
    }
}

#[derive(Debug, Clone)]
pub struct ConferenceLink {
    pub conference_id: Uuid,
    pub conversation_id: Uuid,
    pub owner_principal_id: Uuid,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub conversations_linked: u32,
    pub members_synced: u32,
    pub lifecycles_synced: u32,
    pub messages_mirrored: u32,
    pub deletions_mirrored: u32,
    pub rollback_copies: u32,
    pub revocations: u32,
    pub failures: u32,
}

/// The integration (present only when `FEATURE_UNIFIED_MESSAGING` is configured).
pub struct UnifiedMessaging {
    pub db: DatabaseConnection,
    pub gateway: SharedGateway,
}

pub type SharedUnifiedMessaging = Arc<UnifiedMessaging>;

impl UnifiedMessaging {
    pub fn new(db: DatabaseConnection, gateway: SharedGateway) -> Self {
        Self { db, gateway }
    }

    pub fn environment_id(&self) -> Uuid {
        self.gateway.environment_id()
    }

    pub async fn state(&self) -> ApiResult<ChatMigrationState> {
        Ok(migration::current(&self.db).await?.0)
    }

    // ----------------------------------------------------------------- links

    pub async fn link_of<C: ConnectionTrait>(
        &self,
        db: &C,
        conference_id: Uuid,
    ) -> ApiResult<Option<ConferenceLink>> {
        one(
            db,
            "SELECT conversation_id, owner_principal_id FROM conference_chat_links WHERE conference_id = $1",
            vec![conference_id.into()],
        )
        .await?
        .map(|r| -> ApiResult<ConferenceLink> {
            Ok(ConferenceLink {
                conference_id,
                conversation_id: r.try_get_by_index(0)?,
                owner_principal_id: r.try_get_by_index(1)?,
            })
        })
        .transpose()
    }

    /// Resolves (idempotently, in Messaging) and records the conference's MEETING
    /// conversation. The owner is the meeting space owner's own principal.
    pub async fn ensure_link<C: ConnectionTrait>(
        &self,
        db: &C,
        conference_id: Uuid,
    ) -> ApiResult<ConferenceLink> {
        if let Some(l) = self.link_of(db, conference_id).await? {
            return Ok(l);
        }
        let owner_user: Option<Uuid> = one(
            db,
            "SELECT s.owner_user_id FROM conference_sessions c JOIN meeting_spaces s ON s.id = c.meeting_space_id \
             WHERE c.id = $1",
            vec![conference_id.into()],
        )
        .await?
        .ok_or_else(|| ApiError::domain(ErrorCode::ConferenceNotFound, "Conference not found"))?
        .try_get_by_index(0)?;
        // An environment-owned meeting (Part VI §51) has no CF Meet owner principal to own
        // its Unified Messaging conversation: its chat is unavailable (fail closed).
        let owner_user = owner_user.ok_or_else(|| {
            ApiError::domain(
                ErrorCode::ChatUnavailable,
                "Meeting chat is not available for meetings owned by an application environment",
            )
        })?;
        let owner = self.principal_for_user(owner_user).await?;
        // Meeting chat is server-readable by design and disclosed as such in CF Meet:
        // the conversation is MANAGED (never relabelled as end-to-end encrypted).
        let body = json!({
            "meetingRef": meeting_ref(conference_id),
            "ownerPrincipalId": owner,
            "securityProfile": "MANAGED",
            "managedDisclosureAcknowledged": true,
        });
        let v = self
            .gateway
            .call(
                Peer::Messaging,
                None,
                Method::POST,
                "/internal/v1/meeting-conversations",
                &[],
                Some(&body),
            )
            .await
            .map_err(|e| chat_error(&e))?;
        let conversation_id = v["conversation"]["conversationId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| {
                ApiError::domain(ErrorCode::ChatUnavailable, "Unexpected Messaging answer")
            })?;
        exec(
            db,
            "INSERT INTO conference_chat_links (conference_id, conversation_id, environment_id, owner_principal_id) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (conference_id) DO NOTHING",
            vec![
                conference_id.into(),
                conversation_id.into(),
                self.environment_id().into(),
                owner.into(),
            ],
        )
        .await?;
        if v["created"] == json!(true) {
            inc(&OM.unified_messaging_conversations_linked);
        }
        self.link_of(db, conference_id)
            .await?
            .ok_or_else(|| ApiError::InternalError("conference chat link vanished".into()))
    }

    // ------------------------------------------------------------ membership

    /// Applies the participant's CURRENT admission state / role to the conversation.
    /// The source version is drawn after reading the state, under the link row lock, so
    /// Messaging's fence rejects any older, delayed update. Returns whether anything
    /// was sent.
    pub async fn sync_participant(
        &self,
        link: &ConferenceLink,
        participant_id: Uuid,
    ) -> ApiResult<bool> {
        let txn = self.db.begin().await?;
        exec(
            &txn,
            "SELECT 1 FROM conference_chat_links WHERE conference_id = $1 FOR UPDATE",
            vec![link.conference_id.into()],
        )
        .await?;
        use sea_orm::EntityTrait;
        let Some(p) = conference_participants::Entity::find_by_id(participant_id)
            .one(&txn)
            .await?
        else {
            return Ok(false);
        };
        let previous = one(
            &txn,
            "SELECT synced_state, synced_role, synced_version FROM conference_chat_member_sync WHERE participant_id = $1",
            vec![p.id.into()],
        )
        .await?;
        let prev_state: Option<String> = previous
            .as_ref()
            .map(|r| r.try_get_by_index(0))
            .transpose()?;
        let prev_role: Option<String> = previous
            .as_ref()
            .map(|r| r.try_get_by_index::<Option<String>>(1))
            .transpose()?
            .flatten();
        let target = match conversation_state(p.admission_state) {
            Some(s) => s,
            // Waiting / denied: only an earlier admission needs undoing.
            None if prev_state.as_deref() == Some("ACTIVE") => "REMOVED",
            None => return Ok(false),
        };
        let role = conversation_role(p.role);
        if prev_state.as_deref() == Some(target)
            && (target != "ACTIVE" || prev_role.as_deref() == Some(role))
        {
            return Ok(false);
        }
        let (principal, _) = self.principal_for_participant(&p).await?;
        if principal == link.owner_principal_id {
            // The space owner is the conversation OWNER; Messaging protects that row.
            exec(
                &txn,
                "INSERT INTO conference_chat_member_sync (participant_id, conference_id, principal_id, synced_version, synced_state, synced_role) \
                 VALUES ($1, $2, $3, 0, 'ACTIVE', 'OWNER') ON CONFLICT (participant_id) DO NOTHING",
                vec![p.id.into(), link.conference_id.into(), principal.into()],
            )
            .await?;
            txn.commit().await?;
            return Ok(false);
        }
        let version: i64 = one(
            &txn,
            "SELECT nextval('conference_chat_sync_version_seq')",
            vec![],
        )
        .await?
        .ok_or_else(|| ApiError::InternalError("sequence".into()))?
        .try_get_by_index(0)?;
        let mut body = json!({
            "state": target,
            "sourceVersion": version,
            "reason": match target {
                "ACTIVE" => "MEETING_ADMITTED",
                "BANNED" => "MEETING_BANNED",
                _ => "MEETING_REMOVED",
            },
        });
        if target == "ACTIVE" {
            body["role"] = json!(role);
        }
        let path = format!(
            "/internal/v1/meeting-conversations/{}/members/{}",
            link.conversation_id, principal
        );
        self.gateway
            .call(Peer::Messaging, None, Method::PUT, &path, &[], Some(&body))
            .await
            .map_err(|e| chat_error(&e))?;
        exec(
            &txn,
            "INSERT INTO conference_chat_member_sync (participant_id, conference_id, principal_id, synced_version, synced_state, synced_role, synced_at) \
             VALUES ($1, $2, $3, $4, $5, $6, now()) ON CONFLICT (participant_id) DO UPDATE SET \
               synced_version = EXCLUDED.synced_version, synced_state = EXCLUDED.synced_state, \
               synced_role = EXCLUDED.synced_role, synced_at = now()",
            vec![
                p.id.into(),
                link.conference_id.into(),
                principal.into(),
                version.into(),
                target.into(),
                (target == "ACTIVE").then_some(role).map(str::to_string).into(),
            ],
        )
        .await?;
        txn.commit().await?;
        inc(&OM.unified_messaging_members_synced);
        Ok(true)
    }

    /// Participants of linked conferences whose admission / role changed since the last
    /// applied sync.
    pub async fn sync_members(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT p.id, p.conference_id FROM conference_participants p \
               JOIN conference_chat_links l ON l.conference_id = p.conference_id \
               LEFT JOIN conference_chat_member_sync s ON s.participant_id = p.id \
             WHERE (s.participant_id IS NULL AND p.admission_state IN ('ADMITTED','REMOVED','BANNED')) \
                OR (s.participant_id IS NOT NULL AND s.synced_role <> 'OWNER' AND ( \
                     (p.admission_state = 'ADMITTED' AND (s.synced_state <> 'ACTIVE' OR s.synced_role IS DISTINCT FROM \
                        CASE WHEN p.role IN ('OWNER','HOST','CO_HOST','MODERATOR') THEN 'ADMIN' ELSE 'MEMBER' END)) \
                  OR (p.admission_state = 'REMOVED' AND s.synced_state <> 'REMOVED') \
                  OR (p.admission_state = 'BANNED' AND s.synced_state <> 'BANNED') \
                  OR (p.admission_state IN ('WAITING','DENIED') AND s.synced_state = 'ACTIVE'))) \
             ORDER BY p.updated_at LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut synced = 0;
        for r in rows {
            let pid: Uuid = r.try_get_by_index(0)?;
            let cid: Uuid = r.try_get_by_index(1)?;
            let Some(link) = self.link_of(&self.db, cid).await? else {
                continue;
            };
            match self.sync_participant(&link, pid).await {
                Ok(true) => synced += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "member sync deferred");
                    return Err(e);
                }
            }
        }
        Ok(synced)
    }

    /// Conferences that should have a conversation now: live-ish ones.
    pub async fn link_active_conferences(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT c.id FROM conference_sessions c LEFT JOIN conference_chat_links l ON l.conference_id = c.id \
             WHERE l.conference_id IS NULL AND c.status IN ('PREPARING','WAITING','LIVE') \
             ORDER BY c.created_at LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut n = 0;
        for r in rows {
            self.ensure_link(&self.db, r.try_get_by_index(0)?).await?;
            n += 1;
        }
        Ok(n)
    }

    /// Ended / cancelled / failed conferences close their conversation (once).
    pub async fn sync_lifecycles(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT l.conference_id, l.conversation_id, c.version, c.status FROM conference_chat_links l \
               JOIN conference_sessions c ON c.id = l.conference_id \
             WHERE c.status IN ('ENDED','CANCELLED','FAILED') AND l.lifecycle_synced_state IS DISTINCT FROM 'CLOSED' \
             LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut n = 0;
        for r in rows {
            let conference_id: Uuid = r.try_get_by_index(0)?;
            let conversation_id: Uuid = r.try_get_by_index(1)?;
            let version: i32 = r.try_get_by_index(2)?;
            let status: String = r.try_get_by_index(3)?;
            let body = json!({
                "lifecycleState": "CLOSED",
                "sourceVersion": version as i64,
                "reason": match ConferenceStatus::parse(&status) {
                    Some(ConferenceStatus::Cancelled) => "MEETING_CANCELLED",
                    Some(ConferenceStatus::Failed) => "MEETING_FAILED",
                    _ => "MEETING_ENDED",
                },
            });
            match self
                .gateway
                .call(
                    Peer::Messaging,
                    None,
                    Method::POST,
                    &format!("/internal/v1/meeting-conversations/{conversation_id}/lifecycle"),
                    &[],
                    Some(&body),
                )
                .await
            {
                // Applied, or already closed by a newer update: either way it is done.
                Ok(_) | Err(GatewayError::Refused { .. }) => {}
                Err(e) => return Err(chat_error(&e)),
            }
            exec(
                &self.db,
                "UPDATE conference_chat_links SET lifecycle_synced_state = 'CLOSED', lifecycle_synced_version = $2, updated_at = now() \
                 WHERE conference_id = $1",
                vec![conference_id.into(), (version as i64).into()],
            )
            .await?;
            n += 1;
        }
        Ok(n)
    }

    /// One pass of every background responsibility (also called directly by tests).
    /// Each step is idempotent and safe to run on several instances at once.
    pub async fn sync_once(&self) -> ApiResult<SyncReport> {
        let mut report = SyncReport::default();
        report.revocations = self.process_revocations(50).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "messaging session revocations deferred");
            report.failures += 1;
            0
        });
        let state = self.state().await?;
        if state == ChatMigrationState::LegacyPrimary {
            // Only rollback copies can still be owed after a rollback to LEGACY_PRIMARY.
            report.rollback_copies = self.copy_back_native(200).await.unwrap_or_else(|e| {
                tracing::warn!(error = %e, "rollback copy deferred");
                report.failures += 1;
                0
            });
            return Ok(report);
        }
        macro_rules! step {
            ($field:ident, $e:expr) => {
                match $e {
                    Ok(n) => report.$field += n,
                    Err(e) => {
                        report.failures += 1;
                        tracing::warn!(error = %e, step = stringify!($field), "unified messaging sync step deferred");
                    }
                }
            };
        }
        step!(conversations_linked, self.link_active_conferences(50).await);
        step!(members_synced, self.sync_members(200).await);
        step!(members_synced, self.sync_scoped_members(100).await);
        if state == ChatMigrationState::ShadowValidation {
            step!(messages_mirrored, self.mirror_pending(200).await);
            step!(messages_mirrored, self.mirror_scoped_pending(200).await);
            step!(deletions_mirrored, self.mirror_deletions(200).await);
            step!(rollback_copies, self.copy_back_native(200).await);
        } else {
            // After cutover only breakout / private rows written before this release can
            // still be unmirrored (the database refuses new ones): import them once.
            step!(messages_mirrored, self.mirror_scoped_pending(200).await);
        }
        step!(lifecycles_synced, self.sync_lifecycles(50).await);
        step!(lifecycles_synced, self.sync_scoped_lifecycles(50).await);
        Ok(report)
    }
}

pub(crate) fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

pub(crate) fn parse_time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}
