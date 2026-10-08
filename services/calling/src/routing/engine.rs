//! Route selection (§37–§38): a PURE function of typed, provider-independent inputs.
//!
//! Every input that has no evidence is `Evidence::Unknown` — never invented as a fact.
//! Unknown inputs that a paid route depends on for SAFETY (contact verification,
//! destination country, duplicate risk) deny; unknown inputs that only refine the choice
//! (route health, quality, cost) are recorded as unknown and leave the final execution
//! checks to Telephony and the spend reservation.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    policy::{CountryDecision, DuplicateRiskPolicy, RoutingPolicy, ROUTING_POLICY_VERSION},
    RoutingClass,
};

/// A fact or the explicit absence of evidence for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "evidence",
    content = "value",
    rename_all = "SCREAMING_SNAKE_CASE"
)]
pub enum Evidence<T> {
    Known(T),
    Unknown,
}

impl<T> Evidence<T> {
    pub fn known(&self) -> Option<&T> {
        match self {
            Evidence::Known(v) => Some(v),
            Evidence::Unknown => None,
        }
    }
}

text_enum!(
    /// Neutral class of an endpoint taking part in a route.
    EndpointClass {
        Application = "APPLICATION",
        PublicTelephone = "PUBLIC_TELEPHONE",
        Enterprise = "ENTERPRISE",
    }
);

text_enum!(
    /// Direction of the communication relative to CF Meet.
    RouteDirection {
        Outbound = "OUTBOUND",
        Inbound = "INBOUND",
    }
);

text_enum!(
    /// Verification of a protected contact as Privacy Relay reported it.
    ContactVerification {
        Verified = "VERIFIED",
        NotVerified = "NOT_VERIFIED",
        Expired = "EXPIRED",
        Blocked = "BLOCKED",
    }
);

text_enum!(
    /// Normalized route health (Telephony's view, never a provider status).
    RouteHealth {
        Healthy = "HEALTHY",
        Degraded = "DEGRADED",
        Unavailable = "UNAVAILABLE",
        Unknown = "UNKNOWN",
    }
);

text_enum!(
    /// Why a route was not selected.
    RouteDenial {
        NotAuthorized = "NOT_AUTHORIZED",
        RouteClassNotAllowed = "ROUTE_CLASS_NOT_ALLOWED",
        PublicCallingDisabled = "PUBLIC_CALLING_DISABLED",
        CallbackRelayDisabled = "CALLBACK_RELAY_DISABLED",
        TelecomFeatureDisabled = "TELECOM_FEATURE_DISABLED",
        LiveTelecomNotAllowed = "LIVE_TELECOM_NOT_ALLOWED",
        DestinationCountryDenied = "DESTINATION_COUNTRY_DENIED",
        DestinationCountryUnknown = "DESTINATION_COUNTRY_UNKNOWN",
        ContactNotVerified = "CONTACT_NOT_VERIFIED",
        TargetUnreachable = "TARGET_UNREACHABLE",
        RouteUnavailable = "ROUTE_UNAVAILABLE",
        CapacityExhausted = "CAPACITY_EXHAUSTED",
        CostLimitExceeded = "COST_LIMIT_EXCEEDED",
        SpendLimitExceeded = "SPEND_LIMIT_EXCEEDED",
        DuplicateRiskUnresolved = "DUPLICATE_RISK_UNRESOLVED",
        NotExecutable = "NOT_EXECUTABLE",
        UnsupportedShape = "UNSUPPORTED_SHAPE",
    }
);

text_enum!(
    /// Voice capability a public route must provide.
    RouteCapability {
        VoiceOutbound = "VOICE_OUTBOUND",
        VoiceInbound = "VOICE_INBOUND",
    }
);

/// A protected telephone contact taking part (facts from Privacy Relay; never a number).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactFacts {
    pub contact_reference: Uuid,
    pub verification: Evidence<ContactVerification>,
    pub country: Evidence<String>,
}

/// One destination participant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationFacts {
    pub class: EndpointClass,
    /// Whether the participant can receive application RTC now (eligible endpoints).
    pub app_reachable: Evidence<bool>,
    /// The participant's protected telephone contact, when the call may reach one.
    pub contact: Option<ContactFacts>,
}

impl DestinationFacts {
    /// A protected public-network contact (no application endpoint).
    pub fn protected_contact(contact: ContactFacts) -> Self {
        Self {
            class: EndpointClass::PublicTelephone,
            app_reachable: Evidence::Known(false),
            contact: Some(contact),
        }
    }
}

