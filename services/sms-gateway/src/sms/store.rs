//! Transport persistence: loading (scope-checked), locked transitions with optimistic
//! versions, inserts, and the public message view.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr};
use serde::Serialize;
use uuid::Uuid;

use super::{
    protection::{Protection, Purpose},
    threads::{ts, RemoteView},
    SmsError, SmsResult, Status, TransportRow, TRANSPORT_COLUMNS,
};
use crate::db;

/// A transport of `environment` (another environment's message is NOT_FOUND).
pub async fn load<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    id: Uuid,
) -> Result<Option<TransportRow>, DbErr> {
    db::one::<TransportRow, _>(
        db,
        &format!(
            "SELECT {TRANSPORT_COLUMNS} FROM sms_transports WHERE id = $1 AND environment_id = $2"
        ),
        vec![id.into(), environment_id.into()],
    )
    .await
}

/// A transport by id (internal workers only).
pub async fn load_any<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<TransportRow>, DbErr> {
    db::one::<TransportRow, _>(
        db,
        &format!("SELECT {TRANSPORT_COLUMNS} FROM sms_transports WHERE id = $1"),
        vec![id.into()],
    )
    .await
}

/// Locks a transport for a transition (inside the caller's transaction).
pub async fn lock<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<TransportRow>, DbErr> {
    db::one::<TransportRow, _>(
        db,
        &format!("SELECT {TRANSPORT_COLUMNS} FROM sms_transports WHERE id = $1 FOR UPDATE"),
        vec![id.into()],
    )
    .await
}

