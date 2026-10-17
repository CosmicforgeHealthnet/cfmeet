//! Bounded operational metrics, rendered in the Prometheus text format on a SEPARATE
//! listener (`NUMBERING_METRICS_ADDRESS`; never on the API). Label values are
//! `&'static str` by type: a number, resource id, tenant id or provider reference can
//! never become a label, so cardinality stays bounded by normalized vocabularies.
//! Gauges (resources by state, outbox / job backlog) are computed at scrape time.

use std::{
    collections::BTreeMap,
    fmt::Write,
    sync::{LazyLock, Mutex},
};

use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::state::AppState;

type Labels = Vec<(&'static str, &'static str)>;

static COUNTERS: LazyLock<Mutex<BTreeMap<(&'static str, Labels), u64>>> =
    LazyLock::new(Mutex::default);

/// Adds one to a counter.
pub fn inc(name: &'static str, labels: &[(&'static str, &'static str)]) {
    *COUNTERS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry((name, labels.to_vec()))
        .or_default() += 1;
}

/// The current value of a counter (tests).
pub fn counter(name: &'static str, labels: &[(&'static str, &'static str)]) -> u64 {
    COUNTERS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
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

/// Renders every metric (counters + scrape-time gauges).
pub async fn render(state: &AppState) -> String {
    let mut out = String::new();
    {
        let counters = COUNTERS.lock().unwrap_or_else(|p| p.into_inner());
        let mut last = "";
        for ((name, labels), value) in counters.iter() {
            if *name != last {
                let _ = writeln!(out, "# TYPE {name} counter");
                last = name;
            }
            let _ = writeln!(out, "{name}{} {value}", label_text(labels));
        }
    }
    let db = &state.db;
    for (sql, metric, label) in [
        (
            "SELECT state, count(*) FROM number_resources GROUP BY state ORDER BY state",
            "numbering_resources",
            "state",
        ),
        (
            "SELECT status, count(*) FROM numbering_outbox_events GROUP BY status ORDER BY status",
            "numbering_outbox_events",
            "status",
        ),
        (
            "SELECT status, count(*) FROM numbering_jobs GROUP BY status ORDER BY status",
            "numbering_jobs",
            "status",
        ),
        // Workstreams P–X: lifecycle workflow backlogs, kill switches, open incidents.
        (
            "SELECT status, count(*) FROM number_suspensions GROUP BY status ORDER BY status",
            "numbering_suspensions",
            "status",
        ),
        (
            "SELECT status, count(*) FROM number_release_requests GROUP BY status ORDER BY status",
            "numbering_release_requests",
            "status",
        ),
        (
            "SELECT status, count(*) FROM number_quarantines GROUP BY status ORDER BY status",
            "numbering_quarantines",
            "status",
        ),
        (
            "SELECT status, count(*) FROM number_port_requests GROUP BY status ORDER BY status",
            "numbering_port_requests",
            "status",
        ),
        (
            "SELECT status, count(*) FROM number_relay_leases GROUP BY status ORDER BY status",
            "numbering_relay_leases",
            "status",
        ),
        (
            "SELECT category, count(*) FROM numbering_incidents WHERE status = 'OPEN' GROUP BY category ORDER BY category",
            "numbering_open_incidents",
            "category",
        ),
        (
            "SELECT control_key, CASE WHEN engaged THEN 1::bigint ELSE 0::bigint END FROM number_operational_controls ORDER BY control_key",
            "numbering_control_engaged",
            "control",
        ),
        (
            "SELECT 'STALE', count(*) FROM number_resources WHERE state = 'AVAILABLE' \
               AND (observation_fresh_until IS NULL OR observation_fresh_until <= now())",
            "numbering_inventory_observations",
            "freshness",
        ),
    ] {
        if let Ok(rows) = db
            .query_all(Statement::from_string(DbBackend::Postgres, sql))
            .await
        {
            let _ = writeln!(out, "# TYPE {metric} gauge");
            for row in rows {
                if let (Ok(value), Ok(n)) = (
                    row.try_get_by_index::<String>(0),
                    row.try_get_by_index::<i64>(1),
                ) {
                    // Values are database enumerations (CHECK-constrained), never ids.
                    if value.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') {
                        let _ = writeln!(out, "{metric}{{{label}=\"{value}\"}} {n}");
                    }
                }
            }
        }
    }
    out
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        render(&state).await,
    )
}

/// The metrics listener's router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .with_state(state)
}
