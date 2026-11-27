//! Prometheus counters for the loopback `/metrics` (outcome codes only; no identities,
//! no scopes, no values).

use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct Metrics {
    outcomes: Mutex<BTreeMap<&'static str, u64>>,
}

impl Metrics {
    pub fn count(&self, outcome: &'static str) {
        *self
            .outcomes
            .lock()
            .expect("metrics")
            .entry(outcome)
            .or_insert(0) += 1;
    }

    pub fn get(&self, outcome: &str) -> u64 {
        self.outcomes
            .lock()
            .expect("metrics")
            .get(outcome)
            .copied()
            .unwrap_or(0)
    }

    pub fn render(&self, policy_loaded: bool, enabled: bool) -> String {
        let mut s = String::from("# HELP admin_bridge_requests_total HQ requests by outcome code.\n# TYPE admin_bridge_requests_total counter\n");
        for (k, v) in self.outcomes.lock().expect("metrics").iter() {
            s.push_str(&format!(
                "admin_bridge_requests_total{{outcome=\"{k}\"}} {v}\n"
            ));
        }
        s.push_str(&format!(
            "# HELP admin_bridge_enabled ADMIN_BRIDGE_ENABLED.\n# TYPE admin_bridge_enabled gauge\nadmin_bridge_enabled {}\n\
             # HELP admin_bridge_policy_loaded A verified access policy is loaded at the pinned revision.\n# TYPE admin_bridge_policy_loaded gauge\nadmin_bridge_policy_loaded {}\n",
            u8::from(enabled),
            u8::from(policy_loaded)
        ));
        s
    }
}
