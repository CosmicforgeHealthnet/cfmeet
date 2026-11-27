//! Process metrics (Document 4 §13) in the Prometheus text format at `GET /metrics` on the
//! private listener. Counters are per replica; queue gauges (backlog, oldest age per lane,
//! HELD, dead letters) are read from the database at scrape time, so every replica
//! reports the same shared state. Labels carry categories, lanes, channels and error
//! classes only: never addresses, ids or content.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Mutex,
    },
};

#[derive(Default)]
pub struct Metrics {
    counters: Mutex<BTreeMap<(&'static str, String), u64>>,
    pub critical_send_without_lease: AtomicU64,
    pub breaker_open: AtomicI64,
}

impl Metrics {
    /// Increments `name{labels}` (labels already rendered, e.g. `lane="CRITICAL"`).
    pub fn inc(&self, name: &'static str, labels: impl Into<String>) {
        self.add(name, labels, 1);
    }

    pub fn add(&self, name: &'static str, labels: impl Into<String>, n: u64) {
        let mut c = self.counters.lock().expect("metrics");
        *c.entry((name, labels.into())).or_default() += n;
    }

    pub fn get(&self, name: &'static str, labels: &str) -> u64 {
        self.counters
            .lock()
            .expect("metrics")
            .get(&(name, labels.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// Counters in the text format (gauges are appended by the HTTP handler).
    pub fn render_counters(&self) -> String {
        let mut out = String::new();
        let c = self.counters.lock().expect("metrics");
        let mut last = "";
        for ((name, labels), v) in c.iter() {
            if *name != last {
                out.push_str(&format!("# TYPE {name} counter\n"));
                last = name;
            }
            if labels.is_empty() {
                out.push_str(&format!("{name} {v}\n"));
            } else {
                out.push_str(&format!("{name}{{{labels}}} {v}\n"));
            }
        }
        out.push_str("# TYPE notif_critical_send_without_lease_total counter\n");
        out.push_str(&format!(
            "notif_critical_send_without_lease_total {}\n",
            self.critical_send_without_lease.load(Ordering::Relaxed)
        ));
        out.push_str("# TYPE notif_email_breaker_open gauge\n");
        out.push_str(&format!(
            "notif_email_breaker_open {}\n",
            self.breaker_open.load(Ordering::Relaxed)
        ));
        out
    }
}

/// A label value safe for the text format (closed vocabularies only reach here).
pub fn label(v: &str) -> String {
    v.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(64)
        .collect()
}

/// Worker heartbeats per lane (reported by `/ready`).
#[derive(Default)]
pub struct Heartbeats {
    beats: Mutex<BTreeMap<&'static str, chrono::DateTime<chrono::Utc>>>,
}

impl Heartbeats {
    pub fn beat(&self, worker: &'static str) {
        self.beats
            .lock()
            .expect("heartbeats")
            .insert(worker, chrono::Utc::now());
    }

    pub fn snapshot(&self) -> BTreeMap<&'static str, chrono::DateTime<chrono::Utc>> {
        self.beats.lock().expect("heartbeats").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_render_in_text_format() {
        let m = Metrics::default();
        m.inc("notif_ingested_total", "lane=\"CRITICAL\"");
        m.inc("notif_ingested_total", "lane=\"CRITICAL\"");
        let t = m.render_counters();
        assert!(t.contains("notif_ingested_total{lane=\"CRITICAL\"} 2"));
        assert_eq!(label("a\"b{c}"), "abc");
    }
}
