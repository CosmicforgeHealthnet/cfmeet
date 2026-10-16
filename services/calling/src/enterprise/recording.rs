//! Call recording policy and recordings (Part F §61, §115–§129; ADR 0024).
//!
//! * Recording authority is SEPARATE: from the right to place a Call, from enterprise
//!   membership, from Privacy Relay authorization and from any meeting recording policy.
//!   Permission to call is never permission to record.
//! * A policy is typed and versioned: mode (DISABLED / MANUAL_ALLOWED /
//!   AUTO_WHEN_AUTHORIZED — automatic recording is never assumed), whether notice is
//!   required, whether consent is required and from whom, an opaque jurisdiction
//!   reference (compliance configuration is authoritative input — nothing is inferred
//!   from a country), retention and a maximum length. Managing policies needs
//!   `enterprise.recording.manage` AND the operator-granted MANAGE_RECORDING capability.
//! * Recordings are protected artifacts: integrity metadata only, never an object key or a
//!   stored URL. Every retrieval / deletion attempt — granted or denied — is written to
//!   `call_recording_access_audit`. A principal may reach a recording only when it was a
//!   participant of the recorded Call; another scope's recording is 404.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name, exec,
    policy::{self, AdminCapability, ResourceStatus},
    scope_values, stmt, time, AdminActor, AdminActorKind, EnterpriseError, EnterpriseResult,
    EnterpriseService, PageRequest,
};
use crate::calls::command::CallScope;

/// Signed media reads of a recording live this long.
pub const RECORDING_MEDIA_TTL: Duration = Duration::from_secs(300);
/// The longest recording a policy may allow (the edge delivers one WAV of at most
/// 24 MiB: 8 kHz 16-bit mono for 24 minutes).
pub const MAX_RECORDING_SECONDS: i32 = 1440;

text_enum!(
    /// DISABLED: never recorded. MANUAL_ALLOWED: recorded only on an explicit, authorized
    /// request. AUTO_WHEN_AUTHORIZED: the platform starts recording a CONNECTED Call by
    /// itself — but only once every configured notice / consent requirement is met.
    RecordingMode {
        Disabled = "DISABLED",
        ManualAllowed = "MANUAL_ALLOWED",
        AutoWhenAuthorized = "AUTO_WHEN_AUTHORIZED",
    }
);

text_enum!(
    /// Who must consent: nobody (NONE), every participant, or every participant other than
    /// the one requesting the recording (a system / administrator request: everyone).
    ConsentParticipants {
        Nobody = "NONE",
        AllParticipants = "ALL_PARTICIPANTS",
        OtherParticipants = "OTHER_PARTICIPANTS",
    }
);

text_enum!(
    /// A participant's consent decision.
    ConsentDecision {
        Granted = "GRANTED",
        Declined = "DECLINED",
    }
);