/// Applies a transition (`next`, if any) and column changes to `row`, guarded by its
/// version; refuses a transition the state machine does not allow.
pub async fn update_in<C: ConnectionTrait>(
    db: &C,
    row: &TransportRow,
    next: Option<Status>,
    sets: Vec<(&'static str, sea_orm::Value)>,
    now: DateTime<Utc>,
) -> SmsResult<TransportRow> {
    if let Some(next) = next {
        if next != row.status() && !row.status().may_become(next) {
            return Err(SmsError::Conflict("INVALID_TRANSITION"));
        }
    }
    let mut values: Vec<sea_orm::Value> = vec![row.id.into(), row.version.into(), now.into()];
    let mut assignments = vec![
        "version = version + 1".to_string(),
        "updated_at = GREATEST(updated_at, $3)".to_string(),
    ];
    if let Some(next) = next {
        values.push(next.as_str().into());
        assignments.push(format!("status = ${}", values.len()));
    }
    for (column, value) in sets {
        values.push(value);
        assignments.push(format!("{column} = ${}", values.len()));
    }
    db::one::<TransportRow, _>(
        db,
        &format!(
            "UPDATE sms_transports SET {} WHERE id = $1 AND version = $2 RETURNING {TRANSPORT_COLUMNS}",
            assignments.join(", ")
        ),
        values,
    )
    .await?
    .ok_or(SmsError::Conflict("VERSION_CONFLICT"))
}

/// Everything a new transport row holds (sealed values already sealed).
pub struct NewTransport<'a> {
    pub id: Uuid,
    pub scope: super::events::EventScope,
    pub direction: super::Direction,
    pub status: Status,
    pub number_resource_id: Uuid,
    pub assignment_id: Option<Uuid>,
    pub routing_revision: Option<i64>,
    pub principal_id: Option<Uuid>,
    pub target: Option<(&'a str, Uuid)>,
    pub remote_sealed: String,
    pub remote_fingerprint: &'a str,
    pub remote_masked: &'a str,
    pub content_sealed: String,
    pub content_fingerprint: String,
    pub encoding: super::segments::Encoding,
    pub characters: usize,
    pub segments: usize,
    pub thread_id: Uuid,
    pub service_account_id: Option<Uuid>,
    pub credential_id: Option<Uuid>,
    pub idempotency: Option<(&'a str, &'a str)>,
    pub inbound_grant_id: Option<Uuid>,
    pub provider_id: Option<&'a str>,
    pub max_attempts: i16,
    pub expires_at: Option<DateTime<Utc>>,
    pub received_at: Option<DateTime<Utc>>,
    pub correlation_id: &'a str,
    /// Outbound: the trusted environment kind (inbound: None).
    pub environment_kind: Option<crate::identity::EnvironmentKind>,
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    t: NewTransport<'_>,
    now: DateTime<Utc>,
) -> Result<TransportRow, DbErr> {
    db::one::<TransportRow, _>(
        db,
        &format!(
            "INSERT INTO sms_transports (id, tenant_id, application_id, environment_id, direction, status, \
               number_resource_id, assignment_id, routing_revision, principal_id, target_kind, target_ref, \
               remote_sealed, remote_fingerprint, remote_masked, content_sealed, content_fingerprint, \
               content_encoding, char_count, segment_count, thread_id, service_account_id, credential_id, \
               idempotency_key, request_digest, inbound_grant_id, provider_id, max_attempts, expires_at, \
               received_at, correlation_id, created_at, updated_at, environment_kind) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, \
               $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, $32, $33) \
             RETURNING {TRANSPORT_COLUMNS}"
        ),
        vec![
            t.id.into(),
            t.scope.tenant_id.into(),
            t.scope.application_id.into(),
            t.scope.environment_id.into(),
            t.direction.as_str().into(),
            t.status.as_str().into(),
            t.number_resource_id.into(),
            t.assignment_id.into(),
            t.routing_revision.into(),
            t.principal_id.into(),
            t.target.map(|(k, _)| k.to_string()).into(),
            t.target.map(|(_, r)| r).into(),
            t.remote_sealed.into(),
            t.remote_fingerprint.into(),
            t.remote_masked.into(),
            t.content_sealed.into(),
            t.content_fingerprint.into(),
            t.encoding.as_str().into(),
            (t.characters as i32).into(),
            (t.segments as i16).into(),
            t.thread_id.into(),
            t.service_account_id.into(),
            t.credential_id.into(),
            t.idempotency.map(|(k, _)| k.to_string()).into(),
            t.idempotency.map(|(_, d)| d.to_string()).into(),
            t.inbound_grant_id.into(),
            t.provider_id.map(str::to_string).into(),
            t.max_attempts.into(),
            t.expires_at.into(),
            t.received_at.into(),
            t.correlation_id.into(),
            now.into(),
            t.environment_kind.map(|k| k.as_str().to_string()).into(),
        ],
    )
    .await?
    .ok_or_else(|| DbErr::RecordNotFound("transport".into()))
}

/// The public view of a message. Content is opened for the owning customer only (the
/// caller already scope-checked the row); the remote party is masked.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub message_id: Uuid,
    pub direction: String,
    pub status: String,
    pub number_resource_id: Uuid,
    pub thread_id: Uuid,
    pub remote: RemoteView,
    pub content: Option<String>,
    pub content_purged: bool,
    pub encoding: String,
    pub segment_count: i16,
    pub character_count: i32,
    pub principal_id: Option<Uuid>,
    pub routing: Option<RoutingView>,
    pub failure_code: Option<String>,
    pub blocked_reason: Option<String>,
    pub cost: Option<CostView>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub submitted_at: Option<String>,
    pub delivered_at: Option<String>,
    pub received_at: Option<String>,
    pub failed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingView {
    pub target_kind: String,
    pub target_ref: Uuid,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostView {
    pub estimated_minor: Option<i64>,
    pub confirmed_minor: Option<i64>,
    pub currency: String,
}

pub fn view(row: &TransportRow, protection: &Protection) -> MessageView {
    let content = row.content_sealed.as_deref().and_then(|sealed| {
        protection
            .open(Purpose::Content, row.id, sealed)
            .map_err(|_| tracing::error!(message_id = %row.id, "sealed content does not open"))
            .ok()
    });
    MessageView {
        message_id: row.id,
        direction: row.direction.clone(),
        status: row.status.clone(),
        number_resource_id: row.number_resource_id,
        thread_id: row.thread_id,
        remote: RemoteView {
            masked: row.remote_masked.clone(),
        },
        content,
        content_purged: row.content_purged_at.is_some(),
        encoding: row.content_encoding.clone(),
        segment_count: row.segment_count,
        character_count: row.char_count,
        principal_id: row.principal_id,
        routing: match (&row.target_kind, row.target_ref) {
            (Some(k), Some(r)) => Some(RoutingView {
                target_kind: k.clone(),
                target_ref: r,
            }),
            _ => None,
        },
        failure_code: row.failure_code.clone(),
        blocked_reason: row.blocked_reason.clone(),
        cost: row.cost_currency.as_ref().map(|c| CostView {
            estimated_minor: row.estimated_cost_minor,
            confirmed_minor: row.confirmed_cost_minor,
            currency: c.clone(),
        }),
        created_at: ts(row.created_at),
        expires_at: row.expires_at.map(ts),
        submitted_at: row.submitted_at.map(ts),
        delivered_at: row.delivered_at.map(ts),
        received_at: row.received_at.map(ts),
        failed_at: row.failed_at.map(ts),
    }
}
