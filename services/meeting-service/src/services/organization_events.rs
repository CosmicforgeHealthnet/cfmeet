//! Organizations event ingress (final closure): `POST /internal/v1/organization-events`.
//!
//! The Organizations Service delivers its transactional-outbox events here at least once
//! (`meeting-organization-events.apply`, Organizations only; envelope in
//! `docs/events/organization-event-contracts.md`). Each event is applied once and in
//! order:
//!
//! * dedupe on `eventId` (`organization_event_inbox`; the same id with a different body is
//!   refused 409, which the producer dead-letters);
//! * monotonic per aggregate (`organization_event_fences`): a version at or below the last
//!   applied one is recorded as STALE and changes nothing;
//! * effects, in the same transaction as the inbox row and the fence:
//!   - organization lifecycle / profile / ownership / policy / scope events refresh the
//!     organization projection from the authoritative snapshot (shadow / authoritative
//!     mode; in legacy mode the Meeting Service is itself the authority and the event is
//!     only recorded). Organizations unreachable → 503, the producer retries;
//!   - membership events set the member's desired organization access
//!     (`organization_member_access`: ACTIVE / SUSPENDED / REMOVED, fenced by the
//!     membership version). The offboarding worker
//!     ([`crate::services::organization_access`]) applies it to the member's identities in
//!     the organization's own technical scope; personal-scope identities are never
//!     touched.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `ORGANIZATION_EVENTS_ENABLED` | `true` mounts the route (default off) |
//! | `CF_RUNTIME_ENVIRONMENT` | runtime environment of the trust bundle |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | receiver trust bundle (`cfmeet.meeting-service`) |

use std::{path::PathBuf, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use cf_service_auth::{
    services, InternalPermission, RuntimeEnvironment, ServiceId, ServiceTokenVerifier, SystemClock,
    TrustPolicy,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{config::env_bool, state::AppState};

pub const EVENTS_PATH: &str = "/internal/v1/organization-events";
/// Envelopes are ids, roles and statuses only; anything larger is not an event.
const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct OrganizationEventsConfig {
    pub runtime: RuntimeEnvironment,
    pub trust_bundle_file: PathBuf,
}

impl OrganizationEventsConfig {
    /// `Ok(None)`: off. `Err`: on but misconfigured (startup fails closed).
    pub fn from_env() -> Result<Option<Self>, String> {
        if !env_bool("ORGANIZATION_EVENTS_ENABLED", false) {
            return Ok(None);
        }
        let var = |name: &str| match std::env::var(name) {
            Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
            _ => Err(format!(
                "{name} is required when ORGANIZATION_EVENTS_ENABLED is on"
            )),
        };
        let runtime = RuntimeEnvironment::parse(&var("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT must be local, test, staging or production")?;
        Ok(Some(Self {
            runtime,
            trust_bundle_file: PathBuf::from(var("CF_SERVICE_TRUST_BUNDLE_FILE")?),
        }))
    }

    pub fn verifier(&self) -> Result<Arc<ServiceTokenVerifier>, String> {
        let policy = TrustPolicy::from_file(
            &self.trust_bundle_file,
            &ServiceId::meeting_service(),
            self.runtime,
        )
        .map_err(|e| format!("CF_SERVICE_TRUST_BUNDLE_FILE: {e}"))?;
        Ok(Arc::new(ServiceTokenVerifier::new(
            policy,
            Arc::new(SystemClock),
        )))
    }
}

#[derive(Clone)]
struct EventsState {
    app: AppState,
    verifier: Arc<ServiceTokenVerifier>,
}

pub fn router(app: AppState, verifier: Arc<ServiceTokenVerifier>) -> Router {
    Router::new()
        .route(EVENTS_PATH, post(ingest))
        .with_state(EventsState { app, verifier })
}

fn reply(status: StatusCode, body: Value) -> Response {
    let mut r = (status, Json(body)).into_response();
    r.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    r
}

fn error(status: StatusCode, code: &str, message: &str, request_id: Option<&str>) -> Response {
    reply(
        status,
        json!({ "error": { "code": code, "message": message, "requestId": request_id } }),
    )
}

/// The envelope (schemaVersion 1). Additive fields are tolerated.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub event_id: Uuid,
    pub event_type: String,
    pub schema_version: i64,
    pub organization_id: Uuid,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub aggregate_version: i64,
    #[serde(default)]
    pub payload: Value,
}

impl Envelope {
    fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("schemaVersion must be 1");
        }
        if self.aggregate_version < 1 {
            return Err("aggregateVersion must be at least 1");
        }
        let well_formed = |s: &str, max: usize| {
            !s.is_empty()
                && s.len() <= max
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c == '.')
        };
        if !well_formed(&self.event_type, 64) || !self.event_type.contains('.') {
            return Err("eventType is not an event type");
        }
        if !well_formed(&self.aggregate_type, 32) {
            return Err("aggregateType is not an aggregate type");
        }
        Ok(())
    }
}

