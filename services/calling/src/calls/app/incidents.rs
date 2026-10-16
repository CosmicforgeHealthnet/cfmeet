//! Calling operational incidents (Part F §168; ADR 0026): enterprise conditions an
//! operator must look at — a stuck queue entry, a queue assignment conflict, IVR routing
//! corruption, a voicemail / recording artifact that could not be finalized, a repeated
//! forwarding loop block. Same discipline as Telephony's incidents: deduplicated while
//! OPEN by a stable key (a repeat bumps `occurrences`), bounded restricted evidence (opaque
//! ids and codes only), a bounded metric per category. Raising one never fails the
//! operation that observed it.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use super::enterprise::stmt;
use crate::calls::command::CallScope;

text_enum!(
    /// Closed vocabulary of Calling incidents.
    IncidentCategory {
        StuckQueueEntry = "STUCK_QUEUE_ENTRY",
        QueueAssignmentConflict = "QUEUE_ASSIGNMENT_CONFLICT",
        IvrRoutingCorruption = "IVR_ROUTING_CORRUPTION",
        VoicemailFinalizationFailure = "VOICEMAIL_FINALIZATION_FAILURE",
        RecordingArtifactFailure = "RECORDING_ARTIFACT_FAILURE",
        ForwardingLoopRepeated = "FORWARDING_LOOP_REPEATED",
        // Part H §83: one initiator ran into ACTIVE block rules repeatedly.
        RepeatedBlockedAttempts = "REPEATED_BLOCKED_ATTEMPTS",
        // Part H §83: one initiator ran into destination-policy denials repeatedly.
        RepeatedRouteDenials = "REPEATED_ROUTE_DENIALS",
        // Part H §69: several participants reported the same principal.
        AbuseReportsBurst = "ABUSE_REPORTS_BURST",
        // §81: a durable Telephony command reached its attempt cap (DEAD, not retried).
        TelephonyCommandDead = "TELEPHONY_COMMAND_DEAD",
        // §81: outbox events were dead-lettered (one open incident per environment).
        OutboxDeadLetter = "OUTBOX_DEAD_LETTER",
        // §86: the oldest pending outbox event / Telephony command is older than the
        // configured age (platform-wide, one open incident per queue).
        WorkerBacklog = "WORKER_BACKLOG",
        // §109: repeated failed authentications for one presented credential key id.
        CredentialGuessing = "CREDENTIAL_GUESSING",
    }
);

text_enum!(IncidentSeverity {
    Low = "LOW",
    Medium = "MEDIUM",
    High = "HIGH",
    Critical = "CRITICAL",
});

text_enum!(
    /// What an incident is about (an opaque id of that kind).
    IncidentResource {
        QueueEntry = "QUEUE_ENTRY",
        IvrSession = "IVR_SESSION",
        VoicemailMessage = "VOICEMAIL_MESSAGE",
        CallRecording = "CALL_RECORDING",
        Destination = "DESTINATION",
        Principal = "PRINCIPAL",
        TelephonyCommand = "TELEPHONY_COMMAND",
        OutboxEvent = "OUTBOX_EVENT",
        Credential = "CREDENTIAL",
    }
);

text_enum!(
    /// Lifecycle of an incident (no ACKNOWLEDGED: an operator resolves, and the
    /// resolution history is the operator audit).
    IncidentStatus {
        Open = "OPEN",
        Resolved = "RESOLVED",
    }
);

/// One observation.
#[derive(Debug, Clone)]
pub struct Incident {
    pub category: IncidentCategory,
    pub severity: IncidentSeverity,
    /// `None`: a platform-wide incident (worker backlog, guessing of unknown credential
    /// key ids) — only those categories may be unscoped (the table's CHECK).
    pub scope: Option<CallScope>,
    pub call_id: Option<Uuid>,
    pub resource: Option<(IncidentResource, Uuid)>,
    pub evidence: Value,
    /// An optional time bucket (e.g. a window start) making the dedupe key per window.
    pub bucket: Option<String>,
}

impl Incident {
    pub fn new(
        category: IncidentCategory,
        severity: IncidentSeverity,
        scope: CallScope,
        resource: IncidentResource,
        id: Uuid,
    ) -> Self {
        Self {
            category,
            severity,
            scope: Some(scope),
            call_id: None,
            resource: Some((resource, id)),
            evidence: Value::Object(Default::default()),
            bucket: None,
        }
    }

