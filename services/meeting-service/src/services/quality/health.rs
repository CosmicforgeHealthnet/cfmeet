//! Provider-neutral health classification (Part I §65). All thresholds come from
//! `QualityConfig` (environment, documented defaults) — never hard-coded in handlers.

use serde::Serialize;
use utoipa::ToSchema;

use super::sink::HealthState;
use crate::config::operations::{QualityConfig, Threshold};

/// Session-level aggregates used for classification.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionQuality {
    pub samples: i32,
    pub avg_rtt_ms: Option<f64>,
    pub avg_jitter_ms: Option<f64>,
    pub packet_loss_ratio: Option<f64>,
    pub avg_bitrate_kbps: Option<f64>,
    pub reconnects: i32,
    pub publish_failures: i32,
    pub subscribe_failures: i32,
}

fn grade(value: Option<f64>, t: Threshold) -> HealthState {
    match value {
        Some(v) if v >= t.unhealthy => HealthState::Unhealthy,
        Some(v) if v >= t.degraded => HealthState::Degraded,
        Some(_) => HealthState::Healthy,
        None => HealthState::Unknown,
    }
}

/// Lower-is-worse signal (bitrate): `degraded` > `unhealthy`.
fn grade_low(value: Option<f64>, t: Threshold) -> HealthState {
    match value {
        Some(v) if v <= t.unhealthy => HealthState::Unhealthy,
        Some(v) if v <= t.degraded => HealthState::Degraded,
        Some(_) => HealthState::Healthy,
        None => HealthState::Unknown,
    }
}

pub fn worst(states: impl IntoIterator<Item = HealthState>) -> HealthState {
    let mut out = HealthState::Unknown;
    for s in states {
        out = match (out, s) {
            (HealthState::Unhealthy, _) | (_, HealthState::Unhealthy) => HealthState::Unhealthy,
            (HealthState::Degraded, _) | (_, HealthState::Degraded) => HealthState::Degraded,
            (HealthState::Healthy, _) | (_, HealthState::Healthy) => HealthState::Healthy,
            _ => HealthState::Unknown,
        };
    }
    out
}

/// Classify one participant session (UNKNOWN until a signal exists).
pub fn classify_session(q: &SessionQuality, cfg: &QualityConfig) -> HealthState {
    let reconnects = (q.reconnects > 0).then_some(q.reconnects as f64);
    let failures = if q.publish_failures + q.subscribe_failures > 0 {
        HealthState::Degraded
    } else {
        HealthState::Unknown
    };
    worst([
        grade(q.avg_rtt_ms, cfg.rtt_ms),
        grade(q.avg_jitter_ms, cfg.jitter_ms),
        grade(q.packet_loss_ratio, cfg.packet_loss_ratio),
        grade_low(q.avg_bitrate_kbps, cfg.low_bitrate_kbps),
        grade(reconnects, cfg.reconnects),
        failures,
    ])
}

/// 0–100 quality score (100 = perfect) for support views.
pub fn quality_score(q: &SessionQuality, cfg: &QualityConfig) -> Option<i16> {
    let parts = [
        q.avg_rtt_ms
            .map(|v| 1.0 - (v / cfg.rtt_ms.unhealthy).min(1.0)),
        q.avg_jitter_ms
            .map(|v| 1.0 - (v / cfg.jitter_ms.unhealthy).min(1.0)),
        q.packet_loss_ratio
            .map(|v| 1.0 - (v / cfg.packet_loss_ratio.unhealthy).min(1.0)),
    ];
    let known: Vec<f64> = parts.iter().flatten().copied().collect();
    if known.is_empty() {
        return None;
    }
    Some(((known.iter().sum::<f64>() / known.len() as f64) * 100.0).round() as i16)
}

/// Conference-level inputs (all from PostgreSQL aggregates / incidents).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ConferenceSignals {
    pub live: bool,
    pub room_ready: bool,
    pub credentials_issued: i64,
    pub provider_failures: i64,
    pub timeouts: i64,
    pub active_sessions: i64,
    pub unhealthy_sessions: i64,
    pub degraded_sessions: i64,
    pub open_critical_incidents: i64,
    pub open_incidents: i64,
    pub recording_failures: i64,
    pub transcription_failures: i64,
    pub avg_join_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthReason {
    pub signal: String,
    pub state: String,
}

/// Join failure rate SLI: infrastructure failures over join attempts that reached the
/// provider stage (authorization denials are excluded from both sides).
pub fn join_failure_rate(s: &ConferenceSignals) -> Option<f64> {
    let failures = s.provider_failures + s.timeouts;
    let denominator = s.credentials_issued + s.provider_failures;
    (denominator > 0).then(|| failures as f64 / denominator as f64)
}

