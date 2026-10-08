//! Interconnect route selection inside Telephony (Part E §42, §51, §148–§150).
//!
//! Calling's Routing Engine decides the permitted route CLASS (and ranks candidate
//! Telephony routes); Telephony decides WHICH eligible interconnect executes: enabled
//! route and provider, destination country, capability, circuit breaker, health and
//! capacity. Business authorization never happens here.
//!
//! Capacity is enforced under row locks: the route and provider rows are locked
//! (`FOR UPDATE`) while the live legs on them are counted and the new leg is bound to
//! the route — two concurrent originations can never both take the last slot.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::{json, Value};

use crate::{
    db,
    domain::{CircuitState, Health, RouteCapability},
    error::{ErrorCode, TelephonyError, TelephonyResult},
    incidents::{self, Incident, Severity},
    service::TelephonyService,
};

/// Consecutive definite provider-side failures that open a route / provider circuit.
pub const CIRCUIT_FAILURE_THRESHOLD: i32 = 5;
/// How long an open circuit stays open before one probe (HALF_OPEN) is allowed.
pub const CIRCUIT_OPEN_SECONDS: i64 = 60;

#[derive(Debug, Clone, FromQueryResult)]
pub struct RouteRow {
    pub id: String,
    pub provider_id: String,
    pub priority: i32,
    pub sip_peer_config_ref_id: Option<String>,
    pub max_concurrent_legs: i32,
    pub estimated_leg_cost_minor: Option<i64>,
    pub cost_currency: Option<String>,
    pub max_call_seconds: i32,
    pub circuit_state: String,
    pub circuit_opened_at: Option<DateTime<Utc>>,
    pub health: String,
    pub provider_health: String,
    pub provider_circuit_state: String,
    pub provider_circuit_opened_at: Option<DateTime<Utc>>,
}

const ROUTE_SELECT: &str = "SELECT r.id, r.provider_id, r.priority, r.sip_peer_config_ref_id, \
    r.max_concurrent_legs, r.estimated_leg_cost_minor, r.cost_currency, r.max_call_seconds, \
    r.circuit_state, r.circuit_opened_at, r.health, p.health AS provider_health, \
    p.circuit_state AS provider_circuit_state, p.circuit_opened_at AS provider_circuit_opened_at \
    FROM telecom_routes r JOIN telecom_providers p ON p.id = r.provider_id \
    WHERE r.status = 'ENABLED' AND p.status = 'ENABLED'";

fn circuit_allows(state: &str, opened_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match CircuitState::parse(state) {
        Some(CircuitState::Closed) | Some(CircuitState::HalfOpen) => true,
        Some(CircuitState::Open) => {
            opened_at.is_some_and(|at| now - at >= Duration::seconds(CIRCUIT_OPEN_SECONDS))
        }
        None => false,
    }
}

impl RouteRow {
    /// Whether both circuits allow an attempt now (an expired OPEN allows one probe).
    pub fn circuit_allows(&self, now: DateTime<Utc>) -> bool {
        circuit_allows(&self.circuit_state, self.circuit_opened_at, now)
            && circuit_allows(
                &self.provider_circuit_state,
                self.provider_circuit_opened_at,
                now,
            )
    }

    /// Normalized health: the worse of route and provider; an open circuit is
    /// UNAVAILABLE.
    pub fn effective_health(&self, now: DateTime<Utc>) -> Health {
        if !self.circuit_allows(now) {
            return Health::Unavailable;
        }
        let rank = |h: &str| match Health::parse(h) {
            Some(Health::Healthy) => 0,
            Some(Health::Degraded) => 1,
            Some(Health::Unknown) | None => 2,
            Some(Health::Unavailable) => 3,
        };
        match rank(&self.health).max(rank(&self.provider_health)) {
            0 => Health::Healthy,
            1 => Health::Degraded,
            2 => Health::Unknown,
            _ => Health::Unavailable,
        }
    }
}