    /// One open incident per environment for `category` (no resource).
    pub fn scoped(
        category: IncidentCategory,
        severity: IncidentSeverity,
        scope: CallScope,
    ) -> Self {
        Self {
            category,
            severity,
            scope: Some(scope),
            call_id: None,
            resource: None,
            evidence: Value::Object(Default::default()),
            bucket: Some(scope.environment_id().as_uuid().simple().to_string()),
        }
    }

    /// A platform-wide incident (no tenant scope), deduplicated per `bucket`.
    pub fn platform(
        category: IncidentCategory,
        severity: IncidentSeverity,
        bucket: &'static str,
    ) -> Self {
        Self {
            category,
            severity,
            scope: None,
            call_id: None,
            resource: None,
            evidence: Value::Object(Default::default()),
            bucket: Some(bucket.to_string()),
        }
    }

    /// The resource the incident is about.
    pub fn resource(mut self, resource: IncidentResource, id: Uuid) -> Self {
        self.resource = Some((resource, id));
        self
    }

    /// Dedupes per `bucket` (a bounded label such as a window start) instead of once
    /// per open resource.
    pub fn bucket(mut self, bucket: impl Into<String>) -> Self {
        self.bucket = Some(bucket.into());
        self
    }

    pub fn call(mut self, call_id: Uuid) -> Self {
        self.call_id = Some(call_id);
        self
    }

    pub fn evidence(mut self, evidence: Value) -> Self {
        self.evidence = evidence;
        self
    }

    /// Stable while open: one incident per category and resource.
    fn dedupe_key(&self) -> String {
        let category = self.category.as_str().to_ascii_lowercase();
        let key = match self.resource {
            Some((kind, id)) => format!(
                "{category}:{}:{}",
                kind.as_str().to_ascii_lowercase(),
                id.simple()
            ),
            None => format!("{category}:{}", self.call_id.unwrap_or_default().simple()),
        };
        match &self.bucket {
            Some(bucket) => {
                let bucket: String = bucket
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                    .take(32)
                    .collect::<String>()
                    .to_ascii_lowercase();
                format!("{key}:{bucket}")
            }
            None => key,
        }
    }
}

/// Opens `incident` or counts another occurrence of the open one with the same key.
/// Best effort: a failure is logged and never propagated to the caller's operation. Inside
/// a caller's transaction it runs in a SAVEPOINT, so a failed insert never poisons it (and
/// the incident commits only with the operation that observed it).
pub(crate) async fn raise<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    incident: &Incident,
    at: DateTime<Utc>,
) {
    let evidence = if incident.evidence.to_string().len() > 3500 {
        serde_json::json!({ "truncated": true })
    } else {
        incident.evidence.clone()
    };
    let insert = stmt(
        "INSERT INTO calling_incidents (id, tenant_id, application_id, environment_id, \
             dedupe_key, category, severity, status, call_id, resource_type, resource_id, \
             occurrences, restricted_evidence, first_seen_at, last_seen_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'OPEN', $8, $9, $10, 1, $11, $12, $12) \
             ON CONFLICT (dedupe_key) WHERE status = 'OPEN' DO UPDATE SET \
               occurrences = calling_incidents.occurrences + 1, \
               last_seen_at = GREATEST(calling_incidents.last_seen_at, EXCLUDED.last_seen_at), \
               restricted_evidence = EXCLUDED.restricted_evidence",
        vec![
            Uuid::new_v4().into(),
            incident.scope.map(|s| s.tenant_id().as_uuid()).into(),
            incident.scope.map(|s| s.application_id().as_uuid()).into(),
            incident.scope.map(|s| s.environment_id().as_uuid()).into(),
            incident.dedupe_key().into(),
            incident.category.as_str().into(),
            incident.severity.as_str().into(),
            incident.call_id.into(),
            incident.resource.map(|(kind, _)| kind.as_str()).into(),
            incident.resource.map(|(_, id)| id).into(),
            evidence.into(),
            at.into(),
        ],
    );
    let result = async {
        let savepoint = db.begin().await?;
        savepoint.execute(insert).await?;
        savepoint.commit().await
    }
    .await;
    crate::metrics::inc(
        "calling_enterprise_incidents_total",
        &[("category", incident.category.as_str())],
    );
    match result {
        Ok(_) => tracing::warn!(
            category = incident.category.as_str(),
            severity = incident.severity.as_str(),
            call_id = ?incident.call_id,
            "calling incident raised"
        ),
        Err(err) => tracing::error!(
            category = incident.category.as_str(),
            error = %err,
            "calling incident could not be recorded"
        ),
    }
}

