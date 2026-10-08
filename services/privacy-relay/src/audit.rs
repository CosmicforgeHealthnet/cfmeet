//! Immutable audit evidence and the transactional outbox.
//!
//! Every authority change appends one `relay_audit_events` row (gap-free per context —
//! callers hold the context row lock) and its `privacy_relay_outbox` row in the canonical
//! platform event envelope, in the SAME transaction as the state change. Event data is
//! bounded and safe: ids, versions, statuses, changed authority — never contacts,
//! provider identifiers, credentials or external business references.

use cf_service_auth::RuntimeEnvironment;
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::DatabaseTransaction;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{
    config::privacy_relay_service_id, db, error::RelayResult, scope::CommandContext,
    vocabulary::AuditEventType,
};

/// Payload schema version (contract `cfmeet.privacy-relay.event`).
pub const EVENT_SCHEMA_VERSION: i32 = 1;

/// One audit event to append.
#[derive(Debug, Clone)]
pub struct AuditDraft {
    pub event_type: AuditEventType,
    pub context_id: Uuid,
    pub context_version: i64,
    pub context_status: &'static str,
    /// (session id, session version) the event is about.
    pub session: Option<(Uuid, i64)>,
    /// The session's status after the change, for session lifecycle events.
    pub session_status: Option<&'static str>,
    pub enforcement_job_id: Option<Uuid>,
    pub data: Map<String, Value>,
}

/// Appends the audit event and (for authority changes) its outbox envelope.
pub async fn append(
    txn: &DatabaseTransaction,
    cmd: &CommandContext,
    runtime: RuntimeEnvironment,
    draft: AuditDraft,
    at: DateTime<Utc>,
) -> RelayResult<Uuid> {
    let scope = cmd.scope();
    let sequence = db::scalar_i64(
        txn,
        "SELECT COALESCE(MAX(sequence), 0)::bigint FROM relay_audit_events WHERE context_id = $1",
        vec![draft.context_id.into()],
    )
    .await?
        + 1;
    let id = Uuid::new_v4();
    let data = Value::Object(draft.data.clone());
    db::exec(
        txn,
        "INSERT INTO relay_audit_events (id, tenant_id, application_id, environment_id, context_id, \
           session_id, sequence, event_type, context_version, session_version, actor_kind, \
           actor_service_account_id, actor_service, enforcement_job_id, request_id, correlation_id, data, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
        vec![
            id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            draft.context_id.into(),
            draft.session.map(|s| s.0).into(),
            sequence.into(),
            draft.event_type.as_str().into(),
            draft.context_version.into(),
            draft.session.map(|s| s.1).into(),
            cmd.actor_kind().as_str().into(),
            cmd.service_account_id().into(),
            Some(cmd.actor_service().as_str().to_string()).into(),
            draft.enforcement_job_id.into(),
            cmd.request_id().as_str().into(),
            cmd.correlation_id().as_str().into(),
            data.clone().into(),
            at.into(),
        ],
    )
    .await?;

    let event_id = format!("evt_{}", Uuid::new_v4().simple());
    let (aggregate_type, aggregate_id) = match draft.session {
        Some((session_id, _)) if draft.event_type.is_session_event() => {
            ("RELAY_SESSION", session_id)
        }
        _ => ("COMMUNICATION_CONTEXT", draft.context_id),
    };
    let mut payload = json!({
        "audit_event_id": id,
        "event_type": draft.event_type.as_str(),
        "sequence": sequence,
        "context_id": draft.context_id,
        "context_version": draft.context_version,
        "context_status": draft.context_status,
        "occurred_at": at.to_rfc3339_opts(SecondsFormat::Micros, true),
        "correlation_id": cmd.correlation_id().as_str(),
        "actor_kind": cmd.actor_kind().as_str(),
        "data": data,
    });
    if let Some((session_id, session_version)) = draft.session {
        payload["session_id"] = json!(session_id);
        payload["session_version"] = json!(session_version);
    }
    if let Some(status) = draft.session_status {
        payload["session_status"] = json!(status);
    }
    if let Some(job) = draft.enforcement_job_id {
        payload["enforcement_job_id"] = json!(job);
    }
    let envelope = json!({
        "event_id": event_id,
        "event_type": draft.event_type.envelope_type(),
        "schema_version": EVENT_SCHEMA_VERSION,
        "occurred_at": at.to_rfc3339_opts(SecondsFormat::Micros, true),
        "producer_service": privacy_relay_service_id().as_str(),
        "runtime_environment": runtime.as_str(),
        "correlation_id": cmd.correlation_id().as_str(),
        "ownership": {
            "tenant_id": scope.tenant_id(),
            "application_id": scope.application_id(),
            "application_environment_id": scope.environment_id(),
        },
        "payload": payload,
    });
    db::exec(
        txn,
        "INSERT INTO privacy_relay_outbox (id, event_id, source_audit_event_id, aggregate_type, aggregate_id, \
           tenant_id, application_id, environment_id, event_type, schema_version, envelope, status, \
           attempt_count, next_attempt_at, published_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'PENDING', 0, $12, NULL, $12, $12)",
        vec![
            Uuid::new_v4().into(),
            event_id.into(),
            id.into(),
            aggregate_type.into(),
            aggregate_id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            draft.event_type.envelope_type().into(),
            EVENT_SCHEMA_VERSION.into(),
            envelope.into(),
            at.into(),
        ],
    )
    .await?;
    Ok(id)
}

/// Small builder for bounded event data.
pub fn data(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}
