//! Routing fence protocol v1 (Document 4 §9): one system per message.
//!
//! Pure, database-agnostic logic that every producer runs **inside its own business
//! transaction** (the transaction that writes either the old-path row or the outbox row):
//!
//! * [`decide`] — the decision algorithm of §9.3 (sticky decisions, mode, canary,
//!   CRITICAL `FORCED` fallback);
//! * [`canary`] — the pure canary function of the key (`SHA-256(key)` → `u64` → `mod 100`);
//! * [`legacy_insert_allowed`] — the legacy-table guard trigger rule of §9.4, so producer
//!   tests and the SQL trigger share one specification;
//! * [`critical_reroute`] — the CRITICAL lease reroute rules of §9.8 (X6/X7).
//!
//! The producer-side tables (`notification_routing_state`, `notification_route_decisions`)
//! are created by N7; this module fixes their semantics now so N7 only has to persist them.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The routing protocol version this crate implements (reported as `routingProtocol` in
/// `/ready`; envelopes carry it in `route.protocol`).
pub const ROUTING_PROTOCOL: i16 = 1;

/// `route` in the envelope: the producer's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteRef {
    pub decision_id: Uuid,
    pub routing_version: i32,
    pub protocol: i16,
}

/// `notification_routing_state.mode` (producer side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RoutingMode {
    /// Pre-fence behaviour: old path only; guard triggers are inert.
    LegacyOnly,
    /// Old path, plus a shadow event the service records but never sends.
    Shadow,
    /// Fence active, every new decision OLD (rollback state).
    FencedOld,
    /// Fence active, new decisions NEW for the canary share of keys.
    FencedNew,
}

/// `notification_routing_state.critical_fallback` (§10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CriticalFallback {
    Auto,
    Forced,
    Off,
}

/// One row of `notification_routing_state` (cached ≤ 5 s by producers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingState {
    pub category: String,
    pub mode: RoutingMode,
    pub canary_percent: u8,
    pub critical_fallback: CriticalFallback,
    pub routing_version: i32,
    pub protocol: i16,
}

impl RoutingState {
    /// The initial state installed by the fence-capable release (§9.4): inert.
    pub fn legacy_only(category: &str) -> Self {
        Self {
            category: category.to_string(),
            mode: RoutingMode::LegacyOnly,
            canary_percent: 0,
            critical_fallback: CriticalFallback::Auto,
            routing_version: 1,
            protocol: ROUTING_PROTOCOL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Route {
    Old,
    New,
}

/// `notification_route_decisions.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DecisionReason {
    Routed,
    ReroutedUnacked,
    ReroutedCancelled,
    ReroutedCriticalLease,
    AutoFailover,
    ReroutedPossibleDuplicate,
}

/// A decision already recorded for the key (found `FOR UPDATE` in the transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExistingDecision {
    pub decision_id: Uuid,
    pub route: Route,
    pub routing_version: i32,
}

/// The outcome of [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub decision_id: Uuid,
    pub route: Route,
    pub routing_version: i32,
    pub reason: DecisionReason,
    /// `true`: an earlier decision was followed (write nothing new: no decision row, no
    /// old-path row, no outbox row).
    pub sticky: bool,
    /// `true` (SHADOW mode, route OLD): also write a shadow event the service must never
    /// send.
    pub shadow: bool,
}

impl Decision {
    /// The envelope's `route` for a NEW decision.
    pub fn route_ref(&self) -> RouteRef {
        RouteRef {
            decision_id: self.decision_id,
            routing_version: self.routing_version,
            protocol: ROUTING_PROTOCOL,
        }
    }
}

/// The canary is a pure function of the key: `SHA-256(key)`, first 8 bytes as a
/// big-endian `u64`, `mod 100 < percent`. A retry always yields the same route.
pub fn canary(key: &str, percent: u8) -> bool {
    if percent >= 100 {
        return true;
    }
    let digest = Sha256::digest(key.as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(first) % 100) < u64::from(percent)
}