/// What an event does to the Meeting Service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Refresh the organization projection from the authoritative snapshot.
    RefreshProjection,
    /// Set the member's desired organization access.
    MemberAccess {
        user_id: Uuid,
        desired: &'static str,
    },
    /// Recorded only (no local effect).
    Record,
}

/// The effect of `envelope` (pure; unknown event types are recorded, never refused, so an
/// additive producer change never dead-letters).
pub fn effect_of(envelope: &Envelope) -> Result<Effect, &'static str> {
    let user = || {
        envelope
            .payload
            .get("userId")
            .and_then(Value::as_str)
            .and_then(|u| Uuid::try_parse(u).ok())
            .ok_or("payload.userId is required for membership events")
    };
    Ok(match envelope.event_type.as_str() {
        "membership.activated" | "membership.role_changed" => Effect::MemberAccess {
            user_id: user()?,
            desired: "ACTIVE",
        },
        "membership.suspended" => Effect::MemberAccess {
            user_id: user()?,
            desired: "SUSPENDED",
        },
        "membership.removed" => Effect::MemberAccess {
            user_id: user()?,
            desired: "REMOVED",
        },
        "organization.created"
        | "organization.profile_updated"
        | "organization.provisioned"
        | "organization.suspended"
        | "organization.reactivated"
        | "organization.archived"
        | "ownership.transferred"
        | "policy.updated"
        | "scope.mapping_changed" => Effect::RefreshProjection,
        _ => Effect::Record,
    })
}

/// Canonical digest of an envelope (serde_json maps are ordered).
pub fn digest(body: &Value) -> String {
    hex::encode(Sha256::digest(serde_json::to_vec(body).unwrap_or_default()))
}

async fn ingest(State(s): State<EventsState>, request: Request) -> Response {
    let ctx = match s.verifier.verify_headers(request.headers()) {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::warn!(
                reason = e.failure.as_str(),
                "organization events: service authentication rejected"
            );
            return error(
                StatusCode::UNAUTHORIZED,
                "INTERNAL_AUTHENTICATION_FAILED",
                "Service authentication failed.",
                None,
            );
        }
    };
    let rid = ctx.request_id().as_str().to_string();
    if ctx
        .require_permission(InternalPermission::MeetingOrganizationEventsApply)
        .is_err()
        || ctx.authenticated_service().as_str() != services::ORGANIZATIONS
    {
        return error(
            StatusCode::FORBIDDEN,
            "INTERNAL_PERMISSION_DENIED",
            "Only the Organizations Service delivers organization events.",
            Some(&rid),
        );
    }
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                "The event is too large.",
                Some(&rid),
            )
        }
    };
    let invalid = |m: &str| error(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", m, Some(&rid));
    let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
        return invalid("The body is not JSON.");
    };
    let Ok(envelope) = serde_json::from_value::<Envelope>(body.clone()) else {
        return invalid("The body is not an organization event envelope.");
    };
    if let Err(m) = envelope.validate() {
        return invalid(m);
    }
    let effect = match effect_of(&envelope) {
        Ok(e) => e,
        Err(m) => return invalid(m),
    };
    match apply(&s.app, &envelope, &effect, &digest(&body)).await {
        Ok(outcome) => {
            tracing::info!(
                event_id = %envelope.event_id,
                event_type = %envelope.event_type,
                organization_id = %envelope.organization_id,
                outcome = outcome.as_str(),
                "organization event received"
            );
            reply(StatusCode::OK, outcome.body())
        }
        Err(ApplyError::Conflict) => error(
            StatusCode::CONFLICT,
            "EVENT_IDENTITY_CONFLICT",
            "A different event was already received under this id.",
            Some(&rid),
        ),
        Err(ApplyError::Unavailable(reason)) => {
            tracing::warn!(event_id = %envelope.event_id, reason, "organization event deferred");
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                "The event cannot be applied now; deliver it again later.",
                Some(&rid),
            )
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    Recorded,
    Stale,
    Duplicate,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Applied => "APPLIED",
            Self::Recorded => "RECORDED",
            Self::Stale => "STALE",
            Self::Duplicate => "DUPLICATE",
        }
    }

    fn body(&self) -> Value {
        json!({ "outcome": self.as_str(), "duplicate": *self == Self::Duplicate })
    }
}

