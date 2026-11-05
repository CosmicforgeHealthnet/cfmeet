//! Bounded in-process Prometheus metrics. Label values come only from the registry
//! (products), the closed verdict/reason vocabulary or fixed names — never from requests.

use std::{collections::BTreeMap, sync::Mutex};

const BUCKETS: [f64; 10] = [0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0];

/// Metric name and its label pairs.
type Series = (String, Vec<(&'static str, String)>);

#[derive(Default)]
struct Inner {
    counters: BTreeMap<Series, u64>,
    gauges: BTreeMap<Series, f64>,
    histograms: BTreeMap<String, ([u64; BUCKETS.len()], u64, f64)>,
}

#[derive(Default)]
pub struct Metrics {
    inner: Mutex<Inner>,
}

impl Metrics {
    pub fn inc(&self, name: &str, labels: &[(&'static str, &str)], by: u64) {
        let key = (
            name.to_string(),
            labels.iter().map(|(k, v)| (*k, v.to_string())).collect(),
        );
        *self
            .inner
            .lock()
            .expect("metrics")
            .counters
            .entry(key)
            .or_insert(0) += by;
    }

    pub fn set(&self, name: &str, labels: &[(&'static str, &str)], value: f64) {
        let key = (
            name.to_string(),
            labels.iter().map(|(k, v)| (*k, v.to_string())).collect(),
        );
        self.inner
            .lock()
            .expect("metrics")
            .gauges
            .insert(key, value);
    }

    pub fn add(&self, name: &str, labels: &[(&'static str, &str)], delta: f64) {
        let key = (
            name.to_string(),
            labels.iter().map(|(k, v)| (*k, v.to_string())).collect(),
        );
        *self
            .inner
            .lock()
            .expect("metrics")
            .gauges
            .entry(key)
            .or_insert(0.0) += delta;
    }

    /// Scan duration per product (`cosmicforge_scan_duration_seconds`).
    pub fn observe(&self, product: &str, seconds: f64) {
        let mut inner = self.inner.lock().expect("metrics");
        let h = inner
            .histograms
            .entry(product.to_string())
            .or_insert(([0; BUCKETS.len()], 0, 0.0));
        for (i, b) in BUCKETS.iter().enumerate() {
            if seconds <= *b {
                h.0[i] += 1;
            }
        }
        h.1 += 1;
        h.2 += seconds;
    }

    pub fn render(&self) -> String {
        fn labels(l: &[(&'static str, String)]) -> String {
            if l.is_empty() {
                return String::new();
            }
            let inner: Vec<String> = l.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
            format!("{{{}}}", inner.join(","))
        }
        let inner = self.inner.lock().expect("metrics");
        let mut out = String::new();
        let mut last = String::new();
        for ((name, l), v) in &inner.counters {
            if *name != last {
                out.push_str(&format!("# TYPE {name} counter\n"));
                last = name.clone();
            }
            out.push_str(&format!("{name}{} {v}\n", labels(l)));
        }
        for ((name, l), v) in &inner.gauges {
            if *name != last {
                out.push_str(&format!("# TYPE {name} gauge\n"));
                last = name.clone();
            }
            out.push_str(&format!("{name}{} {v}\n", labels(l)));
        }
        if !inner.histograms.is_empty() {
            out.push_str("# TYPE cosmicforge_scan_duration_seconds histogram\n");
        }
        for (product, (buckets, count, sum)) in &inner.histograms {
            for (i, b) in BUCKETS.iter().enumerate() {
                out.push_str(&format!(
                    "cosmicforge_scan_duration_seconds_bucket{{product=\"{product}\",le=\"{b}\"}} {}\n",
                    buckets[i]
                ));
            }
            out.push_str(&format!(
                "cosmicforge_scan_duration_seconds_bucket{{product=\"{product}\",le=\"+Inf\"}} {count}\n"
            ));
            out.push_str(&format!(
                "cosmicforge_scan_duration_seconds_sum{{product=\"{product}\"}} {sum}\n"
            ));
            out.push_str(&format!(
                "cosmicforge_scan_duration_seconds_count{{product=\"{product}\"}} {count}\n"
            ));
        }
        out
    }
}
