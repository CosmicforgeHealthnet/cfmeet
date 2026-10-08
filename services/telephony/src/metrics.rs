//! Bounded operational metrics (Part E §192–§193).
//!
//! Counters and one latency histogram, kept in process and rendered in the Prometheus
//! text format on a SEPARATE listener (`TELEPHONY_METRICS_ADDR`; never on the provider
//! callback ingress, never on the internal API). Label values are `&'static str` by type:
//! a telephone number, ContactReference, Call / leg id, provider call id or SIP Call-ID
//! cannot become a label (they are runtime values), so cardinality stays bounded by the
//! normalized vocabularies. Gauges (route / provider health, circuits, live legs, open
//! incidents, outbox backlog) are computed from Telephony's own tables at scrape time,
//! as counts per normalized state — never per route or provider identity.

use std::{
    collections::BTreeMap,
    fmt::Write,
    sync::{LazyLock, Mutex},
};

use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use sea_orm::FromQueryResult;

use crate::{db, error::TelephonyResult, service::TelephonyService};

type Labels = Vec<(&'static str, &'static str)>;

/// Upper bounds (ms) of the call-setup latency histogram.
const LATENCY_BUCKETS_MS: [i64; 9] = [
    250, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 30_000, 60_000,
];

#[derive(Default)]
struct Registry {
    counters: BTreeMap<(&'static str, Labels), u64>,
    /// name -> (bucket counts, sum, count)
    histograms: BTreeMap<&'static str, (Vec<u64>, i64, u64)>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(Mutex::default);

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|p| p.into_inner())
}

/// Adds one to a counter.
pub fn inc(name: &'static str, labels: &[(&'static str, &'static str)]) {
    *registry()
        .counters
        .entry((name, labels.to_vec()))
        .or_default() += 1;
}

/// Records one latency observation (milliseconds, clamped at 0).
pub fn observe_ms(name: &'static str, ms: i64) {
    let ms = ms.max(0);
    let mut r = registry();
    let entry = r
        .histograms
        .entry(name)
        .or_insert_with(|| (vec![0; LATENCY_BUCKETS_MS.len()], 0, 0));
    for (i, bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
        if ms <= *bound {
            entry.0[i] += 1;
        }
    }
    entry.1 += ms;
    entry.2 += 1;
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

fn render_registry(out: &mut String) {
    let r = registry();
    let mut last = "";
    for ((name, labels), value) in &r.counters {
        if *name != last {
            let _ = writeln!(out, "# TYPE {name} counter");
            last = name;
        }
        let _ = writeln!(out, "{name}{} {value}", label_text(labels));
    }
    for (name, (buckets, sum, count)) in &r.histograms {
        let _ = writeln!(out, "# TYPE {name} histogram");
        for (bound, n) in LATENCY_BUCKETS_MS.iter().zip(buckets) {
            let _ = writeln!(out, "{name}_bucket{{le=\"{bound}\"}} {n}");
        }
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {count}");
        let _ = writeln!(out, "{name}_sum {sum}");
        let _ = writeln!(out, "{name}_count {count}");
    }
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

impl TelephonyService {
    /// The full scrape: counters, histograms and table-derived gauges.
    pub async fn metrics_text(&self) -> TelephonyResult<String> {
        let mut out = String::new();
        render_registry(&mut out);
        for (name, sql) in [
            (
                "telephony_routes",
                "SELECT health AS state, count(*) AS n FROM telecom_routes WHERE status = 'ENABLED' GROUP BY health",
            ),
            (
                "telephony_route_circuits",
                "SELECT circuit_state AS state, count(*) AS n FROM telecom_routes GROUP BY circuit_state",
            ),
            (
                "telephony_providers",
                "SELECT health AS state, count(*) AS n FROM telecom_providers WHERE status = 'ENABLED' GROUP BY health",
            ),
            (
                "telephony_provider_circuits",
                "SELECT circuit_state AS state, count(*) AS n FROM telecom_providers GROUP BY circuit_state",
            ),
            (
                "telephony_live_legs",
                "SELECT status AS state, count(*) AS n FROM telephony_legs WHERE status NOT IN ('ENDED', 'FAILED') GROUP BY status",
            ),
            (
                "telephony_open_incidents",
                "SELECT category AS state, count(*) AS n FROM telephony_incidents WHERE status = 'OPEN' GROUP BY category",
            ),
        ] {
            let rows: Vec<StateCount> = db::all(&self.db, sql, vec![]).await?;
            let _ = writeln!(out, "# TYPE {name} gauge");
            for r in rows.iter().filter(|r| bounded(&r.state)) {
                let _ = writeln!(out, "{name}{{state=\"{}\"}} {}", r.state, r.n);
            }
        }
        let backlog = db::scalar_i64(
            &self.db,
            "SELECT count(*) FROM telephony_outbox WHERE delivered_at IS NULL",
            vec![],
        )
        .await?;
        let _ = writeln!(out, "# TYPE telephony_outbox_backlog gauge");
        let _ = writeln!(out, "telephony_outbox_backlog {backlog}");
        Ok(out)
    }
}

async fn scrape(State(svc): State<TelephonyService>) -> impl IntoResponse {
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

/// The metrics listener's router (`GET /metrics` only).
pub fn router(svc: TelephonyService) -> Router {
    Router::new().route("/metrics", get(scrape)).with_state(svc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_and_histograms_render_with_bounded_labels() {
        inc("telephony_test_total", &[("outcome", "ACCEPTED")]);
        inc("telephony_test_total", &[("outcome", "ACCEPTED")]);
        observe_ms("telephony_test_latency_ms", 700);
        let mut out = String::new();
        render_registry(&mut out);
        assert!(
            out.contains("telephony_test_total{outcome=\"ACCEPTED\"} 2"),
            "{out}"
        );
        assert!(out.contains("telephony_test_latency_ms_bucket{le=\"1000\"} 1"));
        assert!(out.contains("telephony_test_latency_ms_bucket{le=\"500\"} 0"));
        assert!(bounded("HEALTHY") && !bounded("dp-a") && !bounded("+2348031234567"));
    }
}
