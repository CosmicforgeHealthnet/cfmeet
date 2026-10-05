//! QualityService — client quality-sample ingestion and session aggregates (Part I
//! §58–§64, §82–§85).
//!
//! - Identity is server-derived: the participant comes from `ActorContext`, the
//!   session must be an active session OF THAT participant in THIS conference. The
//!   body never names a user or participant.
//! - Validation: bounded batch, finite numbers inside physical ranges, timestamps near
//!   server time, no free-form JSON (unknown fields rejected), small body limit.
//! - Rate limits per participant session (`QUALITY_RATE_LIMITED`).
//! - Raw samples go only to the telemetry sink; PostgreSQL receives one low-frequency
//!   aggregate update per batch (running averages, max, an RTT histogram for p95,
//!   counters). A repeated `batchId` is ignored, so sink / client retries never double
//!   count relational aggregates.
//! - Telemetry is secondary: sink failures are counted and swallowed; nothing here
//!   affects admission, credentials or meeting state.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    health::{classify_session, quality_score, SessionQuality},
    sink::{FailureCategory, MediaKind, QualitySample},
};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        actor::ActorContext,
        artifacts::{bump, ArtifactCtx, ConferenceCounter},
        collaboration::RateAction,
        meeting_core::Core,
        meeting_permissions::Permission,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaHealth {
    Ok,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NetworkType {
    Wifi,
    Cellular,
    Ethernet,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FailureKind {
    AudioPublish,
    VideoPublish,
    ScreenSharePublish,
    Subscribe,
    JoinMedia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FailureCategoryDto {
    PermissionDenied,
    ProviderUnavailable,
    Transport,
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaFailure {
    pub kind: FailureKind,
    pub category: FailureCategoryDto,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QualitySampleInput {
    pub timestamp: DateTime<Utc>,
    pub rtt_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub packet_loss_ratio: Option<f64>,
    pub bitrate_kbps: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub publish_health: Option<MediaHealth>,
    pub subscribe_health: Option<MediaHealth>,
    pub network_type: Option<NetworkType>,
    pub failure: Option<MediaFailure>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QualitySampleBatch {
    /// Optional: one of the caller's own active sessions (resolved server-side
    /// otherwise). Another participant's session is rejected.
    pub participant_session_id: Option<Uuid>,
    /// Client batch id (idempotency for retries; 1–64 chars).
    pub batch_id: Option<String>,
    /// Provider edge the client connected to (sanitized; operations-only).
    pub provider_region: Option<String>,
    pub provider_node: Option<String>,
    pub samples: Vec<QualitySampleInput>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QualityIngestResponse {
    pub participant_session_id: Uuid,
    pub accepted: usize,
    /// Samples the telemetry sink could not take (meeting unaffected).
    pub sink_dropped: usize,
    pub duplicate_batch: bool,
    pub quality_state: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceQualityView {
    pub conference_id: Uuid,
    pub health_state: String,
    pub active_sessions: i64,
    pub healthy_sessions: i64,
    pub degraded_sessions: i64,
    pub unhealthy_sessions: i64,
    pub avg_rtt_ms: Option<f64>,
    pub avg_jitter_ms: Option<f64>,
    pub avg_packet_loss_ratio: Option<f64>,
    pub reconnects: i64,
}

fn invalid(message: &str) -> ApiError {
    ApiError::domain(ErrorCode::QualitySampleInvalid, message)
}

fn sane_label(v: &Option<String>) -> ApiResult<Option<String>> {
    match v {
        None => Ok(None),
        Some(s)
            if !s.is_empty()
                && s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            Ok(Some(s.clone()))
        }
        Some(_) => Err(invalid(
            "providerRegion / providerNode must be short identifiers",
        )),
    }
}

fn check_range(name: &str, v: Option<f64>, max: f64) -> ApiResult<()> {
    if let Some(x) = v {
        if !x.is_finite() || x < 0.0 || x > max {
            return Err(invalid(&format!("{name} is out of range")));
        }
    }
    Ok(())
}

/// Validate one sample (finite, physical bounds, timestamp near server time).
pub fn validate_sample(
    s: &QualitySampleInput,
    now: DateTime<Utc>,
    skew_secs: i64,
) -> ApiResult<()> {
    if (s.timestamp - now).num_seconds().abs() > skew_secs {
        return Err(invalid("timestamp is too far from server time"));
    }
    check_range("rttMs", s.rtt_ms, 60_000.0)?;
    check_range("jitterMs", s.jitter_ms, 10_000.0)?;
    check_range("packetLossRatio", s.packet_loss_ratio, 1.0)?;
    check_range("bitrateKbps", s.bitrate_kbps, 100_000.0)?;
    check_range("fps", s.fps, 240.0)?;
    if s.width.map(|w| w > 7680).unwrap_or(false) || s.height.map(|h| h > 4320).unwrap_or(false) {
        return Err(invalid("resolution is out of range"));
    }
    Ok(())
}

/// RTT histogram buckets (ms) used for the persisted p95 estimate.
const RTT_BUCKETS: &[f64] = &[
    50.0,
    100.0,
    150.0,
    200.0,
    300.0,
    500.0,
    800.0,
    1500.0,
    f64::MAX,
];

fn p95(hist: &[i64]) -> Option<f64> {
    let total: i64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    let target = (total as f64 * 0.95).ceil() as i64;
    let mut acc = 0;
    for (i, c) in hist.iter().enumerate() {
        acc += c;
        if acc >= target {
            let b = RTT_BUCKETS[i.min(RTT_BUCKETS.len() - 1)];
            return Some(if b == f64::MAX { 3000.0 } else { b });
        }
    }
    None
}

pub struct QualityService {
    core: Core,
}

impl QualityService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    pub async fn ingest(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        batch: &QualitySampleBatch,
    ) -> ApiResult<QualityIngestResponse> {
        if !self.core.flags.quality_telemetry_enabled {
            return Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Quality telemetry is not enabled",
            ));
        }
        let cfg = &self.core.config.quality;
        if batch.samples.is_empty() || batch.samples.len() > cfg.max_samples_per_batch {
            return Err(invalid("samples must contain 1..=max batch size entries"));
        }
        if batch
            .batch_id
            .as_ref()
            .map(|b| b.is_empty() || b.len() > 64)
            .unwrap_or(false)
        {
            return Err(invalid("batchId must be 1-64 characters"));
        }
        let now = Utc::now();
        for s in &batch.samples {
            validate_sample(s, now, cfg.max_sample_skew_secs)?;
        }
        let region = sane_label(&batch.provider_region)?;
        let node = sane_label(&batch.provider_node)?;
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.clone();
        // The session is resolved server-side and must belong to the caller.
        let session_id: Option<Uuid> = {
            let row = self
                .core
                .db
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT id FROM participant_sessions
                      WHERE conference_id = $1 AND conference_participant_id = $2
                        AND state IN ('PENDING','CONNECTED')
                        AND ($3::uuid IS NULL OR id = $3)
                      ORDER BY established_at DESC LIMIT 1",
                    [
                        conference_id.into(),
                        me.id.into(),
                        batch.participant_session_id.into(),
                    ],
                ))
                .await?;
            row.and_then(|r| r.try_get_by_index(0).ok())
        };
        let Some(session_id) = session_id else {
            return Err(ApiError::domain(
                ErrorCode::ParticipantNotFound,
                "No active session of yours matches this conference",
            ));
        };
        self.core
            .collab_limiter
            .check(RateAction::QualitySamples, &session_id.to_string())
            .await
            .map_err(|e| match e {
                ApiError::RateLimited { .. } => ApiError::domain(
                    ErrorCode::QualityRateLimited,
                    "Too many quality batches; slow down",
                ),
                other => other,
            })?;
        let sink = self.core.telemetry.clone();
        let mut dropped = 0;
        let mut publish_failures = 0i32;
        let mut subscribe_failures = 0i32;
        let mut client_join_failures = 0i64;
        for s in &batch.samples {
            if sink
                .record_sample(&QualitySample {
                    rtt_ms: s.rtt_ms,
                    jitter_ms: s.jitter_ms,
                    packet_loss_ratio: s.packet_loss_ratio,
                    bitrate_kbps: s.bitrate_kbps,
                    fps: s.fps,
                })
                .is_err()
            {
                dropped += 1;
            }
            if let Some(f) = s.failure {
                let category = match f.category {
                    FailureCategoryDto::PermissionDenied => FailureCategory::PermissionDenied,
                    FailureCategoryDto::ProviderUnavailable => FailureCategory::ProviderUnavailable,
                    FailureCategoryDto::Transport => FailureCategory::Transport,
                    FailureCategoryDto::Unknown => FailureCategory::Unknown,
                };
                let _ = match f.kind {
                    FailureKind::AudioPublish => {
                        publish_failures += 1;
                        sink.record_publish_failure(MediaKind::Audio, category)
                    }
                    FailureKind::VideoPublish => {
                        publish_failures += 1;
                        sink.record_publish_failure(MediaKind::Video, category)
                    }
                    FailureKind::ScreenSharePublish => {
                        publish_failures += 1;
                        sink.record_publish_failure(MediaKind::ScreenShare, category)
                    }
                    FailureKind::Subscribe => {
                        subscribe_failures += 1;
                        sink.record_subscription_failure(category)
                    }
                    FailureKind::JoinMedia => {
                        client_join_failures += 1;
                        sink.record_join(super::sink::JoinOutcome::ClientFailure, None)
                    }
                };
            }
        }
        let quality_state = self
            .aggregate(
                conference_id,
                session_id,
                batch,
                region,
                node,
                publish_failures,
                subscribe_failures,
                client_join_failures,
            )
            .await?;
        let duplicate = quality_state.is_none();
        Ok(QualityIngestResponse {
            participant_session_id: session_id,
            accepted: batch.samples.len(),
            sink_dropped: dropped,
            duplicate_batch: duplicate,
            quality_state: quality_state.unwrap_or_else(|| "UNCHANGED".into()),
        })
    }

    /// One relational aggregate update per batch (row-locked, idempotent by batch id).
    #[allow(clippy::too_many_arguments)]
    async fn aggregate(
        &self,
        conference_id: Uuid,
        session_id: Uuid,
        batch: &QualitySampleBatch,
        region: Option<String>,
        node: Option<String>,
        publish_failures: i32,
        subscribe_failures: i32,
        client_join_failures: i64,
    ) -> ApiResult<Option<String>> {
        let cfg = &self.core.config.quality;
        let txn = self.core.db.begin().await?;
        let row = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT quality_samples, avg_rtt_ms, max_rtt_ms, avg_jitter_ms, packet_loss_pct,
                        avg_bitrate_kbps, min_fps, rtt_histogram, reconnect_count, publish_failures,
                        subscribe_failures, last_quality_batch
                   FROM participant_sessions WHERE id = $1 AND conference_id = $2 FOR UPDATE",
                [session_id.into(), conference_id.into()],
            ))
            .await?
            .ok_or_else(|| invalid("session not found"))?;
        let last_batch: Option<String> = row.try_get_by_index(11).ok().flatten();
        if let (Some(b), Some(last)) = (&batch.batch_id, &last_batch) {
            if b == last {
                txn.commit().await?;
                return Ok(None);
            }
        }
        let n: i32 = row.try_get_by_index(0).unwrap_or(0);
        let get = |i: usize| -> Option<f64> {
            row.try_get_by_index::<Option<f32>>(i)
                .ok()
                .flatten()
                .map(|v| v as f64)
        };
        let (avg_rtt, max_rtt, avg_jitter, loss_pct, avg_bitrate, min_fps) =
            (get(1), get(2), get(3), get(4), get(5), get(6));
        let mut hist: Vec<i64> = row
            .try_get_by_index::<serde_json::Value>(7)
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        hist.resize(RTT_BUCKETS.len(), 0);
        let reconnects: i32 = row.try_get_by_index(8).unwrap_or(0);
        let pf: i32 = row.try_get_by_index(9).unwrap_or(0);
        let sf: i32 = row.try_get_by_index(10).unwrap_or(0);
        // Running averages over the per-signal sample counts in this batch.
        let mean = |old: Option<f64>, old_n: i32, xs: Vec<f64>| -> Option<f64> {
            if xs.is_empty() {
                return old;
            }
            let k = xs.len() as f64;
            let sum: f64 = xs.iter().sum();
            match old {
                Some(o) if old_n > 0 => Some((o * old_n as f64 + sum) / (old_n as f64 + k)),
                _ => Some(sum / k),
            }
        };
        let rtts: Vec<f64> = batch.samples.iter().filter_map(|s| s.rtt_ms).collect();
        for r in &rtts {
            let idx = RTT_BUCKETS
                .iter()
                .position(|b| r <= b)
                .unwrap_or(RTT_BUCKETS.len() - 1);
            hist[idx] += 1;
        }
        let new_avg_rtt = mean(avg_rtt, n, rtts.clone());
        let new_max_rtt = rtts
            .iter()
            .copied()
            .fold(max_rtt, |m, r| Some(m.map(|x| x.max(r)).unwrap_or(r)));
        let new_jitter = mean(
            avg_jitter,
            n,
            batch.samples.iter().filter_map(|s| s.jitter_ms).collect(),
        );
        let new_loss = mean(
            loss_pct.map(|p| p / 100.0),
            n,
            batch
                .samples
                .iter()
                .filter_map(|s| s.packet_loss_ratio)
                .collect(),
        );
        let new_bitrate = mean(
            avg_bitrate,
            n,
            batch
                .samples
                .iter()
                .filter_map(|s| s.bitrate_kbps)
                .collect(),
        );
        let new_min_fps = batch
            .samples
            .iter()
            .filter_map(|s| s.fps)
            .fold(min_fps, |m, f| Some(m.map(|x| x.min(f)).unwrap_or(f)));
        let resolution = batch
            .samples
            .iter()
            .rev()
            .find_map(|s| s.width.zip(s.height))
            .map(|(w, h)| format!("{w}x{h}"));
        let q = SessionQuality {
            samples: n + batch.samples.len() as i32,
            avg_rtt_ms: new_avg_rtt,
            avg_jitter_ms: new_jitter,
            packet_loss_ratio: new_loss,
            avg_bitrate_kbps: new_bitrate,
            reconnects,
            publish_failures: pf + publish_failures,
            subscribe_failures: sf + subscribe_failures,
        };
        let state = classify_session(&q, cfg);
        let score = quality_score(&q, cfg);
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE participant_sessions
                SET quality_samples = $3, avg_rtt_ms = $4, max_rtt_ms = $5, p95_rtt_ms = $6,
                    avg_jitter_ms = $7, packet_loss_pct = $8, avg_bitrate_kbps = $9, min_fps = $10,
                    last_resolution = COALESCE($11, last_resolution), rtt_histogram = $12,
                    quality_score = $13, quality_state = $14, last_quality_at = now(),
                    last_quality_batch = COALESCE($15, last_quality_batch),
                    publish_failures = publish_failures + $16,
                    subscribe_failures = subscribe_failures + $17,
                    provider_region = COALESCE($18, provider_region),
                    provider_node = COALESCE($19, provider_node)
              WHERE id = $1 AND conference_id = $2",
            [
                session_id.into(),
                conference_id.into(),
                q.samples.into(),
                new_avg_rtt.map(|v| v as f32).into(),
                new_max_rtt.map(|v| v as f32).into(),
                p95(&hist).map(|v| v as f32).into(),
                new_jitter.map(|v| v as f32).into(),
                new_loss.map(|v| (v * 100.0) as f32).into(),
                new_bitrate.map(|v| v as f32).into(),
                new_min_fps.map(|v| v as f32).into(),
                resolution.into(),
                json!(hist).into(),
                score.into(),
                state.as_str().into(),
                batch.batch_id.clone().into(),
                publish_failures.into(),
                subscribe_failures.into(),
                region.into(),
                node.into(),
            ],
        ))
        .await?;
        if publish_failures > 0 {
            bump(
                &txn,
                conference_id,
                ConferenceCounter::PublishFailures,
                publish_failures as i64,
            )
            .await?;
        }
        if subscribe_failures > 0 {
            bump(
                &txn,
                conference_id,
                ConferenceCounter::SubscribeFailures,
                subscribe_failures as i64,
            )
            .await?;
        }
        if client_join_failures > 0 {
            bump(
                &txn,
                conference_id,
                ConferenceCounter::JoinClientFailures,
                client_join_failures,
            )
            .await?;
        }
        txn.commit().await?;
        Ok(Some(state.as_str().to_string()))
    }

    /// Scoped conference quality for hosts holding `quality.view` (no infrastructure
    /// identifiers).
    pub async fn conference_quality(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<ConferenceQualityView> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        c.require(Permission::QualityView, "You cannot view meeting quality")?;
        let s = super::operations::OperationsService::new(self.core.clone())
            .evaluate(conference_id)
            .await?;
        Ok(ConferenceQualityView {
            conference_id,
            health_state: s.health_state.clone(),
            active_sessions: s.active_participants,
            healthy_sessions: s.healthy_sessions,
            degraded_sessions: s.degraded_sessions,
            unhealthy_sessions: s.unhealthy_sessions,
            avg_rtt_ms: s.avg_rtt_ms,
            avg_jitter_ms: s.avg_jitter_ms,
            avg_packet_loss_ratio: s.avg_packet_loss_ratio,
            reconnects: s.reconnects,
        })
    }
}

