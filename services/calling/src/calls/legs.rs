//! Canonical Calling-side call legs (Part C §16).
//!
//! Calling owns the provider-independent leg: id, parent Call, optional route and
//! participant, direction, neutral kind, normalized state and timing. It never stores
//! provider / carrier identifiers, provider status values, provider payloads or
//! telephone numbers. A later execution boundary (Telephony) owns provider bindings,
//! SIP dialogs, callbacks and reconciliation in its own database and maps them to a
//! `CallLegId`; Calling keeps at most an opaque `execution_binding_ref` (a UUID).

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::{
    ids::{CallId, CallLegId, CallParticipantId, CallRouteId},
    kinds::{FailureCategory, LegDirection, LegKind, LegState},
};
use crate::models::call_legs;

/// A leg to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLeg {
    pub route_id: Option<CallRouteId>,
    pub participant_id: Option<CallParticipantId>,
    pub direction: LegDirection,
    pub kind: LegKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallLeg {
    pub id: CallLegId,
    pub call_id: CallId,
    pub route_id: Option<CallRouteId>,
    pub participant_id: Option<CallParticipantId>,
    pub direction: LegDirection,
    pub kind: LegKind,
    pub state: LegState,
    pub failure_category: Option<FailureCategory>,
    /// Opaque execution binding reference (never exposed publicly).
    #[serde(skip)]
    pub execution_binding_ref: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub dialing_at: Option<DateTime<Utc>>,
    pub ringing_at: Option<DateTime<Utc>>,
    pub answered_at: Option<DateTime<Utc>>,
    pub connected_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl From<call_legs::Model> for CallLeg {
    fn from(m: call_legs::Model) -> Self {
        Self {
            id: CallLegId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            route_id: m.route_id.map(CallRouteId::from_uuid),
            participant_id: m.participant_id.map(CallParticipantId::from_uuid),
            direction: m.direction,
            kind: m.leg_kind,
            state: m.state,
            failure_category: m.failure_category,
            execution_binding_ref: m.execution_binding_ref,
            created_at: m.created_at,
            dialing_at: m.dialing_at,
            ringing_at: m.ringing_at,
            answered_at: m.answered_at,
            connected_at: m.connected_at,
            ended_at: m.ended_at,
        }
    }
}
