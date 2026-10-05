//! QualityTelemetrySink — vendor-neutral telemetry boundary (Part I §62–§63, §80).
//!
//! High-frequency signals (WebRTC samples, latencies) go here, never into primary
//! meeting tables. Business code only sees this trait; the default implementation is
//! an in-process Prometheus registry exported at `GET /internal/metrics`.
//!
//! Labels are bounded enums only (outcome, kind, category, region bucket). No
//! participant id, email, phone, name or clinical data is ever a label; correlation
//! ids belong in logs / traces. A failing sink never affects a meeting: callers
//! ignore errors and the sink itself never panics.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JoinOutcome {
    /// Admitted + credential issued.
    Success,
    /// Authorization / policy rejection — never an infrastructure failure.
    Denied,
    ProviderFailure,
    Timeout,
    ClientFailure,
    /// Waiting room: neither success nor failure yet.
    Waiting,
}

impl JoinOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Denied => "denied",
            Self::ProviderFailure => "provider_failure",
            Self::Timeout => "timeout",
            Self::ClientFailure => "client_failure",
            Self::Waiting => "waiting",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MediaKind {
    Audio,
    Video,
    ScreenShare,
    Subscribe,
}

impl MediaKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::ScreenShare => "screen_share",
            Self::Subscribe => "subscribe",
        }
    }
}

/// Stable failure categories (raw provider strings are never exported).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailureCategory {
    PermissionDenied,
    ProviderUnavailable,
    Transport,
    Unknown,
}

impl FailureCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::Transport => "transport",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PipelineStage {
    Requested,
    Started,
    Ready,
    Failed,
}

impl PipelineStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Started => "started",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

/// One validated client / provider quality sample (technical metrics only).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct QualitySample {
    pub rtt_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub packet_loss_ratio: Option<f64>,
    pub bitrate_kbps: Option<f64>,
    pub fps: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

impl HealthState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "HEALTHY",
            Self::Degraded => "DEGRADED",
            Self::Unhealthy => "UNHEALTHY",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("telemetry sink unavailable")]
pub struct SinkUnavailable;

pub type SinkResult = Result<(), SinkUnavailable>;