impl TelephonyService {
    /// Enabled routes for `country` / `capability`, best first (Calling's candidate order
    /// when given, then priority), restricted to registered providers.
    pub async fn eligible_routes<C: ConnectionTrait>(
        &self,
        db_: &C,
        country: &str,
        capability: RouteCapability,
        candidates: &[String],
    ) -> TelephonyResult<Vec<RouteRow>> {
        let rows: Vec<RouteRow> = db::all(
            db_,
            &format!(
                "{ROUTE_SELECT} AND $1 = ANY(r.countries) AND $2 = ANY(r.capabilities) \
                 ORDER BY r.priority, r.id"
            ),
            vec![country.into(), capability.as_str().into()],
        )
        .await?;
        let now = self.now();
        let mut rows: Vec<RouteRow> = rows
            .into_iter()
            .filter(|r| self.providers.get(&r.provider_id).is_some())
            .filter(|r| r.circuit_allows(now))
            .filter(|r| r.effective_health(now) != Health::Unavailable)
            .filter(|r| candidates.is_empty() || candidates.contains(&r.id))
            .collect();
        if !candidates.is_empty() {
            rows.sort_by_key(|r| {
                candidates
                    .iter()
                    .position(|c| c == &r.id)
                    .unwrap_or(usize::MAX)
            });
        }
        Ok(rows)
    }

    /// Reserves capacity on `route` for one more live leg (call inside the transaction
    /// that binds the leg to the route). Locks provider then route.
    pub async fn reserve_capacity<C: ConnectionTrait>(
        &self,
        txn: &C,
        route: &RouteRow,
    ) -> TelephonyResult<()> {
        let provider_max: i64 = db::scalar_i64(
            txn,
            "SELECT max_concurrent_legs::bigint FROM telecom_providers WHERE id = $1 FOR UPDATE",
            vec![route.provider_id.clone().into()],
        )
        .await?;
        let route_max: i64 = db::scalar_i64(
            txn,
            "SELECT max_concurrent_legs::bigint FROM telecom_routes WHERE id = $1 FOR UPDATE",
            vec![route.id.clone().into()],
        )
        .await?;
        let on_route = db::scalar_i64(
            txn,
            "SELECT count(*) FROM telephony_legs WHERE route_id = $1 AND status NOT IN ('ENDED','FAILED')",
            vec![route.id.clone().into()],
        )
        .await?;
        let on_provider = db::scalar_i64(
            txn,
            "SELECT count(*) FROM telephony_legs WHERE provider_id = $1 AND status NOT IN ('ENDED','FAILED')",
            vec![route.provider_id.clone().into()],
        )
        .await?;
        if on_route >= route_max || on_provider >= provider_max {
            return Err(TelephonyError::Refused(ErrorCode::CapacityExhausted));
        }
        Ok(())
    }

    /// Circuit accounting of a definite provider-side failure.
    pub async fn record_route_failure<C: ConnectionTrait>(
        &self,
        db_: &C,
        route_id: &str,
        provider_id: &str,
    ) -> TelephonyResult<()> {
        let now = self.now();
        for (table, id) in [
            ("telecom_routes", route_id),
            ("telecom_providers", provider_id),
        ] {
            let opened: Option<OpenedRow> = db::one(
                db_,
                &format!(
                    "UPDATE {table} SET consecutive_failures = consecutive_failures + 1, updated_at = GREATEST(updated_at, $2), \
                       version = version + 1, \
                       circuit_state = CASE WHEN consecutive_failures + 1 >= $3 OR circuit_state = 'HALF_OPEN' THEN 'OPEN' ELSE circuit_state END, \
                       circuit_opened_at = CASE WHEN consecutive_failures + 1 >= $3 OR circuit_state = 'HALF_OPEN' THEN $2 ELSE circuit_opened_at END \
                     WHERE id = $1 RETURNING circuit_state"
                ),
                vec![id.into(), now.into(), CIRCUIT_FAILURE_THRESHOLD.into()],
            )
            .await?;
            if opened.is_some_and(|o| o.circuit_state == "OPEN") {
                crate::metrics::inc(
                    "telephony_circuit_opened_total",
                    &[(
                        "scope",
                        if table == "telecom_routes" {
                            "ROUTE"
                        } else {
                            "PROVIDER"
                        },
                    )],
                );
                let mut incident = Incident::new(
                    "PROVIDER_CIRCUIT_OPENED",
                    Severity::High,
                    format!("circuit:{table}:{id}"),
                )
                .evidence(json!({ "scope": table, "id": id }));
                if table == "telecom_routes" {
                    incident.route_id = Some(id.to_string());
                } else {
                    incident.provider_id = Some(id.to_string());
                }
                incidents::raise(db_, &incident, now).await?;
            }
        }
        Ok(())
    }

