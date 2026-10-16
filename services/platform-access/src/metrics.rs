//! Bounded operational metrics, rendered in the Prometheus text format on a SEPARATE
//! listener (`PLATFORM_ACCESS_METRICS_ADDRESS`; never on the internal API). Label values
//! are `&'static str` by type: a credential, public id, tenant id or principal reference
//! can never become a label, so cardinality stays bounded by normalized vocabularies.
//! Gauges (credentials by status, change-feed head) are computed at scrape time.

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
    let db = state.platform.db();
    if let Ok(rows) = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT status, count(*) FROM platform_api_credentials GROUP BY status ORDER BY status",
        ))
        .await
    {
        let _ = writeln!(out, "# TYPE platform_access_credentials gauge");
        for row in rows {
            if let (Ok(status), Ok(n)) = (
                row.try_get_by_index::<String>(0),
                row.try_get_by_index::<i64>(1),
            ) {
                let status = match status.as_str() {
                    "ACTIVE" => "ACTIVE",
                    "REVOKED" => "REVOKED",
                    _ => "OTHER",
                };
                let _ = writeln!(out, "platform_access_credentials{{status=\"{status}\"}} {n}");
            }
        }
    }
    if let Ok(Some(row)) = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT COALESCE(max(sequence), 0) FROM platform_identity_changes",
        ))
        .await
    {
        if let Ok(head) = row.try_get_by_index::<i64>(0) {
            let _ = writeln!(out, "# TYPE platform_access_change_feed_head gauge");
            let _ = writeln!(out, "platform_access_change_feed_head {head}");
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
    Router::new().route("/metrics", get(metrics)).with_state(state)
}