pub trait QualityTelemetrySink: Send + Sync {
    fn record_sample(&self, sample: &QualitySample) -> SinkResult;
    fn record_join(&self, outcome: JoinOutcome, latency_ms: Option<f64>) -> SinkResult;
    fn record_media_connected(&self, join_latency_ms: Option<f64>) -> SinkResult;
    fn record_reconnect(&self) -> SinkResult;
    fn record_publish_failure(&self, kind: MediaKind, category: FailureCategory) -> SinkResult;
    fn record_subscription_failure(&self, category: FailureCategory) -> SinkResult;
    fn record_provider_drift(&self, kind: &'static str) -> SinkResult;
    fn record_recording(&self, stage: PipelineStage, latency_secs: Option<f64>) -> SinkResult;
    fn record_transcription(&self, stage: PipelineStage, latency_secs: Option<f64>) -> SinkResult;
    fn record_conference_health(&self, state: HealthState) -> SinkResult;
    /// Prometheus text exposition (empty when the sink cannot export).
    fn export(&self) -> String {
        String::new()
    }
}

/// Fixed-bucket histogram (cumulative buckets rendered at export).
struct Histogram {
    bounds: &'static [f64],
    counts: Vec<AtomicU64>,
    sum_millis: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn new(bounds: &'static [f64]) -> Self {
        Self {
            bounds,
            counts: bounds.iter().map(|_| AtomicU64::new(0)).collect(),
            sum_millis: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn observe(&self, v: f64) {
        if !v.is_finite() || v < 0.0 {
            return;
        }
        for (i, b) in self.bounds.iter().enumerate() {
            if v <= *b {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
        self.sum_millis
            .fetch_add((v * 1000.0) as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn render(&self, out: &mut String, name: &str, help: &str) {
        let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} histogram");
        let mut cumulative = 0u64;
        for (i, b) in self.bounds.iter().enumerate() {
            cumulative += self.counts[i].load(Ordering::Relaxed);
            let _ = writeln!(out, "{name}_bucket{{le=\"{b}\"}} {cumulative}");
        }
        let total = self.count.load(Ordering::Relaxed);
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {total}");
        let _ = writeln!(
            out,
            "{name}_sum {}",
            self.sum_millis.load(Ordering::Relaxed) as f64 / 1000.0
        );
        let _ = writeln!(out, "{name}_count {total}");
    }
}

const LATENCY_SECONDS: &[f64] = &[0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 30.0, 60.0];
const RTT_MS: &[f64] = &[
    25.0, 50.0, 100.0, 200.0, 300.0, 500.0, 800.0, 1500.0, 3000.0,
];
const JITTER_MS: &[f64] = &[5.0, 10.0, 20.0, 30.0, 50.0, 100.0, 200.0];
const LOSS_RATIO: &[f64] = &[0.001, 0.005, 0.01, 0.03, 0.05, 0.1, 0.2, 0.5];
const BITRATE_BPS: &[f64] = &[
    50_000.0,
    150_000.0,
    300_000.0,
    600_000.0,
    1_200_000.0,
    2_500_000.0,
    5_000_000.0,
];
const PROCESSING_SECONDS: &[f64] = &[1.0, 5.0, 15.0, 30.0, 60.0, 300.0, 900.0, 3600.0];

/// In-process Prometheus registry (bounded label sets only).
pub struct PrometheusSink {
    join_attempts: Mutex<BTreeMap<JoinOutcome, u64>>,
    join_latency: Histogram,
    media_connected: AtomicU64,
    media_join_latency: Histogram,
    reconnects: AtomicU64,
    publish_failures: Mutex<BTreeMap<(MediaKind, FailureCategory), u64>>,
    subscription_failures: Mutex<BTreeMap<FailureCategory, u64>>,
    drift: Mutex<BTreeMap<&'static str, u64>>,
    rtt: Histogram,
    jitter: Histogram,
    loss: Histogram,
    bitrate: Histogram,
    samples: AtomicU64,
    recording: Mutex<BTreeMap<PipelineStage, u64>>,
    recording_processing: Histogram,
    transcription: Mutex<BTreeMap<PipelineStage, u64>>,
    transcription_processing: Histogram,
    health: Mutex<BTreeMap<HealthState, u64>>,
}

impl Default for PrometheusSink {
    fn default() -> Self {
        Self::new()
    }
}

fn bump<K: Ord>(m: &Mutex<BTreeMap<K, u64>>, k: K) {
    if let Ok(mut g) = m.lock() {
        *g.entry(k).or_insert(0) += 1;
    }
}

impl PrometheusSink {
    pub fn new() -> Self {
        Self {
            join_attempts: Mutex::new(BTreeMap::new()),
            join_latency: Histogram::new(LATENCY_SECONDS),
            media_connected: AtomicU64::new(0),
            media_join_latency: Histogram::new(LATENCY_SECONDS),
            reconnects: AtomicU64::new(0),
            publish_failures: Mutex::new(BTreeMap::new()),
            subscription_failures: Mutex::new(BTreeMap::new()),
            drift: Mutex::new(BTreeMap::new()),
            rtt: Histogram::new(RTT_MS),
            jitter: Histogram::new(JITTER_MS),
            loss: Histogram::new(LOSS_RATIO),
            bitrate: Histogram::new(BITRATE_BPS),
            samples: AtomicU64::new(0),
            recording: Mutex::new(BTreeMap::new()),
            recording_processing: Histogram::new(PROCESSING_SECONDS),
            transcription: Mutex::new(BTreeMap::new()),
            transcription_processing: Histogram::new(PROCESSING_SECONDS),
            health: Mutex::new(BTreeMap::new()),
        }
    }
}

impl QualityTelemetrySink for PrometheusSink {
    fn record_sample(&self, s: &QualitySample) -> SinkResult {
        self.samples.fetch_add(1, Ordering::Relaxed);
        if let Some(v) = s.rtt_ms {
            self.rtt.observe(v);
        }
        if let Some(v) = s.jitter_ms {
            self.jitter.observe(v);
        }
        if let Some(v) = s.packet_loss_ratio {
            self.loss.observe(v);
        }
        if let Some(v) = s.bitrate_kbps {
            self.bitrate.observe(v * 1000.0);
        }
        Ok(())
    }

    fn record_join(&self, outcome: JoinOutcome, latency_ms: Option<f64>) -> SinkResult {
        bump(&self.join_attempts, outcome);
        if let Some(ms) = latency_ms {
            self.join_latency.observe(ms / 1000.0);
        }
        Ok(())
    }

    fn record_media_connected(&self, join_latency_ms: Option<f64>) -> SinkResult {
        self.media_connected.fetch_add(1, Ordering::Relaxed);
        if let Some(ms) = join_latency_ms {
            self.media_join_latency.observe(ms / 1000.0);
        }
        Ok(())
    }

    fn record_reconnect(&self) -> SinkResult {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn record_publish_failure(&self, kind: MediaKind, category: FailureCategory) -> SinkResult {
        bump(&self.publish_failures, (kind, category));
        Ok(())
    }

    fn record_subscription_failure(&self, category: FailureCategory) -> SinkResult {
        bump(&self.subscription_failures, category);
        Ok(())
    }

    fn record_provider_drift(&self, kind: &'static str) -> SinkResult {
        bump(&self.drift, kind);
        Ok(())
    }

    fn record_recording(&self, stage: PipelineStage, latency_secs: Option<f64>) -> SinkResult {
        bump(&self.recording, stage);
        if let (PipelineStage::Ready, Some(s)) = (stage, latency_secs) {
            self.recording_processing.observe(s);
        }
        Ok(())
    }

    fn record_transcription(&self, stage: PipelineStage, latency_secs: Option<f64>) -> SinkResult {
        bump(&self.transcription, stage);
        if let (PipelineStage::Ready, Some(s)) = (stage, latency_secs) {
            self.transcription_processing.observe(s);
        }
        Ok(())
    }

    fn record_conference_health(&self, state: HealthState) -> SinkResult {
        bump(&self.health, state);
        Ok(())
    }

    fn export(&self) -> String {
        let mut out = String::new();
        let counter = |out: &mut String, name: &str, help: &str, v: u64| {
            let _ = writeln!(
                out,
                "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}"
            );
        };
        if let Ok(m) = self.join_attempts.lock() {
            let total: u64 = m.values().sum();
            counter(
                &mut out,
                "meeting_join_attempts_total",
                "Join attempts",
                total,
            );
            let ok = m.get(&JoinOutcome::Success).copied().unwrap_or(0);
            counter(
                &mut out,
                "meeting_join_success_total",
                "Joins that issued RTC credentials",
                ok,
            );
            let _ = writeln!(
                out,
                "# HELP meeting_join_failure_total Failed joins by outcome (denials excluded from SLI)\n# TYPE meeting_join_failure_total counter"
            );
            for (k, v) in m.iter().filter(|(k, _)| **k != JoinOutcome::Success) {
                let _ = writeln!(
                    out,
                    "meeting_join_failure_total{{outcome=\"{}\"}} {v}",
                    k.as_str()
                );
            }
        }
        self.join_latency.render(
            &mut out,
            "meeting_join_latency_seconds",
            "Join request to credential issued",
        );
        counter(
            &mut out,
            "meeting_join_media_connected_total",
            "Provider-confirmed media connections",
            self.media_connected.load(Ordering::Relaxed),
        );
        self.media_join_latency.render(
            &mut out,
            "meeting_join_media_latency_seconds",
            "Join request to usable media (provider connection)",
        );
        counter(
            &mut out,
            "rtc_reconnects_total",
            "Participant reconnects",
            self.reconnects.load(Ordering::Relaxed),
        );
        if let Ok(m) = self.publish_failures.lock() {
            let _ = writeln!(
                out,
                "# HELP rtc_publish_failures_total Publish failures\n# TYPE rtc_publish_failures_total counter"
            );
            for ((k, c), v) in m.iter() {
                let _ = writeln!(
                    out,
                    "rtc_publish_failures_total{{kind=\"{}\",category=\"{}\"}} {v}",
                    k.as_str(),
                    c.as_str()
                );
            }
        }
        if let Ok(m) = self.subscription_failures.lock() {
            let _ = writeln!(
                out,
                "# HELP rtc_subscription_failures_total Subscription failures\n# TYPE rtc_subscription_failures_total counter"
            );
            for (c, v) in m.iter() {
                let _ = writeln!(
                    out,
                    "rtc_subscription_failures_total{{category=\"{}\"}} {v}",
                    c.as_str()
                );
            }
        }
        if let Ok(m) = self.drift.lock() {
            let _ = writeln!(
                out,
                "# HELP rtc_provider_drift_total Provider drift detections\n# TYPE rtc_provider_drift_total counter"
            );
            for (k, v) in m.iter() {
                let _ = writeln!(out, "rtc_provider_drift_total{{kind=\"{k}\"}} {v}");
            }
        }
        self.rtt
            .render(&mut out, "rtc_rtt_ms", "Sampled WebRTC round-trip time");
        self.jitter
            .render(&mut out, "rtc_jitter_ms", "Sampled jitter");
        self.loss.render(
            &mut out,
            "rtc_packet_loss_ratio",
            "Sampled packet loss ratio",
        );
        self.bitrate
            .render(&mut out, "rtc_bitrate_bps", "Sampled bitrate");
        counter(
            &mut out,
            "rtc_quality_samples_total",
            "Accepted quality samples",
            self.samples.load(Ordering::Relaxed),
        );
        for (prefix, map, hist) in [
            ("recording", &self.recording, &self.recording_processing),
            (
                "transcription",
                &self.transcription,
                &self.transcription_processing,
            ),
        ] {
            if let Ok(m) = map.lock() {
                let get = |s: PipelineStage| m.get(&s).copied().unwrap_or(0);
                counter(
                    &mut out,
                    &format!("{prefix}_requested_total"),
                    "Requested",
                    get(PipelineStage::Requested),
                );
                counter(
                    &mut out,
                    &format!("{prefix}_start_total"),
                    "Provider-confirmed starts",
                    get(PipelineStage::Started),
                );
                counter(
                    &mut out,
                    &format!("{prefix}_ready_total"),
                    "Ready artefacts",
                    get(PipelineStage::Ready),
                );
                counter(
                    &mut out,
                    &format!("{prefix}_failure_total"),
                    "Failures",
                    get(PipelineStage::Failed),
                );
            }
            hist.render(
                &mut out,
                &format!("{prefix}_processing_seconds"),
                "Capture end to READY",
            );
        }
        if let Ok(m) = self.health.lock() {
            let _ = writeln!(
                out,
                "# HELP conference_health_evaluations_total Health evaluations by state\n# TYPE conference_health_evaluations_total counter"
            );
            for (k, v) in m.iter() {
                let _ = writeln!(
                    out,
                    "conference_health_evaluations_total{{state=\"{}\"}} {v}",
                    k.as_str()
                );
            }
        }
        out
    }
}

/// Test / outage double: every call fails (meetings must keep working).
#[derive(Default)]
pub struct FailingSink {
    pub calls: AtomicU64,
    pub down: AtomicBool,
}

impl QualityTelemetrySink for FailingSink {
    fn record_sample(&self, _: &QualitySample) -> SinkResult {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(SinkUnavailable)
    }
    fn record_join(&self, _: JoinOutcome, _: Option<f64>) -> SinkResult {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(SinkUnavailable)
    }
    fn record_media_connected(&self, _: Option<f64>) -> SinkResult {
        Err(SinkUnavailable)
    }
    fn record_reconnect(&self) -> SinkResult {
        Err(SinkUnavailable)
    }
    fn record_publish_failure(&self, _: MediaKind, _: FailureCategory) -> SinkResult {
        Err(SinkUnavailable)
    }
    fn record_subscription_failure(&self, _: FailureCategory) -> SinkResult {
        Err(SinkUnavailable)
    }
    fn record_provider_drift(&self, _: &'static str) -> SinkResult {
        Err(SinkUnavailable)
    }
    fn record_recording(&self, _: PipelineStage, _: Option<f64>) -> SinkResult {
        Err(SinkUnavailable)
    }
    fn record_transcription(&self, _: PipelineStage, _: Option<f64>) -> SinkResult {
        Err(SinkUnavailable)
    }
    fn record_conference_health(&self, _: HealthState) -> SinkResult {
        Err(SinkUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_uses_bounded_labels_only() {
        let s = PrometheusSink::new();
        s.record_join(JoinOutcome::Success, Some(800.0)).unwrap();
        s.record_join(JoinOutcome::Denied, None).unwrap();
        s.record_publish_failure(MediaKind::Audio, FailureCategory::PermissionDenied)
            .unwrap();
        s.record_sample(&QualitySample {
            rtt_ms: Some(120.0),
            packet_loss_ratio: Some(0.02),
            ..Default::default()
        })
        .unwrap();
        s.record_recording(PipelineStage::Ready, Some(12.0))
            .unwrap();
        let text = s.export();
        assert!(text.contains("meeting_join_attempts_total 2"));
        assert!(text.contains("meeting_join_success_total 1"));
        assert!(text.contains("meeting_join_failure_total{outcome=\"denied\"} 1"));
        assert!(text.contains("rtc_rtt_ms_bucket{le=\"200\"} 1"));
        assert!(text.contains("recording_ready_total 1"));
        assert!(text.contains(
            "rtc_publish_failures_total{kind=\"audio\",category=\"permission_denied\"} 1"
        ));
        // No free-form label values exist in the exposition.
        for line in text.lines().filter(|l| l.contains('{')) {
            assert!(
                !line.contains('@') && !line.contains("participant"),
                "unbounded label: {line}"
            );
        }
    }

    #[test]
    fn histogram_ignores_invalid_values() {
        let h = Histogram::new(RTT_MS);
        h.observe(f64::NAN);
        h.observe(-1.0);
        h.observe(f64::INFINITY);
        assert_eq!(h.count.load(Ordering::Relaxed), 0);
    }
}