/// Normalized availability of one candidate Telephony route (opaque reference).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateRoute {
    pub route_ref: String,
    pub health: RouteHealth,
    pub capacity_available: Evidence<bool>,
    /// Conservative estimated cost of one leg (minor units), when Telephony knows it.
    pub estimated_leg_cost_minor: Evidence<i64>,
    /// Recent answer-seizure ratio in permille, when Telephony has evidence.
    pub quality_permille: Evidence<u16>,
}

/// The outcome of earlier paid attempts toward the same destination of this Call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateRisk {
    /// No earlier paid attempt, or every earlier one is definitely finished.
    None,
    /// An earlier paid attempt is still in flight or its outcome is UNKNOWN.
    Unresolved,
}

/// Telecom feature flags (Calling / Telephony configuration).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TelecomFlags {
    pub public_outbound: bool,
    pub public_inbound: bool,
    pub callback_relay: bool,
}

/// Everything §38 lists, as facts or explicit unknowns.
#[derive(Debug, Clone)]
pub struct RouteInputs {
    pub direction: RouteDirection,
    pub source: EndpointClass,
    /// The source participant's own protected contact (callback relay first leg).
    pub source_contact: Option<ContactFacts>,
    pub destinations: Vec<DestinationFacts>,
    /// An explicit request (callback relay command, or a public route under the policy
    /// hook). Never a way around app-first.
    pub requested: Option<RoutingClass>,
    /// Authorization result: whether the call is authorized and which classes the
    /// authorizing decision covers.
    pub authorized: bool,
    pub permitted_classes: Vec<RoutingClass>,
    pub relay_session_id: Option<Uuid>,
    pub context_id: Option<Uuid>,
    /// The application environment may produce real telecom effects.
    pub live_telecom_allowed: bool,
    pub flags: TelecomFlags,
    pub routes: Evidence<Vec<CandidateRoute>>,
    /// Remaining telecom spend headroom (minor units), when limits are configured.
    pub spend_headroom_minor: Evidence<i64>,
    /// An active operational incident affecting public routes.
    pub operational_incident: Evidence<bool>,
    pub duplicate_risk: DuplicateRisk,
}

/// Failover rules carried by a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailoverPolicy {
    /// Total attempts allowed (first attempt + failovers after DEFINITE failures).
    pub max_attempts: u8,
    pub on_definite_failure: bool,
    pub duplicate_risk: DuplicateRiskPolicy,
}

/// The provider-independent route plan (§17). No telephone number, no provider name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutePlan {
    pub route_class: RoutingClass,
    pub policy_version: String,
    pub source_endpoint: EndpointClass,
    pub destination_endpoint: EndpointClass,
    pub public_network_required: bool,
    pub required_capabilities: Vec<RouteCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_country: Option<String>,
    pub protected_contact_refs: Vec<Uuid>,
    pub verified_contacts_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_session_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<Uuid>,
    /// Ordered candidate Telephony route references (opaque; best first).
    pub candidate_routes: Vec<String>,
    pub failover: FailoverPolicy,
    /// Inputs that had no evidence (recorded, never invented).
    pub unknown_inputs: Vec<String>,
    pub selected_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl RoutePlan {
    /// Stable fingerprint of the decision-relevant content (not its timestamps): the
    /// same authoritative inputs always give the same fingerprint, so recomputing a
    /// selection never creates a second active route.
    pub fn fingerprint(&self) -> String {
        let mut copy = self.clone();
        copy.selected_at = DateTime::<Utc>::UNIX_EPOCH;
        copy.expires_at = None;
        let canonical = serde_json::to_vec(&copy).unwrap_or_default();
        hex::encode(Sha256::digest(canonical))
    }
}

/// Result of a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    Selected(Box<RoutePlan>),
    Denied(RouteDenial),
}

impl RouteDecision {
    pub fn plan(&self) -> Option<&RoutePlan> {
        match self {
            RouteDecision::Selected(p) => Some(p),
            RouteDecision::Denied(_) => None,
        }
    }

    pub fn denial(&self) -> Option<RouteDenial> {
        match self {
            RouteDecision::Selected(_) => None,
            RouteDecision::Denied(d) => Some(*d),
        }
    }
}

/// How long a paid route selection stays valid before it must be recomputed.
pub const PAID_PLAN_VALIDITY_SECONDS: i64 = 120;

