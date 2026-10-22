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
use uuid::Uuid;

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

/// Latency buckets in seconds (upper bounds; `+Inf` is implicit).
pub const BUCKETS: [f64; 12] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

#[derive(Debug, Default, Clone)]
struct Histogram {
    buckets: [u64; BUCKETS.len()],
    count: u64,
    sum: f64,
}

static HISTOGRAMS: LazyLock<Mutex<BTreeMap<(&'static str, Labels), Histogram>>> =
    LazyLock::new(Mutex::default);

/// Records one observation (seconds) in a bounded-bucket histogram.
pub fn observe(name: &'static str, labels: &[(&'static str, &'static str)], seconds: f64) {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    };
    let mut all = HISTOGRAMS.lock().unwrap_or_else(|p| p.into_inner());
    let h = all.entry((name, labels.to_vec())).or_default();
    for (i, upper) in BUCKETS.iter().enumerate() {
        if seconds <= *upper {
            h.buckets[i] += 1;
        }
    }
    h.count += 1;
    h.sum += seconds;
}

/// Observations of a histogram (tests).
pub fn observations(name: &'static str, labels: &[(&'static str, &'static str)]) -> u64 {
    HISTOGRAMS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(name, labels.to_vec()))
        .map(|h| h.count)
        .unwrap_or(0)
}

fn render_histograms(out: &mut String) {
    let all = HISTOGRAMS.lock().unwrap_or_else(|p| p.into_inner());
    let mut last = "";
    for ((name, labels), h) in all.iter() {
        if *name != last {
            let _ = writeln!(out, "# TYPE {name} histogram");
            last = name;
        }
        let base: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
        let with = |extra: String| {
            let mut l = base.clone();
            l.push(extra);
            format!("{{{}}}", l.join(","))
        };
        for (i, upper) in BUCKETS.iter().enumerate() {
            let _ = writeln!(
                out,
                "{name}_bucket{} {}",
                with(format!("le=\"{upper}\"")),
                h.buckets[i]
            );
        }
        let _ = writeln!(
            out,
            "{name}_bucket{} {}",
            with("le=\"+Inf\"".into()),
            h.count
        );
        let _ = writeln!(out, "{name}_sum{} {}", label_text(labels), h.sum);
        let _ = writeln!(out, "{name}_count{} {}", label_text(labels), h.count);
    }
}

/// Route templates seen by [`route_template`] (bounded: past the cap every new template
/// is reported as `other`).
static ROUTES: LazyLock<Mutex<std::collections::BTreeSet<&'static str>>> =
    LazyLock::new(Mutex::default);
const MAX_ROUTE_TEMPLATES: usize = 512;

/// A bounded, id-free label for a request path: every segment that is not a short
/// lowercase word (an id, a reaction, a percent-encoded value) becomes `{id}`.
pub fn route_template(path: &str) -> &'static str {
    let template: String = path
        .split('/')
        .map(|seg| {
            // A route word: lowercase letters and hyphens, optionally one trailing digit
            // (`v1`). Ids, hex, reactions and encoded values never match.
            let body = seg
                .strip_suffix(|c: char| c.is_ascii_digit())
                .unwrap_or(seg);
            let word = !body.is_empty()
                && seg.len() <= 32
                && body.as_bytes()[0].is_ascii_lowercase()
                && body.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                && Uuid::parse_str(seg).is_err();
            if seg.is_empty() || word {
                seg
            } else {
                "{id}"
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    let mut routes = ROUTES.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(known) = routes.get(template.as_str()) {
        return known;
    }
    if routes.len() >= MAX_ROUTE_TEMPLATES {
        return "other";
    }
    let leaked: &'static str = Box::leak(template.into_boxed_str());
    routes.insert(leaked);
    leaked
}

fn method_label(m: &axum::http::Method) -> &'static str {
    match m.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    }
}