/// Operator view of one incident: opaque ids, bounded codes and counts only (the
/// restricted evidence holds opaque ids and codes too — never a number or a secret).
#[derive(Debug, Clone, serde::Serialize, sea_orm::FromQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct IncidentView {
    pub id: Uuid,
    pub category: String,
    pub severity: String,
    pub status: String,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub call_id: Option<Uuid>,
    pub resource_type: Option<String>,
    pub resource_id: Option<Uuid>,
    pub occurrences: i32,
    pub restricted_evidence: Value,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

const VIEW_COLUMNS: &str = "id, category, severity, status, tenant_id, application_id, \
    environment_id, call_id, resource_type, resource_id, occurrences, restricted_evidence, \
    first_seen_at, last_seen_at, resolved_at";

/// `platform_admin incidents list`: newest first, optionally one status / category,
/// bounded (1–500).
pub async fn list_incidents<C: ConnectionTrait>(
    db: &C,
    status: Option<IncidentStatus>,
    category: Option<IncidentCategory>,
    limit: u64,
) -> Result<Vec<IncidentView>, sea_orm::DbErr> {
    use sea_orm::FromQueryResult;
    IncidentView::find_by_statement(stmt(
        &format!(
            "SELECT {VIEW_COLUMNS} FROM calling_incidents \
             WHERE ($1::text IS NULL OR status = $1) AND ($2::text IS NULL OR category = $2) \
             ORDER BY last_seen_at DESC, id LIMIT $3"
        ),
        vec![
            status.map(|s| s.as_str()).into(),
            category.map(|c| c.as_str()).into(),
            (limit.clamp(1, 500) as i64).into(),
        ],
    ))
    .all(db)
    .await
}

/// `platform_admin incidents resolve`: OPEN → RESOLVED by an operator, with exactly one
/// operator audit row (INCIDENT_RESOLVED) in the same transaction — the audit rows ARE
/// the resolution history. Resolving a RESOLVED incident changes nothing (idempotent;
/// still audited, like every operator command). A later occurrence opens a NEW incident.
pub async fn operator_resolve(
    db: &sea_orm::DatabaseConnection,
    operator: &crate::platform::operator::OperatorContext,
    id: Uuid,
    now: DateTime<Utc>,
) -> crate::platform::PlatformResult<IncidentView> {
    use sea_orm::FromQueryResult;

    use crate::platform::{
        operator::{self, AuditEntry, AuditTargetKind, OperatorAction},
        PlatformError,
    };
    operator::audited(db, operator, now, move |txn| {
        Box::pin(async move {
            let current = IncidentView::find_by_statement(stmt(
                &format!("SELECT {VIEW_COLUMNS} FROM calling_incidents WHERE id = $1 FOR UPDATE"),
                vec![id.into()],
            ))
            .one(txn)
            .await?
            .ok_or(PlatformError::NotFound("incident"))?;
            let view = if current.status == IncidentStatus::Open.as_str() {
                IncidentView::find_by_statement(stmt(
                    &format!(
                        "UPDATE calling_incidents SET status = 'RESOLVED', \
                           resolved_at = GREATEST($2, last_seen_at) \
                         WHERE id = $1 AND status = 'OPEN' RETURNING {VIEW_COLUMNS}"
                    ),
                    vec![id.into(), now.into()],
                ))
                .one(txn)
                .await?
                .ok_or(PlatformError::NotFound("incident"))?
            } else {
                current.clone()
            };
            let entry = AuditEntry::new(
                OperatorAction::IncidentResolved,
                AuditTargetKind::Incident,
                id,
            )
            .scope(
                current.tenant_id,
                current.application_id,
                current.environment_id,
            )
            .detail(serde_json::json!({
                "category": current.category,
                "previousStatus": current.status,
                "occurrences": current.occurrences,
            }));
            Ok((view, entry))
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ApplicationId, EnvironmentId, TenantId};

    #[test]
    fn dedupe_keys_are_bounded_and_stable() {
        let scope = CallScope::from_parts(
            TenantId::from_uuid(Uuid::new_v4()),
            ApplicationId::from_uuid(Uuid::new_v4()),
            EnvironmentId::from_uuid(Uuid::new_v4()),
        );
        let id = Uuid::new_v4();
        let a = Incident::new(
            IncidentCategory::ForwardingLoopRepeated,
            IncidentSeverity::Medium,
            scope,
            IncidentResource::Destination,
            id,
        );
        let b = a.clone().call(Uuid::new_v4());
        assert_eq!(a.dedupe_key(), b.dedupe_key(), "one incident per resource");
        let key = a.dedupe_key();
        assert!(key.len() <= 160 && key.starts_with("forwarding_loop_repeated:destination:"));
        assert!(key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ":_-".contains(c)));
    }
}