    /// Circuit accounting of a success: closes the circuits (ends a HALF_OPEN probe).
    pub async fn record_route_success<C: ConnectionTrait>(
        &self,
        db_: &C,
        route_id: &str,
        provider_id: &str,
    ) -> TelephonyResult<()> {
        let now = self.now();
        for (table, id) in [
            ("telecom_routes", route_id),
            ("telecom_providers", provider_id),
        ] {
            db::exec(
                db_,
                &format!(
                    "UPDATE {table} SET consecutive_failures = 0, circuit_state = 'CLOSED', circuit_opened_at = NULL, \
                       updated_at = GREATEST(updated_at, $2), version = version + 1 \
                     WHERE id = $1 AND (consecutive_failures <> 0 OR circuit_state <> 'CLOSED')"
                ),
                vec![id.into(), now.into()],
            )
            .await?;
        }
        Ok(())
    }

    /// Normalized availability for Calling's Routing Engine (`GET /routes/availability`):
    /// opaque route refs, health, capacity, estimated cost and quality — never a
    /// provider name or credential.
    pub async fn availability(
        &self,
        country: &str,
        capability: RouteCapability,
    ) -> TelephonyResult<Value> {
        let rows: Vec<RouteRow> = db::all(
            &self.db,
            &format!(
                "{ROUTE_SELECT} AND $1 = ANY(r.countries) AND $2 = ANY(r.capabilities) ORDER BY r.priority, r.id"
            ),
            vec![country.into(), capability.as_str().into()],
        )
        .await?;
        let now = self.now();
        let enabled = self.flags.telephony.enabled()
            && match capability {
                RouteCapability::VoiceOutbound => self.flags.public_outbound.enabled(),
                RouteCapability::VoiceInbound => self.flags.public_inbound.enabled(),
            };
        let mut routes = Vec::new();
        for r in rows
            .iter()
            .filter(|r| self.providers.get(&r.provider_id).is_some())
        {
            let live = db::scalar_i64(
                &self.db,
                "SELECT count(*) FROM telephony_legs WHERE route_id = $1 AND status NOT IN ('ENDED','FAILED')",
                vec![r.id.clone().into()],
            )
            .await?;
            let quality: Option<QualityRow> = db::one(
                &self.db,
                "SELECT count(*) FILTER (WHERE answered_at IS NOT NULL) AS answered, count(*) AS total \
                 FROM telephony_legs WHERE route_id = $1 AND direction = 'OUTBOUND' AND originating_at IS NOT NULL \
                   AND created_at >= $2",
                vec![r.id.clone().into(), (now - Duration::hours(24)).into()],
            )
            .await?;
            let quality = quality
                .filter(|q| q.total >= 10)
                .map(|q| json!({ "evidence": "KNOWN", "value": (q.answered * 1000 / q.total) }))
                .unwrap_or_else(|| json!({ "evidence": "UNKNOWN" }));
            let health = if enabled {
                r.effective_health(now)
            } else {
                Health::Unavailable
            };
            routes.push(json!({
                "routeRef": r.id,
                "health": health.as_str(),
                "capacityAvailable": { "evidence": "KNOWN", "value": live < i64::from(r.max_concurrent_legs) },
                "estimatedLegCostMinor": match (r.estimated_leg_cost_minor, &r.cost_currency) {
                    (Some(c), Some(_)) => json!({ "evidence": "KNOWN", "value": c }),
                    _ => json!({ "evidence": "UNKNOWN" }),
                },
                "costCurrency": r.cost_currency,
                "qualityPermille": quality,
                "maxCallSeconds": r.max_call_seconds,
            }));
        }
        Ok(json!({
            "country": country,
            "capability": capability.as_str(),
            "routes": routes,
            "observedAt": crate::legs::ts(now),
        }))
    }
}

#[derive(Debug, FromQueryResult)]
struct OpenedRow {
    circuit_state: String,
}

#[derive(Debug, FromQueryResult)]
struct QualityRow {
    answered: i64,
    total: i64,
}