/// Which class the shape of the communication calls for (before policy gates).
fn classify(policy: &RoutingPolicy, inputs: &RouteInputs) -> Result<RoutingClass, RouteDenial> {
    let dests = &inputs.destinations;
    if dests.is_empty() {
        return Err(RouteDenial::UnsupportedShape);
    }
    let app_ok = |d: &DestinationFacts| matches!(d.app_reachable, Evidence::Known(true));
    match (inputs.direction, inputs.source) {
        (RouteDirection::Inbound, EndpointClass::PublicTelephone) => {
            if dests.iter().all(|d| d.class == EndpointClass::Application) {
                Ok(RoutingClass::PstnToApp)
            } else if dests.len() == 1 && dests[0].class == EndpointClass::PublicTelephone {
                Ok(RoutingClass::PstnRelay)
            } else {
                Err(RouteDenial::UnsupportedShape)
            }
        }
        (RouteDirection::Inbound, _) => Err(RouteDenial::UnsupportedShape),
        (RouteDirection::Outbound, EndpointClass::Enterprise) => {
            if dests.iter().all(|d| d.class == EndpointClass::Enterprise) {
                Ok(RoutingClass::EnterpriseInternal)
            } else {
                Err(RouteDenial::UnsupportedShape)
            }
        }
        (RouteDirection::Outbound, EndpointClass::PublicTelephone) => {
            // Two outbound public legs: only an explicit callback-relay request.
            if inputs.requested == Some(RoutingClass::CallbackRelay)
                && dests.len() == 1
                && dests[0].contact.is_some()
                && inputs.source_contact.is_some()
            {
                Ok(RoutingClass::CallbackRelay)
            } else {
                Err(RouteDenial::UnsupportedShape)
            }
        }
        (RouteDirection::Outbound, EndpointClass::Application) => {
            if inputs.requested == Some(RoutingClass::CallbackRelay) {
                return Err(RouteDenial::UnsupportedShape);
            }
            // App-first: every destination reachable in the application wins over any
            // paid route — a customer request cannot bypass it unless policy allows.
            let all_app = dests.iter().all(app_ok);
            let single_contact = dests.len() == 1 && dests[0].contact.is_some();
            let public_requested = inputs.requested == Some(RoutingClass::AppToPstn)
                && policy.customer_may_select_public_route;
            let prefer_public = single_contact && (public_requested || !policy.app_first);
            if all_app && !prefer_public {
                Ok(RoutingClass::AppRtc)
            } else if single_contact {
                Ok(RoutingClass::AppToPstn)
            } else {
                Err(RouteDenial::TargetUnreachable)
            }
        }
    }
}

/// Selects the route for `inputs` under `policy` at `now`.
pub fn select(policy: &RoutingPolicy, inputs: &RouteInputs, now: DateTime<Utc>) -> RouteDecision {
    match select_inner(policy, inputs, now) {
        Ok(plan) => RouteDecision::Selected(Box::new(plan)),
        Err(denial) => RouteDecision::Denied(denial),
    }
}

