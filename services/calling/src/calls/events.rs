//! Immutable call events and their publication form.
//!
//! Every call-domain mutation appends one or more [`CallEvent`]s in the same transaction
//! as the mutation, and each event is mirrored by one publication-ready outbox row
//! holding a `cfmeet.platform.event-envelope` whose payload is a
//! `cfmeet.calling.call-lifecycle-event` (v1). The payload contains provider-independent,
//! safe fields only: ids, scope, sequence, versions, states, time, correlation and a
//! bounded data object — never provider ids, telephone numbers, credentials or tokens.
//!
//! Event types are exactly the facts this domain produces today; envelope event types are
//! derived deterministically (`CALL_STATE_CHANGED` → `calling.call-state-changed`).

use cf_service_auth::RuntimeEnvironment;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use super::{
    command::CallScope,
    ids::{CallEventId, CallId},
    kinds::ActorKind,
    state::CallState,
};
use crate::{
    domain::{ApplicationId, EnvironmentId, TenantId},
    internal_auth::calling_service_id,
    models::call_events,
};

/// Payload contract version of every call event.
pub const CALL_EVENT_SCHEMA_VERSION: i32 = 1;

text_enum!(
    /// Facts the call domain records.
    CallEventType {
        CallRequested = "CALL_REQUESTED",
        CallAuthorizationRecorded = "CALL_AUTHORIZATION_RECORDED",
        CallStateChanged = "CALL_STATE_CHANGED",
        CallParticipantAdded = "CALL_PARTICIPANT_ADDED",
        CallParticipantUpdated = "CALL_PARTICIPANT_UPDATED",
        CallParticipantRemoved = "CALL_PARTICIPANT_REMOVED",
        CallSessionCreated = "CALL_SESSION_CREATED",
        CallSessionConnected = "CALL_SESSION_CONNECTED",
        CallSessionDisconnected = "CALL_SESSION_DISCONNECTED",
        CallSessionReconnected = "CALL_SESSION_RECONNECTED",
        CallSessionEnded = "CALL_SESSION_ENDED",
        CallRouteRecorded = "CALL_ROUTE_RECORDED",
        CallRouteStateChanged = "CALL_ROUTE_STATE_CHANGED",
        CallLegRecorded = "CALL_LEG_RECORDED",
        CallLegStateChanged = "CALL_LEG_STATE_CHANGED",
        CallLegBindingAssigned = "CALL_LEG_BINDING_ASSIGNED",
        CallAttemptRecorded = "CALL_ATTEMPT_RECORDED",
        CallAttemptFinished = "CALL_ATTEMPT_FINISHED",
        CallQualityRecorded = "CALL_QUALITY_RECORDED",
        CallUsageRecorded = "CALL_USAGE_RECORDED",
        CallUsageFinalized = "CALL_USAGE_FINALIZED",
        CallInvitationIssued = "CALL_INVITATION_ISSUED",
        CallInvitationAccepted = "CALL_INVITATION_ACCEPTED",
        CallInvitationDeclined = "CALL_INVITATION_DECLINED",
        CallInvitationAnsweredElsewhere = "CALL_INVITATION_ANSWERED_ELSEWHERE",
        CallInvitationCancelled = "CALL_INVITATION_CANCELLED",
        CallInvitationExpired = "CALL_INVITATION_EXPIRED",
        CallTransferRequested = "CALL_TRANSFER_REQUESTED",
        CallTransferCompleted = "CALL_TRANSFER_COMPLETED",
        CallTransferFailed = "CALL_TRANSFER_FAILED",
        CallTransferCancelled = "CALL_TRANSFER_CANCELLED",
        CallMediaSessionCreated = "CALL_MEDIA_SESSION_CREATED",
        CallMediaSessionTerminated = "CALL_MEDIA_SESSION_TERMINATED",
        CallMediaPermissionsUpdated = "CALL_MEDIA_PERMISSIONS_UPDATED",
        CallMediaCredentialIssued = "CALL_MEDIA_CREDENTIAL_ISSUED",
        CallMediaParticipantEvicted = "CALL_MEDIA_PARTICIPANT_EVICTED",
        CallMediaEnforcementPending = "CALL_MEDIA_ENFORCEMENT_PENDING",
        CallMediaReconciled = "CALL_MEDIA_RECONCILED",
        VoicemailReceived = "VOICEMAIL_RECEIVED",
        VoicemailReady = "VOICEMAIL_READY",
        VoicemailDeleted = "VOICEMAIL_DELETED",
    }
);

impl CallEventType {
    /// Envelope `event_type`: `calling.` + the lower-case, hyphenated domain type.
    pub fn envelope_type(&self) -> String {
        format!(
            "calling.{}",
            self.as_str().to_ascii_lowercase().replace('_', "-")
        )
    }

