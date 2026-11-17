//! C8 — per-participant call-quality samples (`POST /v1/calls/{id}/sessions/{session_id}/
//! quality-samples`) and the per-participant quality summary (`GET /v1/calls/{id}/quality`).
//!
//! A sample is what a client measured on ONE of its media sessions over a short window:
//! jitter (ms), packet loss (% of packets) and round-trip time (ms). Every value is
//! validated (finite, within physical bounds) and stored as an immutable
//! `call_quality_records` row of that session (`jitter_ms`, `packet_loss_pct`, `rtt_ms`
//! and the server-computed `mos_estimate`), with its CALL_QUALITY_RECORDED event — the
//! same provider-independent record the end-of-call summary uses. A session holds at most
//! [`MAX_SAMPLES_PER_SESSION`] samples (409 beyond: one sample every ten seconds for two
//! hours), and only a live session (not ENDED) of a Call whose media is live may report.
//!
//! The estimated MOS follows the simplified ITU-T G.107 E-model commonly used for VoIP
//! monitoring: effective latency = RTT / 2 + 2 × jitter + 10 ms; R = 93.2 − latency
//! penalty − 2.5 × loss %; MOS = 1 + 0.035 R + 7·10⁻⁶ R (R − 60)(100 − R), clamped to
//! [1, 4.5]. It is an estimate for monitoring and support, never a billing input.

use chrono::Duration;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, QuerySelect,
    TransactionTrait,
};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{enterprise::stmt, AppCallService};
use crate::{
    calls::{
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallId, CallSessionId},
        kinds::SessionState,
        quality::{NewQualityRecord, QualityMetrics},
    },
    media::permissions::media_state,
    models::call_sessions,
};

/// Samples one session may record.
pub const MAX_SAMPLES_PER_SESSION: i64 = 720;
/// Bounds of one sample.
pub const MAX_JITTER_MS: f64 = 10_000.0;
pub const MAX_RTT_MS: f64 = 60_000.0;
pub const DEFAULT_INTERVAL_SECONDS: i64 = 10;
pub const MAX_INTERVAL_SECONDS: i64 = 300;

/// One client measurement (validated by [`QualitySample::validate`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualitySample {
    pub jitter_ms: f64,
    pub packet_loss_percent: f64,
    pub rtt_ms: f64,
    /// Length of the observation window ending now (1–300 s, default 10).
    pub interval_seconds: Option<i64>,
}

impl QualitySample {
    pub fn validate(&self) -> CallResult<()> {
        let within = |v: f64, max: f64| v.is_finite() && (0.0..=max).contains(&v);
        if !within(self.jitter_ms, MAX_JITTER_MS) {
            return Err(CallError::Validation(
                "jitterMs must be a number between 0 and 10000.",
            ));
        }
        if !within(self.packet_loss_percent, 100.0) {
            return Err(CallError::Validation(
                "packetLossPercent must be a number between 0 and 100.",
            ));
        }
        if !within(self.rtt_ms, MAX_RTT_MS) {
            return Err(CallError::Validation(
                "rttMs must be a number between 0 and 60000.",
            ));
        }
        if self
            .interval_seconds
            .is_some_and(|s| !(1..=MAX_INTERVAL_SECONDS).contains(&s))
        {
            return Err(CallError::Validation(
                "intervalSeconds must be between 1 and 300.",
            ));
        }
        Ok(())
    }
}

