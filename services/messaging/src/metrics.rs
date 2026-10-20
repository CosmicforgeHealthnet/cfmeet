//! Bounded operational metrics, rendered in the Prometheus text format on a SEPARATE
//! listener (`MESSAGING_METRICS_ADDRESS`; never on the API). Label values are
//! `&'static str` by type: an id, key, credential or content can never become a label, so
//! cardinality stays bounded by normalized vocabularies. Gauges (devices, KeyPackages,
//! conversations by state, outbox backlog) are computed at scrape time.

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

static GAUGES: LazyLock<Mutex<BTreeMap<&'static str, i64>>> = LazyLock::new(Mutex::default);

/// Sets an in-process gauge (this instance's value, e.g. open realtime connections).
pub fn gauge(name: &'static str, value: i64) {
    GAUGES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(name, value);
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
    {
        let gauges = GAUGES.lock().unwrap_or_else(|p| p.into_inner());
        for (name, value) in gauges.iter() {
            let _ = writeln!(out, "# TYPE {name} gauge\n{name} {value}");
        }
    }
    let db = &state.db;
    for (sql, metric, label) in [
        (
            "SELECT status, count(*) FROM messaging_devices GROUP BY status ORDER BY status",
            "messaging_devices",
            "status",
        ),
        (
            "SELECT status, count(*) FROM messaging_key_packages GROUP BY status ORDER BY status",
            "messaging_key_packages",
            "status",
        ),
        (
            "SELECT status, count(*) FROM messaging_outbox_events GROUP BY status ORDER BY status",
            "messaging_outbox_events",
            "status",
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