/// HTTP middleware: request latency by method, route template and status class
/// (`messaging_http_request_duration_seconds`; send / create / list / search latency).
pub async fn http_layer(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let started = std::time::Instant::now();
    let method = method_label(request.method());
    let route = route_template(request.uri().path());
    let response = next.run(request).await;
    let class = match response.status().as_u16() {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    };
    observe(
        "messaging_http_request_duration_seconds",
        &[("method", method), ("route", route), ("status", class)],
        started.elapsed().as_secs_f64(),
    );
    response
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
    render_histograms(&mut out);
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
    // Lags (seconds; 0 when nothing is waiting): the oldest committed event not yet
    // published, the oldest due purge, the oldest pending webhook relay.
    for (sql, metric) in [
        (
            "SELECT COALESCE(EXTRACT(EPOCH FROM now() - min(occurred_at)), 0)::bigint FROM messaging_outbox_events WHERE status = 'PENDING'",
            "messaging_outbox_oldest_pending_seconds",
        ),
        (
            "SELECT COALESCE(EXTRACT(EPOCH FROM now() - min(purge_after)), 0)::bigint FROM messaging_messages \
              WHERE purge_after <= now() AND lifecycle NOT IN ('ACTIVE', 'PURGED')",
            "messaging_purge_lag_seconds",
        ),
        (
            "SELECT COALESCE(EXTRACT(EPOCH FROM now() - min(created_at)), 0)::bigint FROM messaging_webhook_relay WHERE status = 'PENDING'",
            "messaging_webhook_relay_oldest_pending_seconds",
        ),
    ] {
        if let Ok(Some(row)) = db
            .query_one(Statement::from_string(DbBackend::Postgres, sql))
            .await
        {
            if let Ok(v) = row.try_get_by_index::<i64>(0) {
                let _ = writeln!(out, "# TYPE {metric} gauge\n{metric} {}", v.max(0));
            }
        }
    }
    let pool = db.get_postgres_connection_pool();
    let (size, idle) = (pool.size() as i64, pool.num_idle() as i64);
    let _ = writeln!(
        out,
        "# TYPE messaging_db_pool_connections gauge\nmessaging_db_pool_connections{{state=\"idle\"}} {idle}\nmessaging_db_pool_connections{{state=\"busy\"}} {}",
        (size - idle).max(0)
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_templates_never_carry_ids_and_stay_bounded() {
        let id = Uuid::new_v4();
        assert_eq!(
            route_template(&format!(
                "/v1/conversations/{id}/messages/{id}/reactions/:thumbsup:"
            )),
            "/v1/conversations/{id}/messages/{id}/reactions/{id}"
        );
        assert_eq!(
            route_template("/v1/messaging/search"),
            "/v1/messaging/search"
        );
        assert_eq!(
            route_template("/v1/messaging/backups/12345"),
            "/v1/messaging/backups/{id}"
        );
        assert_eq!(
            route_template("/v1/messaging/Search%20x"),
            "/v1/messaging/{id}"
        );
        assert_eq!(
            route_template("/v1/messaging/abc123def4567"),
            "/v1/messaging/{id}"
        );
        assert_eq!(
            route_template(&format!("/x/{}", Uuid::new_v4().simple())),
            "/x/{id}"
        );
    }

    #[test]
    fn histograms_render_cumulative_buckets_sum_and_count() {
        observe("test_latency_seconds", &[("op", "a")], 0.02);
        observe("test_latency_seconds", &[("op", "a")], 3.0);
        observe("test_latency_seconds", &[("op", "a")], f64::NAN);
        let mut out = String::new();
        render_histograms(&mut out);
        assert!(out.contains("# TYPE test_latency_seconds histogram"));
        assert!(
            out.contains("test_latency_seconds_bucket{op=\"a\",le=\"0.01\"} 1"),
            "{out}"
        );
        assert!(
            out.contains("test_latency_seconds_bucket{op=\"a\",le=\"0.025\"} 2"),
            "{out}"
        );
        assert!(
            out.contains("test_latency_seconds_bucket{op=\"a\",le=\"5\"} 3"),
            "{out}"
        );
        assert!(
            out.contains("test_latency_seconds_bucket{op=\"a\",le=\"+Inf\"} 3"),
            "{out}"
        );
        assert!(
            out.contains("test_latency_seconds_count{op=\"a\"} 3"),
            "{out}"
        );
        assert_eq!(observations("test_latency_seconds", &[("op", "a")]), 3);
    }
}
