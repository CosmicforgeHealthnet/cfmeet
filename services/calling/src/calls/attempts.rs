//! Call attempts (Part C §16): numbered per Call (gap-free, never duplicated), linked to
//! the previous attempt for future retry / failover evidence, with an explicit unknown
//! outcome. No retry or failover policy is implemented here.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{
    ids::{CallAttemptId, CallId, CallLegId, CallRouteId},
    kinds::{AttemptOutcome, FailureCategory},
};
use crate::models::call_attempts;

/// An attempt to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NewAttempt {
    pub route_id: Option<CallRouteId>,
    pub leg_id: Option<CallLegId>,
    /// The attempt this one follows (same Call).
    pub previous_attempt_id: Option<CallAttemptId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallAttempt {
    pub id: CallAttemptId,
    pub call_id: CallId,
    pub route_id: Option<CallRouteId>,
    pub leg_id: Option<CallLegId>,
    pub attempt_number: i32,
    pub previous_attempt_id: Option<CallAttemptId>,
    pub outcome: AttemptOutcome,
    /// Whether the outcome was (or still is) unknown at some point.
    pub unknown_outcome: bool,
    pub failure_category: Option<FailureCategory>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl From<call_attempts::Model> for CallAttempt {
    fn from(m: call_attempts::Model) -> Self {
        Self {
            id: CallAttemptId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            route_id: m.route_id.map(CallRouteId::from_uuid),
            leg_id: m.leg_id.map(CallLegId::from_uuid),
            attempt_number: m.attempt_number,
            previous_attempt_id: m.previous_attempt_id.map(CallAttemptId::from_uuid),
            outcome: m.outcome,
            unknown_outcome: m.unknown_outcome,
            failure_category: m.failure_category,
            started_at: m.started_at,
            finished_at: m.finished_at,
        }
    }
}