#[derive(Debug)]
pub enum ApplyError {
    Conflict,
    Unavailable(String),
}

impl From<sea_orm::DbErr> for ApplyError {
    fn from(e: sea_orm::DbErr) -> Self {
        Self::Unavailable(format!("database: {e}"))
    }
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Applies one envelope once (inbox, fence and effect in one transaction).
pub async fn apply(
    app: &AppState,
    envelope: &Envelope,
    effect: &Effect,
    request_digest: &str,
) -> Result<Outcome, ApplyError> {
    let txn = app.db.begin().await?;
    // Dedupe: a known id with the same body is a duplicate; a different body conflicts.
    if let Some(row) = txn
        .query_one(stmt(
            "SELECT request_digest FROM organization_event_inbox WHERE event_id = $1",
            vec![envelope.event_id.into()],
        ))
        .await?
    {
        let known: String = row.try_get_by_index(0)?;
        return if known.trim() == request_digest {
            Ok(Outcome::Duplicate)
        } else {
            Err(ApplyError::Conflict)
        };
    }
    // Monotonic per aggregate: the row lock serializes deliveries of one aggregate; a
    // concurrent duplicate of the same event loses on the inbox primary key (503, retried,
    // then answered DUPLICATE).
    let fence: Option<i64> = txn
        .query_one(stmt(
            "SELECT aggregate_version FROM organization_event_fences WHERE aggregate_id = $1 FOR UPDATE",
            vec![envelope.aggregate_id.into()],
        ))
        .await?
        .map(|r| r.try_get_by_index(0))
        .transpose()?;
    let stale = fence.is_some_and(|v| envelope.aggregate_version <= v);
    let outcome = if stale {
        Outcome::Stale
    } else {
        apply_effect(app, &txn, envelope, effect).await?
    };
    txn.execute(stmt(
        "INSERT INTO organization_event_inbox (event_id, event_type, organization_id, aggregate_type, aggregate_id, aggregate_version, request_digest, outcome) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            envelope.event_id.into(),
            envelope.event_type.clone().into(),
            envelope.organization_id.into(),
            envelope.aggregate_type.clone().into(),
            envelope.aggregate_id.into(),
            envelope.aggregate_version.into(),
            request_digest.to_string().into(),
            outcome.as_str().into(),
        ],
    ))
    .await?;
    if !stale {
        txn.execute(stmt(
            "INSERT INTO organization_event_fences (aggregate_id, aggregate_type, aggregate_version) VALUES ($1, $2, $3) \
             ON CONFLICT (aggregate_id) DO UPDATE SET aggregate_version = GREATEST(organization_event_fences.aggregate_version, EXCLUDED.aggregate_version), updated_at = now()",
            vec![
                envelope.aggregate_id.into(),
                envelope.aggregate_type.clone().into(),
                envelope.aggregate_version.into(),
            ],
        ))
        .await?;
    }
    txn.commit().await?;
    Ok(outcome)
}