/// §9.3: decides the route of the message keyed `key` (today's idempotency-key formats)
/// inside the producer transaction. `new_decision_id` is used only when no decision
/// exists yet.
pub fn decide(
    key: &str,
    state: &RoutingState,
    critical: bool,
    existing: Option<&ExistingDecision>,
    new_decision_id: Uuid,
) -> Decision {
    if let Some(e) = existing {
        // Sticky: a retry of the same message follows the earlier decision.
        return Decision {
            decision_id: e.decision_id,
            route: e.route,
            routing_version: e.routing_version,
            reason: DecisionReason::Routed,
            sticky: true,
            shadow: false,
        };
    }
    let mut route = match state.mode {
        RoutingMode::LegacyOnly | RoutingMode::Shadow | RoutingMode::FencedOld => Route::Old,
        RoutingMode::FencedNew if canary(key, state.canary_percent) => Route::New,
        RoutingMode::FencedNew => Route::Old,
    };
    if critical && state.critical_fallback == CriticalFallback::Forced {
        route = Route::Old;
    }
    Decision {
        decision_id: new_decision_id,
        route,
        routing_version: state.routing_version,
        reason: DecisionReason::Routed,
        sticky: false,
        shadow: state.mode == RoutingMode::Shadow && route == Route::Old,
    }
}

/// §9.4: whether the guard trigger lets an old-path row (`email_deliveries`,
/// `meeting_notifications`, `messaging_push_jobs`) be inserted. In `LEGACY_ONLY` the
/// trigger is inert (pre-fence binaries work unchanged); otherwise a decision with route
/// OLD must exist for the key, or the insert raises `ROUTING_DECISION_REQUIRED`.
pub fn legacy_insert_allowed(mode: RoutingMode, decision: Option<Route>) -> bool {
    mode == RoutingMode::LegacyOnly || decision == Some(Route::Old)
}

/// The CRITICAL decision state as Account sees it in the row-locked transaction (§9.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CriticalDecisionState {
    pub route: Route,
    pub lease_count: i32,
    /// `send_lease_until` (Meeting DB clock), seconds since the epoch.
    pub send_lease_until: Option<i64>,
    pub completed: bool,
    pub code_expired: bool,
}

/// Verdict of a CRITICAL reroute attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerouteVerdict {
    /// Reroute now with this reason (zero duplicate risk, or recorded possible duplicate).
    Allowed(DecisionReason),
    /// A lease is live: wait for it to expire.
    WaitForLease,
    /// Completed, already OLD, expired, or a leased-but-uncompleted decision without
    /// FORCED: never rerouted automatically.
    Refused,
}

/// §9.8 / §10.4: may Account reroute this CRITICAL decision to the retained old path?
/// `automatic` is the AUTO failover of relay PW1; `forced` is `critical_fallback = FORCED`.
pub fn critical_reroute(
    state: &CriticalDecisionState,
    now: i64,
    automatic: bool,
    forced: bool,
) -> RerouteVerdict {
    if state.route == Route::Old || state.completed || state.code_expired {
        return RerouteVerdict::Refused;
    }
    if state.send_lease_until.is_some_and(|until| until > now) {
        return RerouteVerdict::WaitForLease;
    }
    if state.lease_count == 0 {
        // The service never had permission to send: no duplicate is possible, and any
        // later X6 claim answers REVOKED.
        return RerouteVerdict::Allowed(if automatic {
            DecisionReason::AutoFailover
        } else {
            DecisionReason::ReroutedCriticalLease
        });
    }
    // Leased at least once, lease expired, not completed: the service may have crashed
    // during or after a provider call. Manual break-glass only.
    if forced && !automatic {
        RerouteVerdict::Allowed(DecisionReason::ReroutedPossibleDuplicate)
    } else {
        RerouteVerdict::Refused
    }
}