    /// Whether events of this type change the Call's state (and version).
    pub fn is_lifecycle(&self) -> bool {
        matches!(
            self,
            CallEventType::CallRequested | CallEventType::CallStateChanged
        )
    }
}

/// An immutable call event, as recorded.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallEvent {
    pub event_id: CallEventId,
    pub call_id: CallId,
    #[serde(skip)]
    pub scope: CallScope,
    pub sequence: i64,
    pub event_type: CallEventType,
    pub schema_version: i32,
    pub from_state: Option<CallState>,
    pub to_state: Option<CallState>,
    pub call_version: i64,
    pub actor_kind: ActorKind,
    pub request_id: String,
    pub correlation_id: String,
    pub data: Value,
    pub occurred_at: DateTime<Utc>,
}

impl From<call_events::Model> for CallEvent {
    fn from(m: call_events::Model) -> Self {
        Self {
            event_id: CallEventId::from_uuid(m.event_id),
            call_id: CallId::from_uuid(m.call_id),
            scope: CallScope::from_parts(
                TenantId::from_uuid(m.tenant_id),
                ApplicationId::from_uuid(m.application_id),
                EnvironmentId::from_uuid(m.environment_id),
            ),
            sequence: m.sequence,
            event_type: m.event_type,
            schema_version: m.schema_version,
            from_state: m.from_state,
            to_state: m.to_state,
            call_version: m.call_version,
            actor_kind: m.actor_kind,
            request_id: m.request_id,
            correlation_id: m.correlation_id,
            data: m.payload,
            occurred_at: m.occurred_at,
        }
    }
}

fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

impl CallEvent {
    /// `cfmeet.calling.call-lifecycle-event` v1 payload.
    pub fn lifecycle_payload(&self) -> Value {
        json!({
            "event_id": self.event_id.envelope_id(),
            "event_type": self.event_type.as_str(),
            "call_id": self.call_id,
            "tenant_id": self.scope.tenant_id(),
            "application_id": self.scope.application_id(),
            "application_environment_id": self.scope.environment_id(),
            "sequence": self.sequence,
            "call_version": self.call_version,
            "from_state": self.from_state.map(|s| s.as_str()),
            "to_state": self.to_state.map(|s| s.as_str()),
            "occurred_at": timestamp(self.occurred_at),
            "correlation_id": self.correlation_id,
            "data": self.data,
        })
    }

    /// `cfmeet.platform.event-envelope` v1 carrying the lifecycle payload.
    pub fn envelope(&self, runtime_environment: RuntimeEnvironment) -> Value {
        json!({
            "event_id": self.event_id.envelope_id(),
            "event_type": self.event_type.envelope_type(),
            "schema_version": self.schema_version,
            "occurred_at": timestamp(self.occurred_at),
            "producer_service": calling_service_id().as_str(),
            "runtime_environment": runtime_environment.as_str(),
            "correlation_id": self.correlation_id,
            "ownership": {
                "tenant_id": self.scope.tenant_id(),
                "application_id": self.scope.application_id(),
                "application_environment_id": self.scope.environment_id(),
            },
            "payload": self.lifecycle_payload(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_types_are_derived_deterministically() {
        assert_eq!(
            CallEventType::CallStateChanged.envelope_type(),
            "calling.call-state-changed"
        );
        assert_eq!(
            CallEventType::CallRequested.envelope_type(),
            "calling.call-requested"
        );
        let mut seen: Vec<String> = CallEventType::ALL
            .iter()
            .map(|t| t.envelope_type())
            .collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), CallEventType::ALL.len());
    }

    #[test]
    fn no_future_domain_events_exist() {
        for t in CallEventType::ALL {
            for future in [
                "FAILOVER",
                "RELAY_NUMBER",
                "PROVIDER",
                "CARRIER",
                "LIVEKIT",
                "SIP",
                "PSTN",
            ] {
                assert!(!t.as_str().contains(future), "{t}");
            }
        }
    }

    /// Part F: voicemail facts are the only artifact facts of a Call and never carry a
    /// provider / edge vocabulary.
    #[test]
    fn part_f_artifact_events_are_explicit() {
        let artifact: Vec<&str> = CallEventType::ALL
            .iter()
            .map(|t| t.as_str())
            .filter(|t| t.starts_with("VOICEMAIL_") || t.starts_with("RECORDING_"))
            .collect();
        assert_eq!(
            artifact,
            ["VOICEMAIL_RECEIVED", "VOICEMAIL_READY", "VOICEMAIL_DELETED"]
        );
    }
}
