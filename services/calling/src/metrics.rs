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

/// Fixed histogram buckets (seconds): waits, rings and durations of calling flows.
pub const BUCKETS: &[f64] = &[
    0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
];

#[derive(Debug, Clone, Default)]
struct Histogram {
    buckets: Vec<u64>,
    count: u64,
    sum: f64,
}

static HISTOGRAMS: LazyLock<Mutex<BTreeMap<Key, Histogram>>> = LazyLock::new(Mutex::default);

fn histograms() -> std::sync::MutexGuard<'static, BTreeMap<Key, Histogram>> {
    HISTOGRAMS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Records one observation (seconds) of a histogram with bounded labels.
pub fn observe(name: &'static str, labels: &[(&'static str, &'static str)], value: f64) {
    let value = if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    };
    let mut all = histograms();
    let h = all.entry((name, labels.to_vec())).or_default();
    if h.buckets.is_empty() {
        h.buckets = vec![0; BUCKETS.len()];
    }
    for (i, bound) in BUCKETS.iter().enumerate() {
        if value <= *bound {
            h.buckets[i] += 1;
        }
    }
    h.count += 1;
    h.sum += value;
}

/// The number of observations of a histogram (tests).
pub fn observations(name: &'static str, labels: &[(&'static str, &'static str)]) -> u64 {
    histograms()
        .get(&(name, labels.to_vec()))
        .map(|h| h.count)
        .unwrap_or(0)
}

/// The Prometheus text exposition of every counter and histogram.
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
    let mut last = "";
    for ((name, labels), h) in histograms().iter() {
        if *name != last {
            let _ = writeln!(out, "# TYPE {name} histogram");
            last = name;
        }
        let base: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
        let with = |extra: String| {
            let mut all = base.clone();
            all.push(extra);
            all.join(",")
        };
        for (bound, n) in BUCKETS.iter().zip(&h.buckets) {
            let _ = writeln!(
                out,
                "{name}_bucket{{{}}} {n}",
                with(format!("le=\"{bound}\""))
            );
        }
        let _ = writeln!(
            out,
            "{name}_bucket{{{}}} {}",
            with("le=\"+Inf\"".into()),
            h.count
        );
        let labels = base.join(",");
        if labels.is_empty() {
            let _ = writeln!(out, "{name}_sum {}", h.sum);
            let _ = writeln!(out, "{name}_count {}", h.count);
        } else {
            let _ = writeln!(out, "{name}_sum{{{labels}}} {}", h.sum);
            let _ = writeln!(out, "{name}_count{{{labels}}} {}", h.count);
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