pub fn classify_conference(
    s: &ConferenceSignals,
    cfg: &QualityConfig,
) -> (HealthState, Vec<HealthReason>) {
    let mut reasons = vec![];
    let mut push = |signal: &str, state: HealthState| {
        if matches!(state, HealthState::Degraded | HealthState::Unhealthy) {
            reasons.push(HealthReason {
                signal: signal.to_string(),
                state: state.as_str().to_string(),
            });
        }
        state
    };
    let mut states = vec![];
    if s.live && !s.room_ready {
        states.push(push("RTC_ROOM_NOT_READY", HealthState::Unhealthy));
    }
    if s.open_critical_incidents > 0 {
        states.push(push("CRITICAL_INCIDENT", HealthState::Unhealthy));
    } else if s.open_incidents > 0 {
        states.push(push("OPEN_INCIDENT", HealthState::Degraded));
    }
    states.push(push(
        "JOIN_FAILURE_RATE",
        grade(join_failure_rate(s), cfg.join_failure_rate),
    ));
    states.push(push(
        "JOIN_LATENCY",
        grade(s.avg_join_latency_ms, cfg.join_latency_ms),
    ));
    if s.active_sessions > 0 {
        let unhealthy_ratio = s.unhealthy_sessions as f64 / s.active_sessions as f64;
        let impaired_ratio =
            (s.unhealthy_sessions + s.degraded_sessions) as f64 / s.active_sessions as f64;
        let st = grade(Some(unhealthy_ratio), cfg.unhealthy_participant_ratio);
        let st = if st == HealthState::Healthy
            && impaired_ratio >= cfg.unhealthy_participant_ratio.degraded
        {
            HealthState::Degraded
        } else {
            st
        };
        states.push(push("PARTICIPANT_QUALITY", st));
    }
    if s.recording_failures > 0 {
        states.push(push("RECORDING_FAILURE", HealthState::Degraded));
    }
    if s.transcription_failures > 0 {
        states.push(push("TRANSCRIPTION_FAILURE", HealthState::Degraded));
    }
    let mut state = worst(states);
    if state == HealthState::Unknown && s.live && s.room_ready {
        state = HealthState::Healthy;
    }
    (state, reasons)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_thresholds_classify_signals() {
        let cfg = QualityConfig::default();
        let good = SessionQuality {
            samples: 3,
            avg_rtt_ms: Some(80.0),
            avg_jitter_ms: Some(5.0),
            packet_loss_ratio: Some(0.001),
            avg_bitrate_kbps: Some(900.0),
            ..Default::default()
        };
        assert_eq!(classify_session(&good, &cfg), HealthState::Healthy);
        assert_eq!(quality_score(&good, &cfg).map(|s| s > 80), Some(true));
        let degraded = SessionQuality {
            avg_rtt_ms: Some(400.0),
            ..good
        };
        assert_eq!(classify_session(&degraded, &cfg), HealthState::Degraded);
        let unhealthy = SessionQuality {
            packet_loss_ratio: Some(0.25),
            ..good
        };
        assert_eq!(classify_session(&unhealthy, &cfg), HealthState::Unhealthy);
        let low_bitrate = SessionQuality {
            avg_bitrate_kbps: Some(20.0),
            ..good
        };
        assert_eq!(classify_session(&low_bitrate, &cfg), HealthState::Unhealthy);
        assert_eq!(
            classify_session(&SessionQuality::default(), &cfg),
            HealthState::Unknown
        );
    }

    #[test]
    fn conference_classification_uses_sli_and_incidents() {
        let cfg = QualityConfig::default();
        let base = ConferenceSignals {
            live: true,
            room_ready: true,
            credentials_issued: 100,
            active_sessions: 10,
            ..Default::default()
        };
        assert_eq!(classify_conference(&base, &cfg).0, HealthState::Healthy);
        let failing = ConferenceSignals {
            provider_failures: 40,
            ..base
        };
        let (state, reasons) = classify_conference(&failing, &cfg);
        assert_eq!(state, HealthState::Unhealthy);
        assert!(reasons.iter().any(|r| r.signal == "JOIN_FAILURE_RATE"));
        // Denials never count: they are not part of the SLI inputs at all.
        assert_eq!(join_failure_rate(&base), Some(0.0));
        let room_lost = ConferenceSignals {
            room_ready: false,
            ..base
        };
        assert_eq!(
            classify_conference(&room_lost, &cfg).0,
            HealthState::Unhealthy
        );
        let poor = ConferenceSignals {
            unhealthy_sessions: 6,
            ..base
        };
        assert_eq!(classify_conference(&poor, &cfg).0, HealthState::Unhealthy);
        let recording = ConferenceSignals {
            recording_failures: 1,
            ..base
        };
        assert_eq!(
            classify_conference(&recording, &cfg).0,
            HealthState::Degraded
        );
    }
}