text_enum!(
    /// Processing state of a recording.
    RecordingState {
        Starting = "STARTING",
        Recording = "RECORDING",
        Stopping = "STOPPING",
        Processing = "PROCESSING",
        Ready = "READY",
        Failed = "FAILED",
        Deleting = "DELETING",
        Deleted = "DELETED",
    }
);

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct PolicyRow {
    pub id: Uuid,
    pub name: String,
    pub mode: String,
    pub notice_required: bool,
    pub consent_required: bool,
    pub consent_participants: String,
    pub jurisdiction_ref: String,
    pub retention_days: i32,
    pub max_recording_seconds: i32,
    pub is_default: bool,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

pub(crate) const POLICY_COLUMNS: &str = "id, name, mode, notice_required, consent_required, \
    consent_participants, jurisdiction_ref, retention_days, max_recording_seconds, is_default, \
    status, version, created_at, updated_at, disabled_at";

impl PolicyRow {
    pub(crate) fn mode(&self) -> RecordingMode {
        RecordingMode::parse(&self.mode).unwrap_or(RecordingMode::Disabled)
    }

    pub(crate) fn consent_participants(&self) -> ConsentParticipants {
        ConsentParticipants::parse(&self.consent_participants)
            .unwrap_or(ConsentParticipants::AllParticipants)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingPolicyView {
    pub recording_policy_id: Uuid,
    pub name: String,
    pub mode: String,
    pub notice_required: bool,
    pub consent_required: bool,
    pub consent_participants: String,
    pub jurisdiction_ref: String,
    pub retention_days: i32,
    pub max_recording_seconds: i32,
    pub is_default: bool,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

fn policy_view(row: PolicyRow) -> RecordingPolicyView {
    RecordingPolicyView {
        recording_policy_id: row.id,
        name: row.name,
        mode: row.mode,
        notice_required: row.notice_required,
        consent_required: row.consent_required,
        consent_participants: row.consent_participants,
        jurisdiction_ref: row.jurisdiction_ref,
        retention_days: row.retention_days,
        max_recording_seconds: row.max_recording_seconds,
        is_default: row.is_default,
        status: row.status,
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingPolicyPage {
    pub recording_policies: Vec<RecordingPolicyView>,
    pub next_cursor: Option<String>,
}

/// Policy settings (create: all; update: the given ones).
#[derive(Debug, Clone)]
pub struct RecordingPolicySettings {
    pub name: String,
    pub mode: RecordingMode,
    pub notice_required: bool,
    pub consent_participants: ConsentParticipants,
    pub jurisdiction_ref: String,
    pub retention_days: i32,
    pub max_recording_seconds: i32,
    pub is_default: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RecordingPolicyChange {
    pub name: Option<String>,
    pub mode: Option<RecordingMode>,
    pub notice_required: Option<bool>,
    pub consent_participants: Option<ConsentParticipants>,
    pub jurisdiction_ref: Option<String>,
    pub retention_days: Option<i32>,
    pub max_recording_seconds: Option<i32>,
    pub is_default: Option<bool>,
}

fn validate_settings(s: &RecordingPolicySettings) -> EnterpriseResult<()> {
    let jurisdiction = &s.jurisdiction_ref;
    if jurisdiction.is_empty()
        || jurisdiction.len() > 64
        || !jurisdiction.as_bytes()[0].is_ascii_alphanumeric()
        || !jurisdiction
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'-'))
    {
        return Err(EnterpriseError::Validation(
            "jurisdictionRef must be 1-64 characters of [A-Za-z0-9._:/-].",
        ));
    }
    // A jurisdiction reference names compliance configuration; it is never a number.
    if jurisdiction.bytes().filter(u8::is_ascii_digit).count() >= 7 {
        return Err(EnterpriseError::Validation(
            "jurisdictionRef must not look like a telephone number.",
        ));
    }
    if !(1..=3650).contains(&s.retention_days) {
        return Err(EnterpriseError::Validation(
            "retentionDays must be between 1 and 3650.",
        ));
    }
    if !(60..=MAX_RECORDING_SECONDS).contains(&s.max_recording_seconds) {
        return Err(EnterpriseError::Validation(
            "maxRecordingSeconds must be between 60 and 1440.",
        ));
    }
    Ok(())
}

pub(crate) async fn policy_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<PolicyRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    PolicyRow::find_by_statement(stmt(
        &format!(
            "SELECT {POLICY_COLUMNS} FROM call_recording_policies \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// The scope's effective recording policy: its ACTIVE default (none: nothing is ever
/// recorded).
pub(crate) async fn effective_recording_policy<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
) -> EnterpriseResult<Option<PolicyRow>> {
    Ok(PolicyRow::find_by_statement(stmt(
        &format!(
            "SELECT {POLICY_COLUMNS} FROM call_recording_policies \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND is_default AND status = 'ACTIVE'"
        ),
        scope_values(scope),
    ))
    .one(db)
    .await?)
}

/// A recording as participants / administrators see it: integrity metadata, never the
/// object key or a URL.
#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct RecordingRow {
    pub id: Uuid,
    pub call_id: Uuid,
    pub authorization_id: Uuid,
    pub policy_id: Uuid,
    pub capture_source: String,
    pub call_leg_id: Option<Uuid>,
    pub state: String,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub stop_reason: Option<String>,
    pub object_key: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_ms: Option<i32>,
    pub sha256: Option<String>,
    pub ready_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub retention_until: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub delete_reason: Option<String>,
    pub delete_attempts: i32,
    pub version: i64,
}

pub(crate) const RECORDING_COLUMNS: &str = "id, call_id, authorization_id, policy_id, \
    capture_source, call_leg_id, state, started_at, stopped_at, stop_reason, object_key, content_type, size_bytes, duration_ms, sha256, ready_at, \
    failure_code, retention_until, deleted_at, delete_reason, delete_attempts, version";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRecordingView {
    pub recording_id: Uuid,
    pub call_id: Uuid,
    pub authorization_id: Uuid,
    pub recording_policy_id: Uuid,
    pub capture_source: String,
    pub state: String,
    pub started_at: String,
    pub stopped_at: Option<String>,
    pub stop_reason: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_ms: Option<i32>,
    pub sha256: Option<String>,
    pub ready_at: Option<String>,
    pub failure_code: Option<String>,
    pub retention_until: Option<String>,
    pub deleted_at: Option<String>,
    pub version: i64,
}

pub(crate) fn recording_view(row: RecordingRow) -> CallRecordingView {
    CallRecordingView {
        recording_id: row.id,
        call_id: row.call_id,
        authorization_id: row.authorization_id,
        recording_policy_id: row.policy_id,
        capture_source: row.capture_source,
        state: row.state,
        started_at: time(row.started_at),
        stopped_at: row.stopped_at.map(time),
        stop_reason: row.stop_reason,
        content_type: row.content_type,
        size_bytes: row.size_bytes,
        duration_ms: row.duration_ms,
        sha256: row.sha256,
        ready_at: row.ready_at.map(time),
        failure_code: row.failure_code,
        retention_until: row.retention_until.map(time),
        deleted_at: row.deleted_at.map(time),
        version: row.version,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRecordingPage {
    pub recordings: Vec<CallRecordingView>,
    pub next_cursor: Option<String>,
}

/// A server-authoritative recording decision.
#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct AuthorizationRow {
    pub id: Uuid,
    pub call_id: Uuid,
    pub policy_id: Option<Uuid>,
    pub policy_version: Option<i64>,
    pub mode: String,
    pub jurisdiction_ref: Option<String>,
    pub actor_kind: String,
    pub actor_principal_id: Option<Uuid>,
    pub required_participant_ids: Vec<Uuid>,
    pub notice_evidence: serde_json::Value,
    pub consent_evidence: serde_json::Value,
    pub decision: String,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

pub(crate) const AUTHORIZATION_COLUMNS: &str = "id, call_id, policy_id, policy_version, mode, \
    jurisdiction_ref, actor_kind, actor_principal_id, required_participant_ids, notice_evidence, \
    consent_evidence, decision, reason, created_at";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingActorView {
    pub kind: String,
    pub principal_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingAuthorizationView {
    pub authorization_id: Uuid,
    pub call_id: Uuid,
    pub recording_policy_id: Option<Uuid>,
    pub policy_version: Option<i64>,
    pub mode: String,
    pub jurisdiction_ref: Option<String>,
    pub actor: RecordingActorView,
    pub required_participant_ids: Vec<Uuid>,
    pub notice: serde_json::Value,
    pub consent: serde_json::Value,
    pub decision: String,
    pub reason: String,
    pub created_at: String,
}

pub(crate) fn authorization_view(row: AuthorizationRow) -> RecordingAuthorizationView {
    RecordingAuthorizationView {
        authorization_id: row.id,
        call_id: row.call_id,
        recording_policy_id: row.policy_id,
        policy_version: row.policy_version,
        mode: row.mode,
        jurisdiction_ref: row.jurisdiction_ref,
        actor: RecordingActorView {
            kind: row.actor_kind,
            principal_id: row.actor_principal_id,
        },
        required_participant_ids: row.required_participant_ids,
        notice: row.notice_evidence,
        consent: row.consent_evidence,
        decision: row.decision,
        reason: row.reason,
        created_at: time(row.created_at),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingAuthorizationList {
    pub authorizations: Vec<RecordingAuthorizationView>,
}

/// A short-lived, single-object, read-only media URL of a recording (never stored).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingMediaAccess {
    pub recording_id: Uuid,
    pub url: String,
    pub expires_at: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub sha256: String,
}

/// One access-audit record.
#[derive(Debug, Clone, FromQueryResult)]
struct AccessRow {
    seq: i64,
    recording_id: Uuid,
    call_id: Uuid,
    actor_kind: String,
    actor_credential_id: Option<Uuid>,
    on_behalf_of_principal_id: Option<Uuid>,
    operation: String,
    outcome: String,
    reason: Option<String>,
    request_id: String,
    correlation_id: String,
    occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingAccessRecord {
    pub seq: i64,
    pub recording_id: Uuid,
    pub call_id: Uuid,
    pub actor_kind: String,
    pub actor_credential_id: Option<Uuid>,
    pub on_behalf_of_principal_id: Option<Uuid>,
    pub operation: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub request_id: String,
    pub correlation_id: String,
    pub occurred_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingAccessList {
    pub records: Vec<RecordingAccessRecord>,
}

/// Who asks for recordings: a recording administrator (the credential holds
/// `enterprise.recording.manage`), or the application acting for one principal, who must
/// have been a participant of the recorded Call.
#[derive(Debug, Clone, Copy)]
pub enum RecordingRequester {
    Administrator,
    Principal(Uuid),
}

impl RecordingRequester {
    fn principal(&self) -> Option<Uuid> {
        match self {
            RecordingRequester::Principal(p) => Some(*p),
            RecordingRequester::Administrator => None,
        }
    }
}

/// Access-audited operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessOperation {
    ReadMetadata,
    MediaAccess,
    Delete,
    RetentionDelete,
    Stop,
}

impl AccessOperation {
    fn as_str(&self) -> &'static str {
        match self {
            AccessOperation::ReadMetadata => "READ_METADATA",
            AccessOperation::MediaAccess => "MEDIA_ACCESS",
            AccessOperation::Delete => "DELETE",
            AccessOperation::RetentionDelete => "RETENTION_DELETE",
            AccessOperation::Stop => "STOP",
        }
    }
}

/// Writes one access-audit record (granted or denied).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn access_audit<C: ConnectionTrait>(
    db: &C,
    actor: &AdminActor,
    recording_id: Uuid,
    call_id: Uuid,
    on_behalf_of: Option<Uuid>,
    operation: AccessOperation,
    granted: bool,
    reason: Option<&str>,
    now: DateTime<Utc>,
) -> EnterpriseResult<()> {
    let mut values = vec![Uuid::new_v4().into()];
    values.extend(scope_values(actor.scope));
    values.extend([
        recording_id.into(),
        call_id.into(),
        if actor.kind == AdminActorKind::System {
            "SYSTEM"
        } else {
            "APPLICATION_CREDENTIAL"
        }
        .into(),
        actor.service_account_id.map(|s| s.as_uuid()).into(),
        actor.credential_id.map(|c| c.as_uuid()).into(),
        on_behalf_of.into(),
        operation.as_str().into(),
        if granted { "GRANTED" } else { "DENIED" }.into(),
        reason.map(str::to_string).into(),
        actor.request_id.clone().into(),
        actor.correlation_id.clone().into(),
        now.into(),
    ]);
    exec(
        db,
        "INSERT INTO call_recording_access_audit (id, tenant_id, application_id, environment_id, \
           recording_id, call_id, actor_kind, actor_service_account_id, actor_credential_id, \
           on_behalf_of_principal_id, operation, outcome, reason, request_id, correlation_id, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        values,
    )
    .await?;
    crate::metrics::inc(
        "calling_enterprise_artifact_access_total",
        &[
            ("artifact", "RECORDING"),
            ("outcome", if granted { "GRANTED" } else { "DENIED" }),
        ],
    );
    Ok(())
}

pub(crate) async fn recording_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<RecordingRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    RecordingRow::find_by_statement(stmt(
        &format!(
            "SELECT {RECORDING_COLUMNS} FROM call_recordings \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// Whether `principal` is (or was) a participant of `call`.
pub(crate) async fn was_participant<C: ConnectionTrait>(
    db: &C,
    call_id: Uuid,
    principal: Uuid,
) -> EnterpriseResult<bool> {
    Ok(db
        .query_one(stmt(
            "SELECT 1 FROM call_participants WHERE call_id = $1 AND principal_id = $2",
            vec![call_id.into(), principal.into()],
        ))
        .await?
        .is_some())
}

/// The object key of a recording (opaque, internal, never returned).
pub(crate) fn object_key(scope: CallScope, recording_id: Uuid, content_type: &str) -> String {
    // The extension follows the verified format (SIP edge: WAV; RTC egress: Ogg/Opus).
    let extension = if content_type == "audio/ogg" {
        "ogg"
    } else {
        "wav"
    };
    format!(
        "recordings/{}/{}/{}/{recording_id}.{extension}",
        scope.tenant_id().as_uuid().simple(),
        scope.application_id().as_uuid().simple(),
        scope.environment_id().as_uuid().simple()
    )
}

impl EnterpriseService {
    pub async fn create_recording_policy(
        &self,
        actor: &AdminActor,
        settings: RecordingPolicySettings,
    ) -> EnterpriseResult<RecordingPolicyView> {
        let name = clean_name(&settings.name, 100, "name must be 1-100 characters.")?;
        validate_settings(&settings)?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRecording).await?;
        let now = self.now();
        if settings.is_default {
            self.clear_default_in(&txn, actor, now).await?;
        }
        let id = Uuid::new_v4();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            name.into(),
            settings.mode.as_str().into(),
            settings.notice_required.into(),
            (settings.consent_participants != ConsentParticipants::Nobody).into(),
            settings.consent_participants.as_str().into(),
            settings.jurisdiction_ref.clone().into(),
            settings.retention_days.into(),
            settings.max_recording_seconds.into(),
            settings.is_default.into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO call_recording_policies (id, tenant_id, application_id, environment_id, name, \
               mode, notice_required, consent_required, consent_participants, jurisdiction_ref, \
               retention_days, max_recording_seconds, is_default, status, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'ACTIVE', 1, $14, $14)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::RecordingPolicy,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new()
                    .with("mode", settings.mode.as_str())
                    .with("noticeRequired", settings.notice_required)
                    .with(
                        "consentParticipants",
                        settings.consent_participants.as_str(),
                    )
                    .with("retentionDays", settings.retention_days as i64)
                    .with("isDefault", settings.is_default),
            },
            now,
        )
        .await?;
        let out = policy_view(policy_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "RECORDING_POLICY"), ("operation", "CREATE")],
        );
        Ok(out)
    }

    /// The current default stops being the default (versioned, audited).
    async fn clear_default_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        actor: &AdminActor,
        now: DateTime<Utc>,
    ) -> EnterpriseResult<()> {
        let Some(current) = PolicyRow::find_by_statement(stmt(
            &format!(
                "SELECT {POLICY_COLUMNS} FROM call_recording_policies \
                 WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
                   AND is_default AND status = 'ACTIVE' FOR UPDATE"
            ),
            scope_values(actor.scope),
        ))
        .one(txn)
        .await?
        else {
            return Ok(());
        };
        exec(
            txn,
            "UPDATE call_recording_policies SET is_default = false, version = version + 1, \
             updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![current.id.into(), now.into()],
        )
        .await?;
        audit::record(
            txn,
            actor,
            AuditFact {
                resource: AuditResource::RecordingPolicy,
                resource_id: current.id,
                operation: AuditOperation::Update,
                old_version: Some(current.version),
                new_version: Some(current.version + 1),
                details: AuditDetails::new().with("isDefault", false),
            },
            now,
        )
        .await?;
        Ok(())
    }

    pub async fn get_recording_policy(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<RecordingPolicyView> {
        Ok(policy_view(policy_row(self.db(), scope, id, false).await?))
    }

    pub async fn list_recording_policies(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<RecordingPolicyPage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {POLICY_COLUMNS} FROM call_recording_policies \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        if let Some(status) = status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some((at, id)) = page.after()? {
            values.push(at.into());
            values.push(id.into());
            sql.push_str(&format!(
                " AND (created_at, id) > (${}, ${})",
                values.len() - 1,
                values.len()
            ));
        }
        sql.push_str(&format!(" ORDER BY created_at, id LIMIT {}", limit + 1));
        let mut rows = PolicyRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        Ok(RecordingPolicyPage {
            recording_policies: rows.into_iter().map(policy_view).collect(),
            next_cursor,
        })
    }

    /// Changes a policy. Returns the view and whether recording became NOT permitted (the
    /// runtime then stops active recordings under it: POLICY_REVOKED).
    pub async fn update_recording_policy(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        change: RecordingPolicyChange,
    ) -> EnterpriseResult<(RecordingPolicyView, bool)> {
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRecording).await?;
        let row = policy_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled policy cannot be changed",
            ));
        }
        let settings = RecordingPolicySettings {
            name: match change.name.as_deref() {
                Some(n) => clean_name(n, 100, "name must be 1-100 characters.")?,
                None => row.name.clone(),
            },
            mode: change.mode.unwrap_or(row.mode()),
            notice_required: change.notice_required.unwrap_or(row.notice_required),
            consent_participants: change
                .consent_participants
                .unwrap_or(row.consent_participants()),
            jurisdiction_ref: change
                .jurisdiction_ref
                .clone()
                .unwrap_or(row.jurisdiction_ref.clone()),
            retention_days: change.retention_days.unwrap_or(row.retention_days),
            max_recording_seconds: change
                .max_recording_seconds
                .unwrap_or(row.max_recording_seconds),
            is_default: change.is_default.unwrap_or(row.is_default),
        };
        validate_settings(&settings)?;
        let now = self.now();
        if settings.is_default && !row.is_default {
            self.clear_default_in(&txn, actor, now).await?;
        }
        exec(
            &txn,
            "UPDATE call_recording_policies SET name = $2, mode = $3, notice_required = $4, \
               consent_required = $5, consent_participants = $6, jurisdiction_ref = $7, \
               retention_days = $8, max_recording_seconds = $9, is_default = $10, \
               version = version + 1, updated_at = GREATEST(updated_at, $11) WHERE id = $1",
            vec![
                id.into(),
                settings.name.clone().into(),
                settings.mode.as_str().into(),
                settings.notice_required.into(),
                (settings.consent_participants != ConsentParticipants::Nobody).into(),
                settings.consent_participants.as_str().into(),
                settings.jurisdiction_ref.clone().into(),
                settings.retention_days.into(),
                settings.max_recording_seconds.into(),
                settings.is_default.into(),
                now.into(),
            ],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::RecordingPolicy,
                resource_id: id,
                operation: AuditOperation::Update,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new()
                    .with("mode", settings.mode.as_str())
                    .with("noticeRequired", settings.notice_required)
                    .with(
                        "consentParticipants",
                        settings.consent_participants.as_str(),
                    )
                    .with("retentionDays", settings.retention_days as i64)
                    .with("isDefault", settings.is_default),
            },
            now,
        )
        .await?;
        let out = policy_view(policy_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        // Recording stops being permitted: disabled, no longer the default, or stricter
        // evidence requirements than the running recordings were authorized under.
        let revoked = settings.mode == RecordingMode::Disabled
            || (row.is_default && !settings.is_default)
            || (settings.notice_required && !row.notice_required)
            || settings.consent_participants != row.consent_participants();
        Ok((out, revoked))
    }

    /// Never blocked: the runtime stops active recordings under it (POLICY_REVOKED).
    pub async fn disable_recording_policy(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<RecordingPolicyView> {
        let txn = self.begin().await?;
        let row = policy_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict("the policy is already disabled"));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE call_recording_policies SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::RecordingPolicy,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = policy_view(policy_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    /// Authorizes `requester` on the recording (participant of its Call, or a recording
    /// administrator); a refused principal is audited and gets 404.
    async fn authorized_recording<C: ConnectionTrait>(
        &self,
        db: &C,
        actor: &AdminActor,
        id: Uuid,
        requester: RecordingRequester,
        operation: AccessOperation,
        lock: bool,
    ) -> EnterpriseResult<RecordingRow> {
        let row = recording_row(db, actor.scope, id, lock).await?;
        if let RecordingRequester::Principal(p) = requester {
            if !was_participant(db, row.call_id, p).await? {
                // Audited on its own connection: the caller's transaction is rolled back.
                access_audit(
                    self.db(),
                    actor,
                    row.id,
                    row.call_id,
                    Some(p),
                    operation,
                    false,
                    Some("NOT_A_PARTICIPANT"),
                    self.now(),
                )
                .await?;
                return Err(EnterpriseError::NotFound);
            }
        }
        Ok(row)
    }

    /// The recordings of a Call (participant or administrator), oldest first.
    pub async fn list_call_recordings(
        &self,
        scope: CallScope,
        call_id: Uuid,
        requester: RecordingRequester,
    ) -> EnterpriseResult<CallRecordingPage> {
        self.require_call_visible(scope, call_id, requester).await?;
        let mut values = scope_values(scope);
        values.push(call_id.into());
        let rows = RecordingRow::find_by_statement(stmt(
            &format!(
                "SELECT {RECORDING_COLUMNS} FROM call_recordings \
                 WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND call_id = $4 \
                 ORDER BY created_at, id LIMIT 100"
            ),
            values,
        ))
        .all(self.db())
        .await?;
        Ok(CallRecordingPage {
            recordings: rows.into_iter().map(recording_view).collect(),
            next_cursor: None,
        })
    }

    /// The recording decisions taken for a Call (participant or administrator).
    pub async fn list_recording_authorizations(
        &self,
        scope: CallScope,
        call_id: Uuid,
        requester: RecordingRequester,
    ) -> EnterpriseResult<RecordingAuthorizationList> {
        self.require_call_visible(scope, call_id, requester).await?;
        let mut values = scope_values(scope);
        values.push(call_id.into());
        let rows = AuthorizationRow::find_by_statement(stmt(
            &format!(
                "SELECT {AUTHORIZATION_COLUMNS} FROM call_recording_authorizations \
                 WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND call_id = $4 \
                 ORDER BY seq LIMIT 200"
            ),
            values,
        ))
        .all(self.db())
        .await?;
        Ok(RecordingAuthorizationList {
            authorizations: rows.into_iter().map(authorization_view).collect(),
        })
    }

    async fn require_call_visible(
        &self,
        scope: CallScope,
        call_id: Uuid,
        requester: RecordingRequester,
    ) -> EnterpriseResult<()> {
        let mut values = scope_values(scope);
        values.push(call_id.into());
        let exists = self
            .db()
            .query_one(stmt(
                "SELECT 1 FROM calls WHERE tenant_id = $1 AND application_id = $2 \
                   AND environment_id = $3 AND id = $4",
                values,
            ))
            .await?
            .is_some();
        if !exists {
            return Err(EnterpriseError::NotFound);
        }
        if let Some(p) = requester.principal() {
            if !was_participant(self.db(), call_id, p).await? {
                return Err(EnterpriseError::NotFound);
            }
        }
        Ok(())
    }

    /// A recording's metadata (audited READ_METADATA).
    pub async fn get_call_recording(
        &self,
        actor: &AdminActor,
        id: Uuid,
        requester: RecordingRequester,
    ) -> EnterpriseResult<CallRecordingView> {
        let row = self
            .authorized_recording(
                self.db(),
                actor,
                id,
                requester,
                AccessOperation::ReadMetadata,
                false,
            )
            .await?;
        access_audit(
            self.db(),
            actor,
            row.id,
            row.call_id,
            requester.principal(),
            AccessOperation::ReadMetadata,
            true,
            None,
            self.now(),
        )
        .await?;
        Ok(recording_view(row))
    }

    /// Authorized media access: participant / administrator, READY only, a short-lived
    /// signed read of ONE object, audited (also when refused). Nothing is stored.
    pub async fn recording_media_access(
        &self,
        actor: &AdminActor,
        id: Uuid,
        requester: RecordingRequester,
    ) -> EnterpriseResult<RecordingMediaAccess> {
        let row = self
            .authorized_recording(
                self.db(),
                actor,
                id,
                requester,
                AccessOperation::MediaAccess,
                false,
            )
            .await?;
        let now = self.now();
        let deny = |reason: &'static str| async move {
            access_audit(
                self.db(),
                actor,
                row.id,
                row.call_id,
                requester.principal(),
                AccessOperation::MediaAccess,
                false,
                Some(reason),
                now,
            )
            .await
        };
        let (Some(key), Some(content_type), Some(size), Some(sha256), "READY") = (
            row.object_key.as_deref(),
            row.content_type.clone(),
            row.size_bytes,
            row.sha256.clone(),
            row.state.as_str(),
        ) else {
            deny("NOT_READY").await?;
            return Err(EnterpriseError::Conflict(
                "the recording media is not available",
            ));
        };
        let read = match self.artifacts.signed_read(key, RECORDING_MEDIA_TTL, now) {
            Ok(r) => r,
            Err(cf_object_storage::StorageError::NotConfigured) => {
                deny("STORAGE_NOT_CONFIGURED").await?;
                return Err(EnterpriseError::Unavailable(
                    "artifact storage is not configured",
                ));
            }
            Err(_) => {
                deny("STORAGE_UNAVAILABLE").await?;
                return Err(EnterpriseError::StorageUnavailable);
            }
        };
        access_audit(
            self.db(),
            actor,
            row.id,
            row.call_id,
            requester.principal(),
            AccessOperation::MediaAccess,
            true,
            None,
            now,
        )
        .await?;
        Ok(RecordingMediaAccess {
            recording_id: id,
            url: read.url,
            expires_at: time(read.expires_at),
            content_type,
            size_bytes: size,
            sha256,
        })
    }

    /// Requests deletion (asynchronous: DELETING now, the object removed by the worker).
    pub async fn delete_call_recording(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        requester: RecordingRequester,
    ) -> EnterpriseResult<CallRecordingView> {
        let txn = self.begin().await?;
        let row = self
            .authorized_recording(&txn, actor, id, requester, AccessOperation::Delete, true)
            .await?;
        check_version(expected_version, row.version)?;
        let now = self.now();
        if !matches!(row.state.as_str(), "READY" | "FAILED") {
            drop(txn);
            access_audit(
                self.db(),
                actor,
                row.id,
                row.call_id,
                requester.principal(),
                AccessOperation::Delete,
                false,
                Some("NOT_DELETABLE"),
                now,
            )
            .await?;
            return Err(EnterpriseError::Conflict(
                "the recording is still being captured or is already deleted",
            ));
        }
        let reason = match requester {
            RecordingRequester::Principal(_) => "USER",
            RecordingRequester::Administrator => "ADMIN",
        };
        exec(
            &txn,
            "UPDATE call_recordings SET state = 'DELETING', deleting_at = $2, delete_reason = $3, \
             next_attempt_at = $2, version = version + 1, updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1",
            vec![id.into(), now.into(), reason.into()],
        )
        .await?;
        access_audit(
            &txn,
            actor,
            row.id,
            row.call_id,
            requester.principal(),
            AccessOperation::Delete,
            true,
            None,
            now,
        )
        .await?;
        let out = recording_view(recording_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    /// The access-audit trail of a recording (recording administrators).
    pub async fn recording_access_audit(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<RecordingAccessList> {
        recording_row(self.db(), scope, id, false).await?;
        let mut values = scope_values(scope);
        values.push(id.into());
        let rows = AccessRow::find_by_statement(stmt(
            "SELECT seq, recording_id, call_id, actor_kind, actor_credential_id, \
               on_behalf_of_principal_id, operation, outcome, reason, request_id, correlation_id, occurred_at \
             FROM call_recording_access_audit \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND recording_id = $4 \
             ORDER BY seq LIMIT 500",
            values,
        ))
        .all(self.db())
        .await?;
        Ok(RecordingAccessList {
            records: rows
                .into_iter()
                .map(|r| RecordingAccessRecord {
                    seq: r.seq,
                    recording_id: r.recording_id,
                    call_id: r.call_id,
                    actor_kind: r.actor_kind,
                    actor_credential_id: r.actor_credential_id,
                    on_behalf_of_principal_id: r.on_behalf_of_principal_id,
                    operation: r.operation,
                    outcome: r.outcome,
                    reason: r.reason,
                    request_id: r.request_id,
                    correlation_id: r.correlation_id,
                    occurred_at: time(r.occurred_at),
                })
                .collect(),
        })
    }
}