fn select_inner(
    policy: &RoutingPolicy,
    inputs: &RouteInputs,
    now: DateTime<Utc>,
) -> Result<RoutePlan, RouteDenial> {
    if !inputs.authorized {
        return Err(RouteDenial::NotAuthorized);
    }
    let class = classify(policy, inputs)?;
    if !policy.allows(class) {
        return Err(RouteDenial::RouteClassNotAllowed);
    }
    if !class.is_executable() {
        return Err(RouteDenial::NotExecutable);
    }
    let mut unknown = Vec::new();
    let mut protected = Vec::new();
    let mut country = None;
    let paid = class.is_paid();
    if paid {
        if !inputs.live_telecom_allowed {
            return Err(RouteDenial::LiveTelecomNotAllowed);
        }
        if !policy.public_calling_enabled {
            return Err(RouteDenial::PublicCallingDisabled);
        }
        let flag = match class {
            RoutingClass::AppToPstn => inputs.flags.public_outbound,
            RoutingClass::PstnToApp => inputs.flags.public_inbound,
            RoutingClass::PstnRelay => inputs.flags.public_inbound && inputs.flags.public_outbound,
            RoutingClass::CallbackRelay => {
                if !policy.callback_relay_enabled {
                    return Err(RouteDenial::CallbackRelayDisabled);
                }
                inputs.flags.callback_relay && inputs.flags.public_outbound
            }
            _ => true,
        };
        if !flag {
            return Err(RouteDenial::TelecomFeatureDisabled);
        }
        // The authorizing decision must cover this class (Privacy Relay route).
        if !inputs.permitted_classes.contains(&class) {
            return Err(RouteDenial::NotAuthorized);
        }
        // Protected contacts: verification and destination country.
        let contacts: Vec<&ContactFacts> = inputs
            .source_contact
            .iter()
            .chain(
                inputs
                    .destinations
                    .iter()
                    .filter_map(|d| d.contact.as_ref()),
            )
            .collect();
        let outbound_contacts: Vec<&ContactFacts> = match class {
            RoutingClass::PstnToApp => vec![],
            RoutingClass::CallbackRelay => contacts.clone(),
            _ => inputs
                .destinations
                .iter()
                .filter_map(|d| d.contact.as_ref())
                .collect(),
        };
        for c in &contacts {
            protected.push(c.contact_reference);
            if policy.require_verified_contacts
                && c.verification != Evidence::Known(ContactVerification::Verified)
            {
                // An inbound caller is protected on arrival, not verified: only the
                // contacts CF Meet dials must be verified.
                if outbound_contacts.contains(c) {
                    return Err(RouteDenial::ContactNotVerified);
                }
            }
        }
        for c in &outbound_contacts {
            match &c.country {
                Evidence::Known(cc) => {
                    if policy.destinations.decide(cc) == CountryDecision::Deny {
                        return Err(RouteDenial::DestinationCountryDenied);
                    }
                    country.get_or_insert_with(|| cc.clone());
                }
                Evidence::Unknown => return Err(RouteDenial::DestinationCountryUnknown),
            }
        }
        if inputs.duplicate_risk == DuplicateRisk::Unresolved {
            return Err(RouteDenial::DuplicateRiskUnresolved);
        }
        if inputs.operational_incident == Evidence::Known(true) {
            return Err(RouteDenial::RouteUnavailable);
        }
        if inputs.operational_incident == Evidence::Unknown {
            unknown.push("operational_incident".to_string());
        }
    }
    // Candidate public routes (paid classes only): healthy first, then degraded, then
    // unknown health; never an unavailable or full route.
    let mut candidates: Vec<&CandidateRoute> = Vec::new();
    if paid {
        match &inputs.routes {
            Evidence::Unknown => unknown.push("route_health".to_string()),
            Evidence::Known(routes) => {
                let mut usable: Vec<&CandidateRoute> = routes
                    .iter()
                    .filter(|r| r.health != RouteHealth::Unavailable)
                    .filter(|r| r.capacity_available != Evidence::Known(false))
                    .collect();
                if usable.is_empty() {
                    return Err(
                        if routes.iter().any(|r| r.health != RouteHealth::Unavailable) {
                            RouteDenial::CapacityExhausted
                        } else {
                            RouteDenial::RouteUnavailable
                        },
                    );
                }
                let rank = |h: RouteHealth| match h {
                    RouteHealth::Healthy => 0,
                    RouteHealth::Degraded => 1,
                    RouteHealth::Unknown => 2,
                    RouteHealth::Unavailable => 3,
                };
                usable.sort_by(|a, b| {
                    rank(a.health)
                        .cmp(&rank(b.health))
                        .then_with(|| {
                            let q = |r: &CandidateRoute| {
                                r.quality_permille.known().copied().unwrap_or(0)
                            };
                            q(b).cmp(&q(a))
                        })
                        .then_with(|| a.route_ref.cmp(&b.route_ref))
                });
                // Cost: the per-leg bound of the policy and the spend headroom.
                if let Some(max) = policy.max_estimated_leg_cost_minor {
                    usable.retain(|r| match r.estimated_leg_cost_minor {
                        Evidence::Known(c) => c <= max,
                        Evidence::Unknown => true,
                    });
                    if usable.is_empty() {
                        return Err(RouteDenial::CostLimitExceeded);
                    }
                }
                if usable
                    .iter()
                    .any(|r| r.estimated_leg_cost_minor == Evidence::Unknown)
                {
                    unknown.push("estimated_cost".to_string());
                }
                if usable
                    .iter()
                    .any(|r| r.quality_permille == Evidence::Unknown)
                {
                    unknown.push("quality".to_string());
                }
                candidates = usable;
            }
        }
        let legs = if matches!(class, RoutingClass::CallbackRelay | RoutingClass::PstnRelay) {
            2
        } else {
            1
        };
        match inputs.spend_headroom_minor {
            Evidence::Known(headroom) => {
                let cheapest = candidates
                    .iter()
                    .filter_map(|r| r.estimated_leg_cost_minor.known().copied())
                    .min()
                    .unwrap_or(0);
                if headroom <= 0 || cheapest.saturating_mul(legs) > headroom {
                    return Err(RouteDenial::SpendLimitExceeded);
                }
            }
            Evidence::Unknown => unknown.push("spend_headroom".to_string()),
        }
    }
    unknown.sort();
    unknown.dedup();
    let (source_endpoint, destination_endpoint) = match class {
        RoutingClass::AppRtc => (EndpointClass::Application, EndpointClass::Application),
        RoutingClass::AppToPstn => (EndpointClass::Application, EndpointClass::PublicTelephone),
        RoutingClass::PstnToApp => (EndpointClass::PublicTelephone, EndpointClass::Application),
        RoutingClass::PstnRelay | RoutingClass::CallbackRelay => (
            EndpointClass::PublicTelephone,
            EndpointClass::PublicTelephone,
        ),
        RoutingClass::EnterpriseInternal => (EndpointClass::Enterprise, EndpointClass::Enterprise),
    };
    let required_capabilities = match class {
        RoutingClass::AppToPstn | RoutingClass::CallbackRelay => {
            vec![RouteCapability::VoiceOutbound]
        }
        RoutingClass::PstnToApp => vec![RouteCapability::VoiceInbound],
        RoutingClass::PstnRelay => vec![
            RouteCapability::VoiceOutbound,
            RouteCapability::VoiceInbound,
        ],
        _ => vec![],
    };
    let max_attempts = if paid {
        1 + policy.max_failover_attempts
    } else {
        1
    };
    Ok(RoutePlan {
        route_class: class,
        policy_version: ROUTING_POLICY_VERSION.to_string(),
        source_endpoint,
        destination_endpoint,
        public_network_required: paid,
        required_capabilities,
        destination_country: country,
        protected_contact_refs: protected,
        verified_contacts_required: paid && policy.require_verified_contacts,
        relay_session_id: inputs.relay_session_id,
        context_id: inputs.context_id,
        candidate_routes: candidates.iter().map(|r| r.route_ref.clone()).collect(),
        failover: FailoverPolicy {
            max_attempts,
            on_definite_failure: paid && policy.max_failover_attempts > 0,
            duplicate_risk: policy.duplicate_risk,
        },
        unknown_inputs: unknown,
        selected_at: now,
        expires_at: paid.then(|| now + Duration::seconds(PAID_PLAN_VALIDITY_SECONDS)),
    })
}