/// Join-SLI classification of a join error (denials are never infrastructure
/// failures; client errors are excluded from the SLI).
pub fn classify_join_error(code: Option<ErrorCode>) -> super::sink::JoinOutcome {
    use super::sink::JoinOutcome;
    match code {
        Some(
            ErrorCode::RtcProviderUnavailable
            | ErrorCode::RevocationCheckUnavailable
            | ErrorCode::ArtifactStorageUnavailable,
        ) => JoinOutcome::ProviderFailure,
        Some(
            ErrorCode::MeetingAccessDenied
            | ErrorCode::MeetingPasscodeRequired
            | ErrorCode::MeetingPasscodeInvalid
            | ErrorCode::InvitationInvalid
            | ErrorCode::InvitationExpired
            | ErrorCode::ConferenceLocked
            | ErrorCode::ParticipantBanned
            | ErrorCode::PermissionDenied
            | ErrorCode::AuthenticationRequired
            | ErrorCode::ConsentRequired
            | ErrorCode::ProfilePolicyDenied
            | ErrorCode::MeetingCapacityReached,
        ) => JoinOutcome::Denied,
        _ => JoinOutcome::ClientFailure,
    }
}

/// Record a join outcome on the conference counters + sink (never fails a join).
pub async fn record_join<C: ConnectionTrait>(
    conn: &C,
    core: &Core,
    conference_id: Uuid,
    outcome: super::sink::JoinOutcome,
    latency_ms: Option<f64>,
) {
    use super::sink::JoinOutcome;
    let _ = bump(conn, conference_id, ConferenceCounter::JoinAttempts, 1).await;
    let counter = match outcome {
        JoinOutcome::Success => Some(ConferenceCounter::JoinSuccesses),
        JoinOutcome::Denied => Some(ConferenceCounter::JoinDenials),
        JoinOutcome::ProviderFailure => Some(ConferenceCounter::JoinProviderFailures),
        JoinOutcome::Timeout => Some(ConferenceCounter::JoinTimeouts),
        JoinOutcome::ClientFailure => Some(ConferenceCounter::JoinClientFailures),
        JoinOutcome::Waiting => None,
    };
    if let Some(c) = counter {
        let _ = bump(conn, conference_id, c, 1).await;
    }
    let _ = core.telemetry.record_join(outcome, latency_ms);
}

