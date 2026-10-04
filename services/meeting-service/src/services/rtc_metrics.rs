//! Process-wide RTC control-plane counters (Part E observability).
//!
//! Exposed through `GET /internal/rtc/metrics` (internal token) and structured logs.
//! Counters are monotonic; alerting compares rates (e.g. `room_terminate_failures`).

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        #[derive(Default)]
        pub struct RtcMetrics { $(pub $name: AtomicU64,)* }

        impl RtcMetrics {
            pub const fn new() -> Self { Self { $($name: AtomicU64::new(0),)* } }
            pub fn snapshot(&self) -> Value {
                let mut map = Map::new();
                $(map.insert(stringify!($name).to_string(), Value::from(self.$name.load(Ordering::Relaxed)));)*
                Value::Object(map)
            }
        }
    };
}

counters!(
    provider_errors,
    room_create_failures,
    room_terminate_failures,
    rooms_confirmed,
    rooms_terminated,
    credentials_issued,
    credentials_refused,
    participants_removed,
    permission_updates,
    tracks_muted,
    webhooks_received,
    webhooks_rejected,
    webhooks_duplicate,
    webhooks_unknown_room,
    stale_sessions_evicted,
    drift_detected,
    drift_repaired,
    drift_escalated,
    reconciler_runs,
    outbox_published,
    outbox_retried,
    outbox_dead_lettered,
    realtime_publish_failures,
    ws_connections,
    ws_rejected,
    ws_resync_required,
);

pub static RTC_METRICS: RtcMetrics = RtcMetrics::new();

pub fn inc(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

pub fn add(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Ordering::Relaxed);
}
