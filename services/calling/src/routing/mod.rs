//! CF Meet Routing Engine (Part E §37–§38).
//!
//! CosmicForge-owned, provider-independent route selection inside Calling (Calling already
//! owns the canonical CallRoute; there is no separate routing service). The engine:
//!
//! * classifies a communication into one of six canonical route classes;
//! * applies the typed [`policy::RoutingPolicy`] of the application environment
//!   (app-first, allowed classes, destination countries, verified contacts, telecom
//!   enablement, failover, cost bound, duplicate risk);
//! * consumes the §38 inputs as facts or explicit `Unknown` evidence
//!   ([`engine::Evidence`]), never inventing data;
//! * produces a [`engine::RoutePlan`] — no telephone number, no provider name — whose
//!   fingerprint makes re-selection idempotent;
//! * decides revisions before CONNECTED only, never failing over an UNKNOWN outcome.
//!
//! Route health and capacity come from Telephony's normalized internal API through the
//! [`availability::RouteAvailabilitySource`] port; Calling never reads Telephony data.

pub mod availability;
pub mod engine;
pub mod policy;
pub mod store;

use crate::calls::kinds::RouteClass;

text_enum!(
    /// The canonical route classes. No provider names.
    RoutingClass {
        AppRtc = "APP_RTC",
        AppToPstn = "APP_TO_PSTN",
        PstnToApp = "PSTN_TO_APP",
        PstnRelay = "PSTN_RELAY",
        CallbackRelay = "CALLBACK_RELAY",
        EnterpriseInternal = "ENTERPRISE_INTERNAL",
    }
);

impl RoutingClass {
    /// The neutral network kind recorded in `call_routes.route_class` (Part C).
    pub fn network_class(&self) -> RouteClass {
        match self {
            RoutingClass::AppRtc => RouteClass::Application,
            RoutingClass::AppToPstn | RoutingClass::PstnToApp => RouteClass::Mixed,
            RoutingClass::PstnRelay | RoutingClass::CallbackRelay => RouteClass::PublicNetwork,
            RoutingClass::EnterpriseInternal => RouteClass::PrivateNetwork,
        }
    }

    /// Whether the class uses paid public-network legs.
    pub fn is_paid(&self) -> bool {
        !matches!(
            self,
            RoutingClass::AppRtc | RoutingClass::EnterpriseInternal
        )
    }

    /// Whether CF Meet executes the class. Every class is executable since Part F made
    /// ENTERPRISE_INTERNAL (internal-only enterprise calls, no public leg) executable.
    pub fn is_executable(&self) -> bool {
        true
    }

    /// The Privacy Relay communication route that authorizes this class, if any.
    pub fn privacy_route(&self) -> Option<&'static str> {
        match self {
            RoutingClass::AppRtc => Some("APPLICATION"),
            RoutingClass::AppToPstn | RoutingClass::PstnToApp | RoutingClass::PstnRelay => {
                Some("PUBLIC_RELAY")
            }
            RoutingClass::CallbackRelay => Some("CALLBACK_RELAY"),
            RoutingClass::EnterpriseInternal => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_classes_exist_and_map_to_network_kinds() {
        assert_eq!(RoutingClass::ALL.len(), 6);
        assert!(!RoutingClass::AppRtc.is_paid());
        assert!(RoutingClass::CallbackRelay.is_paid());
        assert_eq!(RoutingClass::AppToPstn.network_class(), RouteClass::Mixed);
        for c in RoutingClass::ALL {
            let s = c.as_str().to_lowercase();
            for vendor in ["livekit", "africa", "twilio", "asterisk", "sip"] {
                assert!(!s.contains(vendor));
            }
        }
    }
}
