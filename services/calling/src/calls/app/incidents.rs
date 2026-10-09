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
    }
);

/// One observation.
#[derive(Debug, Clone)]
pub struct Incident {
    pub category: IncidentCategory,
    pub severity: IncidentSeverity,
    pub scope: CallScope,
    pub call_id: Option<Uuid>,
    pub resource: Option<(IncidentResource, Uuid)>,
    pub evidence: Value,
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
            scope,
            call_id: None,
            resource: Some((resource, id)),
            evidence: Value::Object(Default::default()),
        }
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
        match self.resource {
            Some((kind, id)) => format!(
                "{category}:{}:{}",
                kind.as_str().to_ascii_lowercase(),
                id.simple()
            ),
            None => format!("{category}:{}", self.call_id.unwrap_or_default().simple()),
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
            incident.scope.tenant_id().as_uuid().into(),
            incident.scope.application_id().as_uuid().into(),
            incident.scope.environment_id().as_uuid().into(),
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
