//! Bounded operational metrics (§84).
//!
//! Counters are kept in process and rendered in the Prometheus text format on a SEPARATE
//! listener (`PRIVACY_RELAY_METRICS_ADDRESS`; never on the public router, never under
//! `/internal/v1`). Label values are `&'static str` by type and come from the normalized
//! vocabularies (lifecycle events, authorization reasons, error codes, retention
//! classes): a tenant / application / principal / context / session id, a
//! ContactReference, a telephone number, a provider reference or a secret cannot become
//! a label. Gauges (outbox backlog and its oldest age, enforcement jobs, relay-number
//! pool, open incidents) are computed from Privacy Relay's own tables at scrape time, as
//! counts per normalized state — never per identity.
//!
//! Families:
//!
//! | name | type | labels |
//! |---|---|---|
//! | `privacy_relay_context_events_total` | counter | `event` (CREATED, REVOKED, EXPIRED, SUSPENDED, RESUMED, COMPLETED) |
//! | `privacy_relay_session_events_total` | counter | `event` (CREATED, REVOKED, EXPIRED, SUSPENDED, RESUMED, COMPLETED) |
//! | `privacy_relay_relay_number_events_total` | counter | `event` (ASSIGNED, RELEASED, COOLDOWN_STARTED, COOLDOWN_COMPLETED), `reason` (end reason, on RELEASED) |
//! | `privacy_relay_relay_number_assignment_refusals_total` | counter | `reason` (error code) |
//! | `privacy_relay_authorization_decisions_total` | counter | `decision`, `reason`, `route` |
//! | `privacy_relay_inbound_resolutions_total` | counter | `outcome`, `reason` |
//! | `privacy_relay_contact_resolution_failures_total` | counter | `reason` (error code) |
//! | `privacy_relay_outbox_published_total` | counter | — |
//! | `privacy_relay_outbox_retries_total` | counter | `reason` (bounded failure code) |
//! | `privacy_relay_outbox_dead_lettered_total` | counter | `reason`, `kind` (PERMANENT, EXHAUSTED) |
//! | `privacy_relay_enforcement_attempts_total` | counter | `outcome` (APPLIED or failure category) |
//! | `privacy_relay_enforcement_dead_lettered_total` | counter | `reason` (enforcement reason) |
//! | `privacy_relay_retention_batches_total` / `_rows_total` | counter | `data_class`, `action` |
//! | `privacy_relay_incidents_raised_total` | counter | `category` |
//! | `privacy_relay_outbox_pending` | gauge | — |
//! | `privacy_relay_outbox_oldest_pending_age_seconds` | gauge | — |
//! | `privacy_relay_outbox_dead_letter` | gauge | — |
//! | `privacy_relay_enforcement_jobs` | gauge | `status` (PENDING, DEAD_LETTER) |
//! | `privacy_relay_enforcement_oldest_pending_age_seconds` | gauge | — |
//! | `privacy_relay_relay_numbers` | gauge | `status` |
//! | `privacy_relay_relay_assignments` | gauge | `status` (ACTIVE, COOLDOWN) |
//! | `privacy_relay_open_incidents` | gauge | `category` |

use std::{
    collections::BTreeMap,
    fmt::Write,
    sync::{LazyLock, Mutex},
};

use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use sea_orm::FromQueryResult;

use crate::{db, error::RelayResult, service::RelayService};

type Labels = Vec<(&'static str, &'static str)>;

#[derive(Default)]
struct Registry {
    counters: BTreeMap<(&'static str, Labels), u64>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(Mutex::default);

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|p| p.into_inner())
}

/// Adds one to a counter.
pub fn inc(name: &'static str, labels: &[(&'static str, &'static str)]) {
    add(name, labels, 1);
}

/// Adds `n` to a counter.
pub fn add(name: &'static str, labels: &[(&'static str, &'static str)], n: u64) {
    if n == 0 {
        return;
    }
    *registry()
        .counters
        .entry((name, labels.to_vec()))
        .or_default() += n;
}

/// The current value of a counter (tests).
pub fn counter(name: &'static str, labels: &[(&'static str, &'static str)]) -> u64 {
    registry()
        .counters
        .get(&(name, labels.to_vec()))
        .copied()
        .unwrap_or(0)
}

