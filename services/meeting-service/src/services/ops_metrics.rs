//! Process-wide Parts G/H counters (recording, artefacts, consent, transcription,
//! intelligence, registration, promotion, backstage, profile authorization).
//!
//! Exposed through `GET /internal/rtc/metrics` and the Prometheus exporter
//! (`GET /internal/metrics`). Counters carry no labels and therefore no PII.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        #[derive(Default)]
        pub struct OpsMetrics { $(pub $name: AtomicU64,)* }

        impl OpsMetrics {
            pub const fn new() -> Self { Self { $($name: AtomicU64::new(0),)* } }
            pub fn snapshot(&self) -> Value {
                let mut map = Map::new();
                $(map.insert(stringify!($name).to_string(), Value::from(self.$name.load(Ordering::Relaxed)));)*
                Value::Object(map)
            }
            pub fn pairs(&self) -> Vec<(&'static str, u64)> {
                vec![$((stringify!($name), self.$name.load(Ordering::Relaxed)),)*]
            }
        }
    };
}

counters!(
    recording_requested,
    recording_started,
    recording_stop_requested,
    recording_processing,
    recording_ready,
    recording_failed,
    recording_provider_errors,
    recording_callbacks_duplicate,
    recording_callbacks_unknown,
    artifact_finalize_retries,
    artifact_integrity_failures,
    artifact_missing,
    artifact_access_granted,
    artifact_access_denied,
    artifact_sign_failures,
    artifact_generated,
    artifact_deleted,
    artifact_delete_failures,
    retention_runs,
    consent_required,
    consent_recorded,
    consent_declined,
    consent_withdrawn,
    transcription_started,
    transcription_ready,
    transcription_failed,
    transcription_segments_final,
    transcription_segments_duplicate,
    transcription_segments_late,
    transcription_callbacks_rejected,
    translation_requests,
    intelligence_dispatched,
    intelligence_blocked,
    intelligence_failed,
    registrations_created,
    registrations_rejected,
    promotions,
    demotions,
    stage_capacity_rejections,
    backstage_entries,
    backstage_exits,
    backstage_evictions,
    profile_policy_denials,
    enhanced_audit_records,
    // Parts J–L: migration / compatibility observation
    legacy_writes_rejected,
    shadow_reads,
    shadow_mismatches,
    shadow_failures,
);

pub static OPS_METRICS: OpsMetrics = OpsMetrics::new();

pub fn inc(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

pub fn add(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Ordering::Relaxed);
}