/// Provider-confirmed media connection (PENDING → CONNECTED, once per session):
/// join latency to usable media and reconnect accounting. Latency ends at the receipt
/// of the verified provider callback: provider event timestamps carry whole seconds
/// only, which would under-report sub-second joins. Webhook replays are
/// deduplicated upstream and the transition happens once, so nothing double counts.
pub async fn record_media_connected<C: ConnectionTrait>(
    conn: &C,
    core: &Core,
    session_id: Uuid,
    conference_id: Uuid,
) -> ApiResult<()> {
    let row = conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE participant_sessions s
                SET join_latency_ms = GREATEST(0, (EXTRACT(EPOCH FROM (clock_timestamp()
                        - COALESCE(s.join_requested_at, s.established_at))) * 1000))::int,
                    join_outcome = COALESCE(s.join_outcome, 'CONNECTED'),
                    reconnect_count = CASE WHEN s.reconnect_of_session_id IS NULL THEN 0
                        ELSE COALESCE((SELECT p.reconnect_count + 1 FROM participant_sessions p
                                        WHERE p.id = s.reconnect_of_session_id), 1) END
              WHERE s.id = $1
              RETURNING s.join_latency_ms, s.reconnect_of_session_id IS NOT NULL",
            [session_id.into()],
        ))
        .await?;
    let Some(row) = row else { return Ok(()) };
    let latency: Option<i32> = row.try_get_by_index(0).ok().flatten();
    let reconnect: bool = row.try_get_by_index(1).unwrap_or(false);
    bump(
        conn,
        conference_id,
        ConferenceCounter::JoinMediaConnected,
        1,
    )
    .await?;
    if reconnect {
        bump(conn, conference_id, ConferenceCounter::Reconnects, 1).await?;
        let _ = core.telemetry.record_reconnect();
    }
    let _ = core
        .telemetry
        .record_media_connected(latency.map(|l| l as f64));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> QualitySampleInput {
        QualitySampleInput {
            timestamp: Utc::now(),
            rtt_ms: Some(100.0),
            jitter_ms: Some(10.0),
            packet_loss_ratio: Some(0.01),
            bitrate_kbps: Some(800.0),
            width: Some(1280),
            height: Some(720),
            fps: Some(30.0),
            publish_health: None,
            subscribe_health: None,
            network_type: None,
            failure: None,
        }
    }

    #[test]
    fn samples_are_bounded_and_finite() {
        let now = Utc::now();
        assert!(validate_sample(&sample(), now, 300).is_ok());
        for bad in [
            QualitySampleInput {
                rtt_ms: Some(f64::NAN),
                ..sample()
            },
            QualitySampleInput {
                rtt_ms: Some(f64::INFINITY),
                ..sample()
            },
            QualitySampleInput {
                jitter_ms: Some(-1.0),
                ..sample()
            },
            QualitySampleInput {
                packet_loss_ratio: Some(1.5),
                ..sample()
            },
            QualitySampleInput {
                fps: Some(1000.0),
                ..sample()
            },
            QualitySampleInput {
                width: Some(100_000),
                ..sample()
            },
            QualitySampleInput {
                timestamp: now - chrono::Duration::hours(2),
                ..sample()
            },
        ] {
            assert!(validate_sample(&bad, now, 300).is_err());
        }
        assert!(sane_label(&Some("eu-west_1".into())).is_ok());
        assert!(sane_label(&Some("bob@example.com".into())).is_err());
    }

    #[test]
    fn p95_from_histogram() {
        let mut h = vec![0i64; RTT_BUCKETS.len()];
        h[0] = 95;
        h[5] = 5;
        assert_eq!(p95(&h), Some(50.0));
        h[5] = 10;
        assert_eq!(p95(&h), Some(500.0));
        assert_eq!(p95(&[0; 9]), None);
    }

    #[test]
    fn join_errors_are_classified_without_counting_denials_as_outages() {
        use super::super::sink::JoinOutcome;
        assert_eq!(
            classify_join_error(Some(ErrorCode::PermissionDenied)),
            JoinOutcome::Denied
        );
        assert_eq!(
            classify_join_error(Some(ErrorCode::RtcProviderUnavailable)),
            JoinOutcome::ProviderFailure
        );
        assert_eq!(
            classify_join_error(Some(ErrorCode::ValidationError)),
            JoinOutcome::ClientFailure
        );
    }
}