async fn apply_effect<C: ConnectionTrait>(
    app: &AppState,
    txn: &C,
    envelope: &Envelope,
    effect: &Effect,
) -> Result<Outcome, ApplyError> {
    match effect {
        Effect::Record => Ok(Outcome::Recorded),
        Effect::RefreshProjection => {
            let Some(authority) = app
                .meeting
                .core
                .organization_authority
                .as_ref()
                .filter(|a| a.mode() != super::organizations_authority::AuthorityMode::Legacy)
            else {
                // Legacy mode: the Meeting Service owns organizations itself.
                return Ok(Outcome::Recorded);
            };
            match authority
                .ensure_projection(txn, envelope.organization_id)
                .await
            {
                Ok(_) => Ok(Outcome::Applied),
                // Unknown to Organizations (deleted before delivery): nothing to project.
                Err(e) if e.code() == Some(crate::error::ErrorCode::OrganizationNotFound) => {
                    Ok(Outcome::Recorded)
                }
                Err(e) => Err(ApplyError::Unavailable(format!("projection refresh: {e}"))),
            }
        }
        Effect::MemberAccess { user_id, desired } => {
            txn.execute(stmt(
                "INSERT INTO organization_member_access (organization_id, user_id, desired_state, membership_version, next_attempt_at, attempts, updated_at) \
                 VALUES ($1, $2, $3, $4, now(), 0, now()) \
                 ON CONFLICT (organization_id, user_id) DO UPDATE SET \
                   desired_state = EXCLUDED.desired_state, membership_version = EXCLUDED.membership_version, \
                   next_attempt_at = now(), attempts = 0, last_error_class = NULL, updated_at = now() \
                 WHERE organization_member_access.membership_version < EXCLUDED.membership_version",
                vec![
                    envelope.organization_id.into(),
                    (*user_id).into(),
                    (*desired).into(),
                    envelope.aggregate_version.into(),
                ],
            ))
            .await?;
            Ok(Outcome::Applied)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(event_type: &str, payload: Value) -> Envelope {
        Envelope {
            event_id: Uuid::new_v4(),
            event_type: event_type.into(),
            schema_version: 1,
            organization_id: Uuid::new_v4(),
            aggregate_type: "membership".into(),
            aggregate_id: Uuid::new_v4(),
            aggregate_version: 1,
            payload,
        }
    }

    #[test]
    fn membership_events_set_the_desired_access_and_need_the_user() {
        let u = Uuid::new_v4();
        for (t, desired) in [
            ("membership.activated", "ACTIVE"),
            ("membership.role_changed", "ACTIVE"),
            ("membership.suspended", "SUSPENDED"),
            ("membership.removed", "REMOVED"),
        ] {
            assert_eq!(
                effect_of(&envelope(t, json!({ "userId": u }))).unwrap(),
                Effect::MemberAccess {
                    user_id: u,
                    desired
                },
                "{t}"
            );
            assert!(effect_of(&envelope(t, json!({}))).is_err(), "{t}");
        }
    }

    #[test]
    fn lifecycle_events_refresh_the_projection_and_unknown_ones_are_recorded() {
        for t in [
            "organization.suspended",
            "organization.reactivated",
            "policy.updated",
        ] {
            assert_eq!(
                effect_of(&envelope(t, json!({}))).unwrap(),
                Effect::RefreshProjection
            );
        }
        assert_eq!(
            effect_of(&envelope("team.created", json!({}))).unwrap(),
            Effect::Record
        );
    }

    #[test]
    fn malformed_envelopes_are_refused() {
        let mut e = envelope("membership.removed", json!({}));
        e.schema_version = 2;
        assert!(e.validate().is_err());
        let mut e = envelope("membership.removed", json!({}));
        e.aggregate_version = 0;
        assert!(e.validate().is_err());
        assert!(envelope("Membership.Removed", json!({}))
            .validate()
            .is_err());
        assert!(envelope("membership", json!({})).validate().is_err());
    }

    #[test]
    fn the_digest_is_independent_of_key_order() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":3}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":3,"y":2},"b":1}"#).unwrap();
        assert_eq!(digest(&a), digest(&b));
    }
}
