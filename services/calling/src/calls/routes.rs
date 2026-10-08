//! Call routes: the provider-independent logical routing decision (Part C §16).
//!
//! Domain foundation only: the entity, its ownership, states and a bounded decision
//! snapshot. Route *selection* belongs to the later routing engine; no algorithm, carrier
//! choice or provider business logic lives here.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{
    attributes::SafeAttributes,
    errors::CallError,
    ids::{CallId, CallRouteId},
    kinds::{RouteClass, RouteState},
};
use crate::models::call_routes;

/// Version label of the policy a decision was made under (`^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PolicyVersion(String);

impl PolicyVersion {
    pub fn parse(value: &str) -> Result<Self, CallError> {
        let bytes = value.as_bytes();
        let ok = !bytes.is_empty()
            && bytes.len() <= 64
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if ok {
            Ok(Self(value.to_string()))
        } else {
            Err(CallError::Validation("invalid policy version"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A routing decision to record.
#[derive(Debug, Clone)]
pub struct NewRoute {
    pub route_class: RouteClass,
    pub policy_version: PolicyVersion,
    pub decision_snapshot: SafeAttributes,
    /// The Routing Engine's provider-independent plan (Part E). Without one, an
    /// APPLICATION route is recorded as APP_RTC.
    pub plan: Option<crate::routing::engine::RoutePlan>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallRoute {
    pub id: CallRouteId,
    pub call_id: CallId,
    pub route_class: RouteClass,
    pub state: RouteState,
    pub policy_version: String,
    pub selected_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_class: Option<crate::routing::RoutingClass>,
}

impl From<call_routes::Model> for CallRoute {
    fn from(m: call_routes::Model) -> Self {
        Self {
            id: CallRouteId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            route_class: m.route_class,
            state: m.state,
            policy_version: m.policy_version,
            selected_at: m.selected_at,
            ended_at: m.ended_at,
            routing_class: m.routing_class,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_versions_are_bounded_labels() {
        for ok in ["v1", "2031-01.policy_3", "A"] {
            assert!(PolicyVersion::parse(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-v1", "has space", &"v".repeat(65), "+2348012345678"] {
            assert!(PolicyVersion::parse(bad).is_err(), "{bad}");
        }
    }
}