fn label_text(labels: &[(&'static str, &'static str)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
    format!("{{{}}}", inner.join(","))
}

/// The in-process counters in the Prometheus text format.
pub fn render_registry() -> String {
    let mut out = String::new();
    let r = registry();
    let mut last = "";
    for ((name, labels), value) in &r.counters {
        if *name != last {
            let _ = writeln!(out, "# TYPE {name} counter");
            last = name;
        }
        let _ = writeln!(out, "{name}{} {value}", label_text(labels));
    }
    out
}

#[derive(Debug, FromQueryResult)]
struct StateCount {
    state: String,
    n: i64,
}

/// Only normalized vocabulary values may become gauge labels.
fn bounded(state: &str) -> bool {
    !state.is_empty()
        && state.len() <= 48
        && state.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
}

impl RelayService {
    /// The full scrape: counters and table-derived gauges.
    pub async fn metrics_text(&self) -> RelayResult<String> {
        let mut out = render_registry();
        let now = self.now();
        for (name, key, sql) in [
            (
                "privacy_relay_enforcement_jobs",
                "status",
                "SELECT status AS state, count(*) AS n FROM privacy_relay_enforcement_jobs \
                 WHERE status IN ('PENDING', 'DEAD_LETTER') GROUP BY status",
            ),
            (
                "privacy_relay_relay_assignments",
                "status",
                "SELECT status AS state, count(*) AS n FROM relay_number_assignments \
                 WHERE status IN ('ACTIVE', 'COOLDOWN') GROUP BY status",
            ),
            (
                "privacy_relay_open_incidents",
                "category",
                "SELECT category AS state, count(*) AS n FROM privacy_relay_incidents WHERE status = 'OPEN' GROUP BY category",
            ),
        ] {
            let rows: Vec<StateCount> = db::all(&self.db, sql, vec![]).await?;
            let _ = writeln!(out, "# TYPE {name} gauge");
            for r in rows.iter().filter(|r| bounded(&r.state)) {
                let _ = writeln!(out, "{name}{{{key}=\"{}\"}} {}", r.state, r.n);
            }
        }
        // The relay-number pool is read inside its own boundary.
        let _ = writeln!(out, "# TYPE privacy_relay_relay_numbers gauge");
        for (state, n) in crate::relay_numbers::pool_status_counts(&self.db).await? {
            if bounded(&state) {
                let _ = writeln!(out, "privacy_relay_relay_numbers{{status=\"{state}\"}} {n}");
            }
        }
        for (name, sql) in [
            (
                "privacy_relay_outbox_pending",
                "SELECT count(*)::bigint FROM privacy_relay_outbox WHERE status = 'PENDING'",
            ),
            (
                "privacy_relay_outbox_dead_letter",
                "SELECT count(*)::bigint FROM privacy_relay_outbox WHERE status = 'DEAD_LETTER'",
            ),
            (
                "privacy_relay_outbox_oldest_pending_age_seconds",
                "SELECT COALESCE(EXTRACT(EPOCH FROM ($1 - min(created_at))), 0)::bigint \
                 FROM privacy_relay_outbox WHERE status = 'PENDING'",
            ),
            (
                "privacy_relay_enforcement_oldest_pending_age_seconds",
                "SELECT COALESCE(EXTRACT(EPOCH FROM ($1 - min(created_at))), 0)::bigint \
                 FROM privacy_relay_enforcement_jobs WHERE status = 'PENDING'",
            ),
        ] {
            let values = if sql.contains("$1") {
                vec![now.into()]
            } else {
                vec![]
            };
            let value = db::scalar_i64(&self.db, sql, values).await?.max(0);
            let _ = writeln!(out, "# TYPE {name} gauge");
            let _ = writeln!(out, "{name} {value}");
        }
        Ok(out)
    }
}

async fn scrape(State(svc): State<RelayService>) -> impl IntoResponse {
    match svc.metrics_text().await {
        Ok(text) => (
            axum::http::StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
            text,
        )
            .into_response(),
        Err(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// The metrics listener's router (`GET /metrics` only; served on its own address).
pub fn router(svc: RelayService) -> Router {
    Router::new().route("/metrics", get(scrape)).with_state(svc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_render_with_bounded_labels() {
        inc("privacy_relay_test_total", &[("event", "CREATED")]);
        add("privacy_relay_test_total", &[("event", "CREATED")], 2);
        add("privacy_relay_test_total", &[("event", "REVOKED")], 0);
        let out = render_registry();
        assert!(
            out.contains("privacy_relay_test_total{event=\"CREATED\"} 3"),
            "{out}"
        );
        assert!(!out.contains("REVOKED"), "a zero add creates no series");
        assert!(bounded("PENDING") && !bounded("ck-1") && !bounded("+447946000000"));
    }
}