/// Outcome of the latest attempt, for route revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptFact {
    Pending,
    Succeeded,
    DefiniteFailure,
    Unknown,
}

/// What may happen to a selected route now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revision {
    /// Nothing to change.
    Keep,
    /// Another attempt (next candidate route) is safe.
    Failover { next_route_ref: Option<String> },
    /// No further attempt: the reason says why.
    Stop(RevisionStop),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionStop {
    /// After CONNECTED a route changes only through explicit transfer semantics.
    ConnectedRequiresTransfer,
    /// The previous outcome is unknown: reconcile before any duplicate-risk attempt.
    AwaitReconciliation,
    /// The plan does not permit failover, or every attempt is used.
    Exhausted,
}

/// Facts at revision time.
#[derive(Debug, Clone)]
pub struct RevisionFacts {
    pub call_connected: bool,
    pub last_attempt: AttemptFact,
    pub attempts_used: u8,
    /// Candidate route references already attempted (in order).
    pub attempted_routes: Vec<String>,
    /// Candidates now known unavailable.
    pub unavailable_routes: Vec<String>,
}

/// §25: revision before CONNECTED only, on a DEFINITE failure, when policy permits and
/// duplicate risk is controlled. An UNKNOWN outcome never fails over.
pub fn revise(plan: &RoutePlan, facts: &RevisionFacts) -> Revision {
    match facts.last_attempt {
        AttemptFact::Pending | AttemptFact::Succeeded => {
            if facts.call_connected {
                return Revision::Keep;
            }
            Revision::Keep
        }
        AttemptFact::Unknown => Revision::Stop(RevisionStop::AwaitReconciliation),
        AttemptFact::DefiniteFailure => {
            if facts.call_connected {
                return Revision::Stop(RevisionStop::ConnectedRequiresTransfer);
            }
            if !plan.failover.on_definite_failure
                || facts.attempts_used >= plan.failover.max_attempts
            {
                return Revision::Stop(RevisionStop::Exhausted);
            }
            let next = plan
                .candidate_routes
                .iter()
                .find(|r| {
                    !facts.attempted_routes.contains(r) && !facts.unavailable_routes.contains(r)
                })
                .cloned();
            if next.is_none() && !plan.candidate_routes.is_empty() {
                return Revision::Stop(RevisionStop::Exhausted);
            }
            Revision::Failover {
                next_route_ref: next,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::policy::DestinationPolicy;

    fn now() -> DateTime<Utc> {
        "2026-10-08T12:00:00Z".parse().unwrap()
    }

    fn public_policy() -> RoutingPolicy {
        RoutingPolicy {
            allowed_route_classes: RoutingClass::ALL.to_vec(),
            app_first: true,
            customer_may_select_public_route: false,
            public_calling_enabled: true,
            callback_relay_enabled: true,
            require_verified_contacts: true,
            destinations: DestinationPolicy {
                allow: vec!["NG".into()],
                deny: vec!["GH".into()],
                default: CountryDecision::Deny,
            },
            max_failover_attempts: 1,
            max_estimated_leg_cost_minor: Some(500),
            duplicate_risk: DuplicateRiskPolicy::BlockUntilReconciled,
        }
    }

    fn route(r: &str, health: RouteHealth) -> CandidateRoute {
        CandidateRoute {
            route_ref: r.into(),
            health,
            capacity_available: Evidence::Known(true),
            estimated_leg_cost_minor: Evidence::Known(100),
            quality_permille: Evidence::Known(600),
        }
    }

    fn contact(verification: ContactVerification, country: &str) -> ContactFacts {
        ContactFacts {
            contact_reference: Uuid::new_v4(),
            verification: Evidence::Known(verification),
            country: Evidence::Known(country.into()),
        }
    }

    fn app_dest(reachable: bool) -> DestinationFacts {
        DestinationFacts {
            class: EndpointClass::Application,
            app_reachable: Evidence::Known(reachable),
            contact: None,
        }
    }

    fn inputs() -> RouteInputs {
        RouteInputs {
            direction: RouteDirection::Outbound,
            source: EndpointClass::Application,
            source_contact: None,
            destinations: vec![app_dest(true)],
            requested: None,
            authorized: true,
            permitted_classes: RoutingClass::ALL.to_vec(),
            relay_session_id: Some(Uuid::nil()),
            context_id: None,
            live_telecom_allowed: true,
            flags: TelecomFlags {
                public_outbound: true,
                public_inbound: true,
                callback_relay: true,
            },
            routes: Evidence::Known(vec![route("r-a", RouteHealth::Healthy)]),
            spend_headroom_minor: Evidence::Known(10_000),
            operational_incident: Evidence::Known(false),
            duplicate_risk: DuplicateRisk::None,
        }
    }

    fn class(d: &RouteDecision) -> RoutingClass {
        d.plan().expect("selected").route_class
    }

    #[test]
    fn app_rtc_wins_when_every_target_is_reachable() {
        let d = select(&public_policy(), &inputs(), now());
        assert_eq!(class(&d), RoutingClass::AppRtc);
        assert!(!d.plan().unwrap().public_network_required);
        // A customer request for a public route does not bypass app-first.
        let mut i = inputs();
        i.destinations[0].contact = Some(contact(ContactVerification::Verified, "NG"));
        i.requested = Some(RoutingClass::AppToPstn);
        assert_eq!(
            class(&select(&public_policy(), &i, now())),
            RoutingClass::AppRtc
        );
        // ...unless the explicit policy hook allows it.
        let mut p = public_policy();
        p.customer_may_select_public_route = true;
        assert_eq!(class(&select(&p, &i, now())), RoutingClass::AppToPstn);
    }

    #[test]
    fn app_unavailable_with_a_verified_contact_routes_app_to_pstn() {
        let mut i = inputs();
        i.destinations = vec![DestinationFacts {
            class: EndpointClass::Application,
            app_reachable: Evidence::Known(false),
            contact: Some(contact(ContactVerification::Verified, "NG")),
        }];
        let d = select(&public_policy(), &i, now());
        let plan = d.plan().unwrap();
        assert_eq!(plan.route_class, RoutingClass::AppToPstn);
        assert_eq!(plan.destination_country.as_deref(), Some("NG"));
        assert_eq!(plan.candidate_routes, vec!["r-a".to_string()]);
        assert_eq!(plan.failover.max_attempts, 2);
        assert!(plan.expires_at.is_some());
        // A direct protected-contact target (no application endpoint at all).
        i.destinations[0].class = EndpointClass::PublicTelephone;
        i.destinations[0].app_reachable = Evidence::Unknown;
        assert_eq!(
            class(&select(&public_policy(), &i, now())),
            RoutingClass::AppToPstn
        );
        // Without a contact, an unreachable application target has no route.
        i.destinations[0].contact = None;
        assert_eq!(
            select(&public_policy(), &i, now()).denial(),
            Some(RouteDenial::TargetUnreachable)
        );
    }

    #[test]
    fn inbound_public_calls_route_to_app_or_relay() {
        let mut i = inputs();
        i.direction = RouteDirection::Inbound;
        i.source = EndpointClass::PublicTelephone;
        i.source_contact = Some(contact(ContactVerification::NotVerified, "NG"));
        assert_eq!(
            class(&select(&public_policy(), &i, now())),
            RoutingClass::PstnToApp
        );
        // Two protected telephone endpoints: phone-to-phone relay.
        i.destinations = vec![DestinationFacts {
            class: EndpointClass::PublicTelephone,
            app_reachable: Evidence::Unknown,
            contact: Some(contact(ContactVerification::Verified, "NG")),
        }];
        let d = select(&public_policy(), &i, now());
        assert_eq!(class(&d), RoutingClass::PstnRelay);
        assert_eq!(d.plan().unwrap().protected_contact_refs.len(), 2);
        // Disabled inbound calling stops it.
        i.flags.public_inbound = false;
        assert_eq!(
            select(&public_policy(), &i, now()).denial(),
            Some(RouteDenial::TelecomFeatureDisabled)
        );
    }

    #[test]
    fn callback_relay_only_on_explicit_policy_permitted_request() {
        let mut i = inputs();
        i.source = EndpointClass::PublicTelephone;
        i.source_contact = Some(contact(ContactVerification::Verified, "NG"));
        i.destinations = vec![DestinationFacts {
            class: EndpointClass::PublicTelephone,
            app_reachable: Evidence::Unknown,
            contact: Some(contact(ContactVerification::Verified, "NG")),
        }];
        assert_eq!(
            select(&public_policy(), &i, now()).denial(),
            Some(RouteDenial::UnsupportedShape)
        );
        i.requested = Some(RoutingClass::CallbackRelay);
        assert_eq!(
            class(&select(&public_policy(), &i, now())),
            RoutingClass::CallbackRelay
        );
        let mut p = public_policy();
        p.callback_relay_enabled = false;
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::CallbackRelayDisabled)
        );
        // An offline application participant never triggers callback relay by itself.
        let mut j = inputs();
        j.destinations = vec![app_dest(false)];
        assert_ne!(
            select(&public_policy(), &j, now())
                .plan()
                .map(|p| p.route_class),
            Some(RoutingClass::CallbackRelay)
        );
    }

    #[test]
    fn enterprise_internal_is_representable_but_not_executable_yet() {
        let mut i = inputs();
        i.source = EndpointClass::Enterprise;
        i.destinations = vec![DestinationFacts {
            class: EndpointClass::Enterprise,
            app_reachable: Evidence::Unknown,
            contact: None,
        }];
        assert_eq!(
            select(&public_policy(), &i, now()).denial(),
            Some(RouteDenial::NotExecutable)
        );
        assert_eq!(
            classify(&public_policy(), &i),
            Ok(RoutingClass::EnterpriseInternal)
        );
    }

    fn pstn_inputs() -> RouteInputs {
        let mut i = inputs();
        i.destinations = vec![DestinationFacts {
            class: EndpointClass::PublicTelephone,
            app_reachable: Evidence::Unknown,
            contact: Some(contact(ContactVerification::Verified, "NG")),
        }];
        i
    }

    #[test]
    fn policy_gates_deny_paid_routes() {
        let p = public_policy();
        let mut i = pstn_inputs();
        i.destinations[0].contact = Some(contact(ContactVerification::Verified, "GH"));
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::DestinationCountryDenied)
        );
        i.destinations[0].contact = Some(contact(ContactVerification::Verified, "KE"));
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::DestinationCountryDenied)
        );
        i.destinations[0].contact.as_mut().unwrap().country = Evidence::Unknown;
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::DestinationCountryUnknown)
        );
        for v in [
            ContactVerification::NotVerified,
            ContactVerification::Expired,
            ContactVerification::Blocked,
        ] {
            let mut i = pstn_inputs();
            i.destinations[0].contact = Some(contact(v, "NG"));
            assert_eq!(
                select(&p, &i, now()).denial(),
                Some(RouteDenial::ContactNotVerified)
            );
        }
        let mut i = pstn_inputs();
        i.destinations[0].contact.as_mut().unwrap().verification = Evidence::Unknown;
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::ContactNotVerified)
        );
        let mut i = pstn_inputs();
        i.authorized = false;
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::NotAuthorized)
        );
        let mut i = pstn_inputs();
        i.permitted_classes = vec![RoutingClass::AppRtc];
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::NotAuthorized)
        );
        let mut i = pstn_inputs();
        i.live_telecom_allowed = false;
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::LiveTelecomNotAllowed)
        );
        let mut i = pstn_inputs();
        i.flags.public_outbound = false;
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::TelecomFeatureDisabled)
        );
        let mut i = pstn_inputs();
        i.spend_headroom_minor = Evidence::Known(50);
        assert_eq!(
            select(&p, &i, now()).denial(),
            Some(RouteDenial::SpendLimitExceeded)
        );
        let mut q = p.clone();
        q.max_estimated_leg_cost_minor = Some(10);
        assert_eq!(
            select(&q, &pstn_inputs(), now()).denial(),
            Some(RouteDenial::CostLimitExceeded)
        );
        let mut q = p.clone();
        q.public_calling_enabled = false;
        assert_eq!(
            select(&q, &pstn_inputs(), now()).denial(),
            Some(RouteDenial::PublicCallingDisabled)
        );
        assert_eq!(
            select(&RoutingPolicy::default(), &pstn_inputs(), now()).denial(),
            Some(RouteDenial::RouteClassNotAllowed)
        );
        // Disabling public calling never disables application RTC.
        assert_eq!(class(&select(&q, &inputs(), now())), RoutingClass::AppRtc);
        assert_eq!(
            class(&select(&RoutingPolicy::default(), &inputs(), now())),
            RoutingClass::AppRtc
        );
    }

    #[test]
    fn unavailable_providers_fall_back_to_a_healthy_alternate() {
        let mut i = pstn_inputs();
        i.routes = Evidence::Known(vec![
            route("r-a", RouteHealth::Unavailable),
            route("r-b", RouteHealth::Degraded),
            route("r-c", RouteHealth::Healthy),
        ]);
        let plan = select(&public_policy(), &i, now()).plan().cloned().unwrap();
        assert_eq!(
            plan.candidate_routes,
            vec!["r-c".to_string(), "r-b".to_string()]
        );
        i.routes = Evidence::Known(vec![route("r-a", RouteHealth::Unavailable)]);
        assert_eq!(
            select(&public_policy(), &i, now()).denial(),
            Some(RouteDenial::RouteUnavailable)
        );
        let mut full = route("r-a", RouteHealth::Healthy);
        full.capacity_available = Evidence::Known(false);
        i.routes = Evidence::Known(vec![full]);
        assert_eq!(
            select(&public_policy(), &i, now()).denial(),
            Some(RouteDenial::CapacityExhausted)
        );
        // Unknown health is recorded as unknown, not invented.
        i.routes = Evidence::Unknown;
        let plan = select(&public_policy(), &i, now()).plan().cloned().unwrap();
        assert!(plan.unknown_inputs.contains(&"route_health".to_string()));
    }

    #[test]
    fn unknown_origination_blocks_failover_until_reconciled() {
        let mut i = pstn_inputs();
        i.duplicate_risk = DuplicateRisk::Unresolved;
        assert_eq!(
            select(&public_policy(), &i, now()).denial(),
            Some(RouteDenial::DuplicateRiskUnresolved)
        );
        let mut i = pstn_inputs();
        i.routes = Evidence::Known(vec![
            route("r-a", RouteHealth::Healthy),
            route("r-b", RouteHealth::Healthy),
        ]);
        let plan = select(&public_policy(), &i, now()).plan().cloned().unwrap();
        let facts = |last, used: u8, connected| RevisionFacts {
            call_connected: connected,
            last_attempt: last,
            attempts_used: used,
            attempted_routes: vec!["r-a".into()],
            unavailable_routes: vec![],
        };
        assert_eq!(
            revise(&plan, &facts(AttemptFact::Unknown, 1, false)),
            Revision::Stop(RevisionStop::AwaitReconciliation)
        );
        assert_eq!(
            revise(&plan, &facts(AttemptFact::DefiniteFailure, 1, false)),
            Revision::Failover {
                next_route_ref: Some("r-b".into())
            }
        );
        assert_eq!(
            revise(&plan, &facts(AttemptFact::DefiniteFailure, 2, false)),
            Revision::Stop(RevisionStop::Exhausted)
        );
        assert_eq!(
            revise(&plan, &facts(AttemptFact::DefiniteFailure, 1, true)),
            Revision::Stop(RevisionStop::ConnectedRequiresTransfer)
        );
        assert_eq!(
            revise(&plan, &facts(AttemptFact::Pending, 1, false)),
            Revision::Keep
        );
    }

    #[test]
    fn identical_inputs_give_identical_fingerprints() {
        let a = select(&public_policy(), &pstn_inputs(), now())
            .plan()
            .cloned()
            .unwrap();
        let mut later = pstn_inputs();
        later.destinations = a
            .protected_contact_refs
            .iter()
            .map(|c| DestinationFacts {
                class: EndpointClass::PublicTelephone,
                app_reachable: Evidence::Unknown,
                contact: Some(ContactFacts {
                    contact_reference: *c,
                    verification: Evidence::Known(ContactVerification::Verified),
                    country: Evidence::Known("NG".into()),
                }),
            })
            .collect();
        let b = select(&public_policy(), &later, now() + Duration::seconds(30))
            .plan()
            .cloned()
            .unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        let mut changed = later.clone();
        changed.routes = Evidence::Known(vec![route("r-z", RouteHealth::Healthy)]);
        let c = select(&public_policy(), &changed, now())
            .plan()
            .cloned()
            .unwrap();
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    #[test]
    fn plans_never_contain_numbers_or_provider_names() {
        let plan = select(&public_policy(), &pstn_inputs(), now())
            .plan()
            .cloned()
            .unwrap();
        let text = serde_json::to_string(&plan).unwrap();
        assert!(!text.contains('+'));
        for word in ["provider", "carrier", "sip", "e164"] {
            assert!(!text.to_lowercase().contains(word), "{word}");
        }
    }
}
