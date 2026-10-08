//! Bounded operational counters for public-network calling (Part E §192–§193).
//!
//! Rendered in the Prometheus text format on an optional SEPARATE listener
//! (`CALLING_METRICS_ADDRESS`), never on the customer API. Label values are
//! `&'static str` by type, so no ContactReference, Call id, principal or number can ever
//! become a label: cardinality is bounded by the normalized vocabularies (route classes,
//! denial / spend reasons, abuse-signal kinds).

use std::{
    collections::BTreeMap,
    fmt::Write,
    sync::{LazyLock, Mutex},
};

use axum::{http::header, response::IntoResponse, routing::get, Router};

type Key = (&'static str, Vec<(&'static str, &'static str)>);

static COUNTERS: LazyLock<Mutex<BTreeMap<Key, u64>>> = LazyLock::new(Mutex::default);

fn counters() -> std::sync::MutexGuard<'static, BTreeMap<Key, u64>> {
    COUNTERS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Adds one to a counter.
pub fn inc(name: &'static str, labels: &[(&'static str, &'static str)]) {
    *counters().entry((name, labels.to_vec())).or_default() += 1;
}

/// The current value of a counter (tests).
pub fn counter(name: &'static str, labels: &[(&'static str, &'static str)]) -> u64 {
    counters()
        .get(&(name, labels.to_vec()))
        .copied()
        .unwrap_or(0)
}

/// The Prometheus text exposition of every counter.
pub fn render() -> String {
    let mut out = String::new();
    let mut last = "";
    for ((name, labels), value) in counters().iter() {
        if *name != last {
            let _ = writeln!(out, "# TYPE {name} counter");
            last = name;
        }
        let labels: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
        if labels.is_empty() {
            let _ = writeln!(out, "{name} {value}");
        } else {
            let _ = writeln!(out, "{name}{{{}}} {value}", labels.join(","));
        }
    }
    out
}

/// The metrics listener's router (`GET /metrics` only).
pub fn router() -> Router {
    Router::new().route(
        "/metrics",
        get(|| async {
            (
                [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
                render(),
            )
                .into_response()
        }),
    )
}
