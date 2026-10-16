//! Typed routing policy (§37 / §38). Never arbitrary route JSON: every field is closed and
//! validated, unknown fields are refused, and the default policy is the restrictive one
//! (application RTC only, no paid route).

use serde::{Deserialize, Serialize};

use super::RoutingClass;

/// What the router does when an earlier paid attempt has an unresolved outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DuplicateRiskPolicy {
    /// No new paid attempt toward the same destination until the earlier outcome is
    /// reconciled (the only accepted value today: duplicate paid calls are never risked).
    BlockUntilReconciled,
}

/// ISO country decision of a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CountryDecision {
    Allow,
    Deny,
}

/// Destination policy by ISO 3166-1 alpha-2 country. `deny` always wins; a country in
/// neither list gets `default`. Premium / high-risk number categories plug in later (no
/// category data source exists, so none is invented).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationPolicy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    pub default: CountryDecision,
}

impl DestinationPolicy {
    pub fn decide(&self, country: &str) -> CountryDecision {
        if self.deny.iter().any(|c| c == country) {
            CountryDecision::Deny
        } else if self.allow.iter().any(|c| c == country) {
            CountryDecision::Allow
        } else {
            self.default
        }
    }
}

/// The routing policy of one application environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingPolicy {
    /// Route classes this environment may use at all.
    pub allowed_route_classes: Vec<RoutingClass>,
    /// Application RTC wins over every paid route whenever all required participants can
    /// receive it.
    pub app_first: bool,
    /// The explicit hook letting the customer request a public route although the
    /// participants are reachable in the application (off unless policy grants it).
    pub customer_may_select_public_route: bool,
    pub public_calling_enabled: bool,
    pub callback_relay_enabled: bool,
    /// Every protected telephone participant must be (effectively) VERIFIED.
    pub require_verified_contacts: bool,
    pub destinations: DestinationPolicy,
    /// Additional attempts after a DEFINITE failure before connection (0–3).
    pub max_failover_attempts: u8,
    /// Upper bound on the estimated cost of one public leg, in minor units of the
    /// tenant's billing currency; None = no per-call bound beyond the spend limits.
    #[serde(default)]
    pub max_estimated_leg_cost_minor: Option<i64>,
    pub duplicate_risk: DuplicateRiskPolicy,
    /// Part H §78: destination countries (ISO 3166-1 alpha-2, or `*` for all) whose paid
    /// routes require an operator-recorded compliance eligibility. Unioned with the
    /// platform overlay (`CALLING_COMPLIANCE_REQUIRED_COUNTRIES`), which it cannot remove.
    #[serde(default)]
    pub compliance_required_countries: Vec<String>,
}

/// Version label of the policy model (recorded on every route).
pub const ROUTING_POLICY_VERSION: &str = "ROUTING_POLICY_V1";
pub const MAX_FAILOVER_ATTEMPTS: u8 = 3;

impl Default for RoutingPolicy {
    /// Restrictive default: application RTC and internal-only enterprise calls
    /// (ENTERPRISE_INTERNAL: no public leg, no spend). Every paid class needs an explicit
    /// policy.
    fn default() -> Self {
        Self {
            allowed_route_classes: vec![RoutingClass::AppRtc, RoutingClass::EnterpriseInternal],
            app_first: true,
            customer_may_select_public_route: false,
            public_calling_enabled: false,
            callback_relay_enabled: false,
            require_verified_contacts: true,
            destinations: DestinationPolicy {
                allow: vec![],
                deny: vec![],
                default: CountryDecision::Deny,
            },
            max_failover_attempts: 0,
            max_estimated_leg_cost_minor: None,
            duplicate_risk: DuplicateRiskPolicy::BlockUntilReconciled,
            compliance_required_countries: vec![],
        }
    }
}

fn iso_country(c: &str) -> bool {
    c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase())
}

impl RoutingPolicy {
    /// Parses and validates a stored / administered policy document.
    pub fn parse(value: &serde_json::Value) -> Result<Self, &'static str> {
        let policy: Self =
            serde_json::from_value(value.clone()).map_err(|_| "the routing policy is malformed")?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.allowed_route_classes.is_empty() {
            return Err("allowedRouteClasses must not be empty");
        }
        let mut classes = self.allowed_route_classes.clone();
        classes.sort();
        classes.dedup();
        if classes.len() != self.allowed_route_classes.len() {
            return Err("allowedRouteClasses must be distinct");
        }
        let countries = self
            .destinations
            .allow
            .iter()
            .chain(&self.destinations.deny);
        let mut count = 0;
        for c in countries {
            count += 1;
            if !iso_country(c) {
                return Err("destination countries must be ISO 3166-1 alpha-2 codes");
            }
        }
        if count > 300 {
            return Err("too many destination countries");
        }
        if self.compliance_required_countries.len() > 300 {
            return Err("too many compliance-required countries");
        }
        if self
            .compliance_required_countries
            .iter()
            .any(|c| c != "*" && !iso_country(c))
        {
            return Err("complianceRequiredCountries must be ISO 3166-1 alpha-2 codes or *");
        }
        if self.max_failover_attempts > MAX_FAILOVER_ATTEMPTS {
            return Err("maxFailoverAttempts must be between 0 and 3");
        }
        if self.max_estimated_leg_cost_minor.is_some_and(|c| c < 0) {
            return Err("maxEstimatedLegCostMinor must not be negative");
        }
        if self.callback_relay_enabled
            && !self
                .allowed_route_classes
                .contains(&RoutingClass::CallbackRelay)
        {
            return Err("callbackRelayEnabled requires CALLBACK_RELAY in allowedRouteClasses");
        }
        Ok(())
    }

    /// Whether the customer policy marks `country` as compliance-required.
    pub fn requires_compliance(&self, country: &str) -> bool {
        self.compliance_required_countries
            .iter()
            .any(|c| c == "*" || c == country)
    }

    pub fn allows(&self, class: RoutingClass) -> bool {
        self.allowed_route_classes.contains(&class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_default_policy_is_restrictive_and_valid() {
        let p = RoutingPolicy::default();
        p.validate().unwrap();
        assert!(!p.public_calling_enabled && !p.callback_relay_enabled);
        assert_eq!(p.destinations.decide("NG"), CountryDecision::Deny);
    }

    #[test]
    fn arbitrary_route_json_is_refused() {
        let mut v = serde_json::to_value(RoutingPolicy::default()).unwrap();
        v["carrier"] = json!("anything");
        assert!(RoutingPolicy::parse(&v).is_err());
        let mut v = serde_json::to_value(RoutingPolicy::default()).unwrap();
        v["allowedRouteClasses"] = json!(["SIP_DIRECT"]);
        assert!(RoutingPolicy::parse(&v).is_err());
        let mut v = serde_json::to_value(RoutingPolicy::default()).unwrap();
        v["destinations"]["allow"] = json!(["Nigeria"]);
        assert!(RoutingPolicy::parse(&v).is_err());
        let mut v = serde_json::to_value(RoutingPolicy::default()).unwrap();
        v["maxFailoverAttempts"] = json!(9);
        assert!(RoutingPolicy::parse(&v).is_err());
    }

    #[test]
    fn deny_wins_over_allow() {
        let d = DestinationPolicy {
            allow: vec!["NG".into(), "GH".into()],
            deny: vec!["GH".into()],
            default: CountryDecision::Deny,
        };
        assert_eq!(d.decide("NG"), CountryDecision::Allow);
        assert_eq!(d.decide("GH"), CountryDecision::Deny);
        assert_eq!(d.decide("KE"), CountryDecision::Deny);
    }
}
