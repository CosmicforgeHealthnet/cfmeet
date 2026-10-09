//! Durable administrative audit evidence (Part F §21, §155).
//!
//! Every administrative change appends one `enterprise_admin_audit` row in the SAME
//! transaction as the change: who (service account / credential from the authenticated
//! context), the scope, the resource type and id, the operation, old / new version, the
//! request / correlation ids, the time and a bounded detail object of safe values only
//! (ids, enumerations, counts, booleans — never a secret, a telephone number, a DTMF
//! digit or an object key). The table is append-only (trigger).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{
    exec, scope_values, stmt, time, AdminActor, EnterpriseResult, EnterpriseService, PageRequest,
};
use crate::calls::command::CallScope;

text_enum!(
    /// Audited enterprise resource kinds.
    AuditResource {
        CallPolicy = "CALL_POLICY",
        Department = "DEPARTMENT",
        Extension = "EXTENSION",
        RingGroup = "RING_GROUP",
        Queue = "QUEUE",
        IvrFlow = "IVR_FLOW",
        BusinessHours = "BUSINESS_HOURS",
        ForwardingRule = "FORWARDING_RULE",
        VoicemailMailbox = "VOICEMAIL_MAILBOX",
        VoicemailMessage = "VOICEMAIL_MESSAGE",
        RecordingPolicy = "RECORDING_POLICY",
        Recording = "RECORDING",
        SipDevice = "SIP_DEVICE",
        PublicContact = "PUBLIC_CONTACT",
        WebhookSubscription = "WEBHOOK_SUBSCRIPTION",
    }
);

text_enum!(
    /// Audited operations.
    AuditOperation {
        Create = "CREATE",
        Update = "UPDATE",
        Disable = "DISABLE",
        ReplaceMembers = "REPLACE_MEMBERS",
        PublishVersion = "PUBLISH_VERSION",
        Activate = "ACTIVATE",
        ReplaceSchedule = "REPLACE_SCHEDULE",
        MarkRead = "MARK_READ",
        MarkUnread = "MARK_UNREAD",
        Delete = "DELETE",
        RetentionDelete = "RETENTION_DELETE",
        MediaAccess = "MEDIA_ACCESS",
        RotateSecret = "ROTATE_SECRET",
        Replay = "REPLAY",
    }
);

/// A bounded detail object of safe values.
#[derive(Debug, Default, Clone)]
pub struct AuditDetails(Map<String, Value>);

impl AuditDetails {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.0.insert(key.to_string(), value.into());
        self
    }

    pub(crate) fn into_value(self) -> Value {
        Value::Object(self.0)
    }
}

/// One audit fact.
pub(crate) struct AuditFact {
    pub resource: AuditResource,
    pub resource_id: Uuid,
    pub operation: AuditOperation,
    pub old_version: Option<i64>,
    pub new_version: Option<i64>,
    pub details: AuditDetails,
}

/// Appends one audit row inside the caller's transaction.
pub(crate) async fn record<C: ConnectionTrait>(
    db: &C,
    actor: &AdminActor,
    fact: AuditFact,
    at: DateTime<Utc>,
) -> EnterpriseResult<()> {
    let mut values = vec![Uuid::new_v4().into()];
    values.extend(scope_values(actor.scope));
    values.extend([
        actor.kind.as_str().into(),
        actor.service_account_id.map(|s| s.as_uuid()).into(),
        actor.credential_id.map(|c| c.as_uuid()).into(),
        fact.resource.as_str().into(),
        fact.resource_id.into(),
        fact.operation.as_str().into(),
        fact.old_version.into(),
        fact.new_version.into(),
        bounded(&actor.request_id).into(),
        bounded(&actor.correlation_id).into(),
        fact.details.into_value().into(),
        at.into(),
    ]);
    exec(
        db,
        "INSERT INTO enterprise_admin_audit (id, tenant_id, application_id, environment_id, \
         actor_kind, actor_service_account_id, actor_credential_id, resource_type, resource_id, \
         operation, old_version, new_version, request_id, correlation_id, details, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        values,
    )
    .await?;
    Ok(())
}

fn encode_seq(seq: i64) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(format!("audit|{seq}"))
}

fn decode_seq(cursor: &str) -> EnterpriseResult<i64> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|t| t.strip_prefix("audit|").and_then(|n| n.parse::<i64>().ok()))
        .filter(|n| *n > 0)
        .ok_or(super::EnterpriseError::Validation("cursor is invalid."))
}

fn bounded(id: &str) -> String {
    id.chars().take(128).collect()
}

#[derive(Debug, Clone, FromQueryResult)]
struct AuditRow {
    id: Uuid,
    seq: i64,
    actor_kind: String,
    actor_service_account_id: Option<Uuid>,
    actor_credential_id: Option<Uuid>,
    resource_type: String,
    resource_id: Uuid,
    operation: String,
    old_version: Option<i64>,
    new_version: Option<i64>,
    request_id: String,
    correlation_id: String,
    details: Value,
    occurred_at: DateTime<Utc>,
}

/// Safe audit record view.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecordView {
    pub audit_id: Uuid,
    pub actor_kind: String,
    pub actor_service_account_id: Option<Uuid>,
    pub actor_credential_id: Option<Uuid>,
    pub resource_type: String,
    pub resource_id: Uuid,
    pub operation: String,
    pub old_version: Option<i64>,
    pub new_version: Option<i64>,
    pub request_id: String,
    pub correlation_id: String,
    pub details: Value,
    pub occurred_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditPage {
    pub records: Vec<AuditRecordView>,
    pub next_cursor: Option<String>,
}

impl EnterpriseService {
    /// Newest-first audit records of the scope, optionally of one resource.
    pub async fn audit_records(
        &self,
        scope: CallScope,
        resource_id: Option<Uuid>,
        page: &PageRequest,
    ) -> EnterpriseResult<AuditPage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = String::from(
            "SELECT id, seq, actor_kind, actor_service_account_id, actor_credential_id, resource_type, resource_id, \
             operation, old_version, new_version, request_id, correlation_id, details, occurred_at \
             FROM enterprise_admin_audit \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3",
        );
        if let Some(resource) = resource_id {
            values.push(resource.into());
            sql.push_str(&format!(" AND resource_id = ${}", values.len()));
        }
        if let Some(cursor) = &page.cursor {
            let seq = decode_seq(cursor)?;
            values.push(seq.into());
            sql.push_str(&format!(" AND seq < ${}", values.len()));
        }
        // Insertion order (a monotonic sequence): stable even for equal timestamps.
        sql.push_str(&format!(" ORDER BY seq DESC LIMIT {}", limit + 1));
        let mut rows = AuditRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last().map(|r| encode_seq(r.seq))
        } else {
            None
        };
        Ok(AuditPage {
            records: rows
                .into_iter()
                .map(|r| AuditRecordView {
                    audit_id: r.id,
                    actor_kind: r.actor_kind,
                    actor_service_account_id: r.actor_service_account_id,
                    actor_credential_id: r.actor_credential_id,
                    resource_type: r.resource_type,
                    resource_id: r.resource_id,
                    operation: r.operation,
                    old_version: r.old_version,
                    new_version: r.new_version,
                    request_id: r.request_id,
                    correlation_id: r.correlation_id,
                    details: r.details,
                    occurred_at: time(r.occurred_at),
                })
                .collect(),
            next_cursor,
        })
    }
}