/// The estimated MOS (1.0–4.5) of a sample (simplified E-model, see the module docs).
pub fn estimated_mos(jitter_ms: f64, packet_loss_percent: f64, rtt_ms: f64) -> f64 {
    let latency = rtt_ms / 2.0 + 2.0 * jitter_ms + 10.0;
    let mut r = if latency < 160.0 {
        93.2 - latency / 40.0
    } else {
        93.2 - (latency - 120.0) / 10.0
    };
    r -= 2.5 * packet_loss_percent;
    if r <= 0.0 {
        return 1.0;
    }
    let mos = 1.0 + 0.035 * r + 0.000_007 * r * (r - 60.0) * (100.0 - r);
    round2(mos.clamp(1.0, 4.5))
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[derive(Debug, FromQueryResult)]
struct SummaryRow {
    participant_id: Uuid,
    samples: i64,
    jitter_avg: f64,
    jitter_max: f64,
    loss_avg: f64,
    loss_max: f64,
    rtt_avg: f64,
    rtt_max: f64,
    mos_avg: f64,
    mos_min: f64,
    last_observed_at: chrono::DateTime<chrono::Utc>,
}

/// One participant's aggregate (`GET /v1/calls/{id}/quality`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantQuality {
    pub participant_id: Uuid,
    pub sample_count: i64,
    pub jitter_ms: Aggregate,
    pub packet_loss_percent: Aggregate,
    pub rtt_ms: Aggregate,
    pub mos_estimate: MosAggregate,
    pub last_observed_at: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Aggregate {
    pub avg: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MosAggregate {
    pub avg: f64,
    pub min: f64,
}

impl AppCallService {
    /// `POST /v1/calls/{call_id}/sessions/{session_id}/quality-samples` (C8).
    pub async fn record_quality_sample(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        session_id: CallSessionId,
        sample: QualitySample,
    ) -> CallResult<Value> {
        sample.validate()?;
        let call = self.calls_row(cmd.scope(), call_id).await?;
        let txn = self.db().begin().await?;
        // The session row lock serializes the session's samples (the bound holds under
        // concurrency).
        let session = call_sessions::Entity::find_by_id(session_id.as_uuid())
            .filter(call_sessions::Column::CallId.eq(call.id))
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        if session.state == SessionState::Ended {
            return Err(CallError::InvalidState("the session has ended"));
        }
        if !media_state(call.current_state) {
            return Err(CallError::InvalidState("the call's media is not live"));
        }
        let recorded: i64 = txn
            .query_one(stmt(
                "SELECT count(*) FROM call_quality_records \
                 WHERE call_id = $1 AND session_id = $2 AND metrics ? 'mos_estimate'",
                vec![call.id.into(), session.id.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index(0))
            .transpose()?
            .unwrap_or(0);
        if recorded >= MAX_SAMPLES_PER_SESSION {
            return Err(CallError::Conflict(
                "the session reached its quality sample limit",
            ));
        }
        let mos = estimated_mos(sample.jitter_ms, sample.packet_loss_percent, sample.rtt_ms);
        let metrics = QualityMetrics::new()
            .with("jitter_ms", round2(sample.jitter_ms))?
            .with("packet_loss_pct", round2(sample.packet_loss_percent))?
            .with("rtt_ms", round2(sample.rtt_ms))?
            .with("mos_estimate", mos)?;
        let now = self.calls.now();
        let interval = sample.interval_seconds.unwrap_or(DEFAULT_INTERVAL_SECONDS);
        let record = self
            .calls
            .record_quality_in(
                &txn,
                cmd,
                &call,
                NewQualityRecord {
                    session_id: Some(CallSessionId::from_uuid(session.id)),
                    leg_id: None,
                    observed_from: now - Duration::seconds(interval),
                    observed_to: now,
                    metrics,
                },
            )
            .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_quality_samples_total",
            &[("call_kind", call.call_kind.as_str())],
        );
        Ok(json!({
            "qualityRecordId": record.id,
            "callId": call.id,
            "sessionId": session.id,
            "participantId": session.participant_id,
            "jitterMs": round2(sample.jitter_ms),
            "packetLossPercent": round2(sample.packet_loss_percent),
            "rttMs": round2(sample.rtt_ms),
            "mosEstimate": mos,
            "observedFrom": crate::calls::read_model::ts(record.observed_from),
            "observedTo": crate::calls::read_model::ts(record.observed_to),
        }))
    }

    /// `GET /v1/calls/{call_id}/quality` (C8): per-participant aggregates of the samples.
    pub async fn quality_summary(&self, scope: CallScope, call_id: CallId) -> CallResult<Value> {
        let call = self.calls_row(scope, call_id).await?;
        let rows = SummaryRow::find_by_statement(stmt(
            "SELECT s.participant_id, count(*) AS samples, \
                    avg((r.metrics->>'jitter_ms')::float8) AS jitter_avg, \
                    max((r.metrics->>'jitter_ms')::float8) AS jitter_max, \
                    avg((r.metrics->>'packet_loss_pct')::float8) AS loss_avg, \
                    max((r.metrics->>'packet_loss_pct')::float8) AS loss_max, \
                    avg((r.metrics->>'rtt_ms')::float8) AS rtt_avg, \
                    max((r.metrics->>'rtt_ms')::float8) AS rtt_max, \
                    avg((r.metrics->>'mos_estimate')::float8) AS mos_avg, \
                    min((r.metrics->>'mos_estimate')::float8) AS mos_min, \
                    max(r.observed_to) AS last_observed_at \
               FROM call_quality_records r JOIN call_sessions s ON s.id = r.session_id \
              WHERE r.call_id = $1 AND r.metrics ? 'mos_estimate' \
              GROUP BY s.participant_id ORDER BY s.participant_id",
            vec![call.id.into()],
        ))
        .all(self.db())
        .await?;
        let participants: Vec<ParticipantQuality> = rows
            .into_iter()
            .map(|r| ParticipantQuality {
                participant_id: r.participant_id,
                sample_count: r.samples,
                jitter_ms: Aggregate {
                    avg: round2(r.jitter_avg),
                    max: round2(r.jitter_max),
                },
                packet_loss_percent: Aggregate {
                    avg: round2(r.loss_avg),
                    max: round2(r.loss_max),
                },
                rtt_ms: Aggregate {
                    avg: round2(r.rtt_avg),
                    max: round2(r.rtt_max),
                },
                mos_estimate: MosAggregate {
                    avg: round2(r.mos_avg),
                    min: round2(r.mos_min),
                },
                last_observed_at: crate::calls::read_model::ts(r.last_observed_at),
            })
            .collect();
        Ok(json!({ "callId": call.id, "participants": participants }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mos_is_high_for_a_clean_path_and_falls_with_impairment() {
        let clean = estimated_mos(2.0, 0.0, 40.0);
        assert!(clean >= 4.3 && clean <= 4.5, "{clean}");
        let lossy = estimated_mos(2.0, 5.0, 40.0);
        let laggy = estimated_mos(2.0, 0.0, 800.0);
        let jittery = estimated_mos(120.0, 0.0, 40.0);
        for worse in [lossy, laggy, jittery] {
            assert!(worse < clean, "{worse} < {clean}");
            assert!((1.0..=4.5).contains(&worse));
        }
        assert_eq!(estimated_mos(10_000.0, 100.0, 60_000.0), 1.0);
        // Monotonic in loss.
        let mut last = 5.0;
        for loss in [0.0, 1.0, 2.0, 5.0, 10.0, 20.0, 40.0] {
            let m = estimated_mos(5.0, loss, 60.0);
            assert!(m <= last, "{loss}: {m} > {last}");
            last = m;
        }
    }

    #[test]
    fn samples_are_bounded_and_finite() {
        let ok = QualitySample {
            jitter_ms: 12.5,
            packet_loss_percent: 0.4,
            rtt_ms: 80.0,
            interval_seconds: None,
        };
        assert!(ok.validate().is_ok());
        for bad in [
            QualitySample {
                jitter_ms: -1.0,
                ..ok
            },
            QualitySample {
                jitter_ms: f64::NAN,
                ..ok
            },
            QualitySample {
                packet_loss_percent: 100.5,
                ..ok
            },
            QualitySample {
                rtt_ms: f64::INFINITY,
                ..ok
            },
            QualitySample {
                rtt_ms: 60_001.0,
                ..ok
            },
            QualitySample {
                interval_seconds: Some(0),
                ..ok
            },
            QualitySample {
                interval_seconds: Some(301),
                ..ok
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
