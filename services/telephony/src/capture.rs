//! Edge media capture (Part F §60, §105–§108; ADR 0023).
//!
//! ```text
//! Calling CAPTURE(captureId, purpose, max, beep) on an internal enterprise SIP leg
//!   ─▶ telephony_captures RECORDING, the edge records into its TRANSIENT spool under
//!      `cfm-cap-<captureId>` (Telephony's own name, never Calling's): a VOICEMAIL the
//!      caller's channel; a call RECORDING the leg's bridge when it is bridged (the mixed
//!      call audio), else its channel
//! Calling CAPTURE_STOP ─▶ the edge stops the capture early (never blocked)
//! edge RecordingFinished ─▶ FINISHED (also after a caller hang-up: the partial audio)
//! edge RecordingFailed   ─▶ FAILED (EDGE_RECORDING_FAILED)
//! worker: FINISHED ─▶ fetch the spool file, size / duration / SHA-256 ─▶ POST Calling
//!         /internal/v1/telephony/captures ─▶ 200: DELIVERED, spool deleted
//!                                         ─▶ 503 / unreachable: retry with backoff
//!                                         ─▶ 4xx / attempts exhausted: DEAD (incident),
//!                                            spool deleted anyway
//!         FAILED ─▶ the bounded failure is delivered the same way
//!         RECORDING past its bound (end event lost) ─▶ FINISHED
//! ```
//!
//! Application nodes are never long-term storage: the spool copy is deleted as soon as
//! Calling acknowledged it, or when the capture is dead-lettered. Audio never passes
//! through logs, metrics, incidents or this service's database; only bounded codes do.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

use crate::{
    commands::{executor::Executed, CaptureSpec, CommandRow},
    db,
    error::TelephonyResult,
    incidents::{self, Incident, Severity},
    legs::LegRow,
    ports::PortError,
    service::TelephonyService,
    sip::EdgeError,
};

/// The longest capture Calling may command (a call recording; one WAV delivery).
pub const MAX_CAPTURE_SECONDS: i32 = 1440;
/// The longest voicemail message.
pub const MAX_VOICEMAIL_SECONDS: i32 = 600;
/// The largest spool file delivered (600 s of 16-bit 16 kHz mono is ~19 MiB).
pub const MAX_CAPTURE_BYTES: usize = 24 * 1024 * 1024;
/// Delivery attempts before a capture is dead-lettered.
pub const MAX_DELIVERY_ATTEMPTS: i32 = 20;
/// How long after its bound a RECORDING capture whose end event was lost is finished.
const RECORDING_GRACE_SECONDS: i64 = 120;
/// A claimed delivery is not picked up again for this long (a crashed worker's claim
/// expires and the capture is retried).
const DELIVERY_LEASE_SECONDS: i64 = 120;

/// The edge spool name of a capture (Telephony's own; valid as an edge reference).
pub fn spool_name(capture_id: Uuid) -> String {
    format!("cfm-cap-{}", capture_id.simple())
}

/// The duration of a PCM WAV file from its header (None: not a readable WAV).
pub fn wav_duration_ms(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let u32_at = |i: usize| -> Option<u32> {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let mut at = 12;
    let mut byte_rate = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(at + 4)? as usize;
        let body = at + 8;
        match id {
            b"fmt " => byte_rate = u32_at(body + 8).filter(|r| *r > 0),
            b"data" => {
                let available = bytes.len() - body;
                // A recording cut short (or still streaming) may carry a 0 / max size.
                let len = if size == 0 || size > available {
                    available
                } else {
                    size
                };
                return Some(len as u64 * 1000 / byte_rate? as u64);
            }
            _ => {}
        }
        at = body.checked_add(size)?.checked_add(size % 2)?;
    }
    None
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds(2_i64.saturating_pow(attempts.clamp(1, 10) as u32).min(600))
}

#[derive(Debug, FromQueryResult)]
struct CaptureRow {
    id: Uuid,
    telephony_leg_id: Uuid,
    calling_call_id: Uuid,
    calling_call_leg_id: Uuid,
    spool_name: String,
    status: String,
    failure_code: Option<String>,
    delivery_attempts: i32,
}

const CAPTURE_COLUMNS: &str =
    "id, telephony_leg_id, calling_call_id, calling_call_leg_id, spool_name, \
    status, failure_code, delivery_attempts";

/// What one capture pass did (tests and metrics).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CaptureReport {
    pub delivered: usize,
    pub retried: usize,
    pub dead: usize,
    pub spool_deleted: usize,
}

impl TelephonyService {
    /// Executes CAPTURE on an enterprise leg's channel (called by the enterprise control
    /// executor). Idempotent per capture id.
    pub(crate) async fn start_capture(
        &self,
        command: CommandRow,
        leg: LegRow,
        channel: String,
    ) -> TelephonyResult<Executed> {
        let now = self.now();
        let Ok(spec) = serde_json::from_value::<CaptureSpec>(command.payload["capture"].clone())
        else {
            self.finish_command_pub(command.id, "FAILED", "INVALID_CAPTURE", now)
                .await?;
            return Ok(Executed::DefiniteFailure("INVALID_CAPTURE"));
        };
        let name = spool_name(spec.capture_id);
        db::exec(
            &self.db,
            "INSERT INTO telephony_captures (id, telephony_leg_id, command_id, tenant_id, application_id, \
               environment_id, calling_call_id, calling_call_leg_id, purpose, spool_name, \
               max_duration_seconds, status, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'RECORDING', $12, $12) \
             ON CONFLICT (id) DO NOTHING",
            vec![
                spec.capture_id.into(),
                leg.id.into(),
                command.id.into(),
                leg.tenant_id.into(),
                leg.application_id.into(),
                leg.environment_id.into(),
                leg.calling_call_id.into(),
                leg.calling_call_leg_id.into(),
                spec.purpose.clone().into(),
                name.clone().into(),
                spec.max_duration_seconds.into(),
                now.into(),
            ],
        )
        .await?;
        let row: Option<CaptureRow> = db::one(
            &self.db,
            &format!("SELECT {CAPTURE_COLUMNS} FROM telephony_captures WHERE id = $1"),
            vec![spec.capture_id.into()],
        )
        .await?;
        let Some(row) = row.filter(|r| r.telephony_leg_id == leg.id) else {
            // A capture id is bound to ONE leg.
            self.finish_command_pub(command.id, "FAILED", "CAPTURE_CONFLICT", now)
                .await?;
            return Ok(Executed::DefiniteFailure("CAPTURE_CONFLICT"));
        };
        if row.status != "RECORDING" {
            self.finish_command_pub(command.id, "SUCCEEDED", "ALREADY_CAPTURED", now)
                .await?;
            return Ok(Executed::Done);
        }
        // A call recording records the mixed bridge audio when the leg is bridged.
        let bridge = if spec.purpose == "RECORDING" {
            self.live_bridge_of(leg.id).await?
        } else {
            None
        };
        let outcome = match &bridge {
            Some(bridge) => {
                self.edge
                    .record_bridge(bridge, &name, spec.max_duration_seconds as u32, spec.beep)
                    .await
            }
            None => {
                self.edge
                    .record(&channel, &name, spec.max_duration_seconds as u32, spec.beep)
                    .await
            }
        };
        let result = match outcome {
            Ok(()) => "STARTED",
            // A previous attempt timed out after the edge may have started it.
            Err(EdgeError::Refused) if command.attempts > 1 => "ASSUMED_STARTED",
            Err(EdgeError::Refused) => {
                self.capture_failed(row.id, "EDGE_REFUSED").await?;
                "EDGE_REFUSED"
            }
            Err(EdgeError::NotFound) => {
                self.capture_failed(row.id, "CALLER_GONE").await?;
                "CHANNEL_GONE"
            }
            Err(EdgeError::Unavailable | EdgeError::Timeout) => {
                return self.reschedule_pub(&command, "EDGE_UNAVAILABLE").await;
            }
        };
        crate::metrics::inc(
            "telephony_enterprise_commands_total",
            &[("kind", "CAPTURE"), ("result", result)],
        );
        if result == "EDGE_REFUSED" {
            self.finish_command_pub(command.id, "FAILED", result, now)
                .await?;
            return Ok(Executed::DefiniteFailure("EDGE_REFUSED"));
        }
        self.finish_command_pub(command.id, "SUCCEEDED", result, now)
            .await?;
        Ok(Executed::Done)
    }

    /// The edge bridge a leg is a live member of (none: not bridged).
    async fn live_bridge_of(&self, leg_id: Uuid) -> TelephonyResult<Option<String>> {
        #[derive(FromQueryResult)]
        struct Bridge {
            edge_bridge_ref: String,
        }
        let row: Option<Bridge> = db::one(
            &self.db,
            "SELECT b.edge_bridge_ref FROM telephony_bridge_legs l \
               JOIN telephony_bridges b ON b.id = l.bridge_id \
              WHERE l.telephony_leg_id = $1 AND l.status = 'JOINED' \
                AND b.status IN ('PENDING', 'ACTIVE') AND b.edge_bridge_ref IS NOT NULL \
              ORDER BY b.created_at DESC LIMIT 1",
            vec![leg_id.into()],
        )
        .await?;
        Ok(row.map(|b| b.edge_bridge_ref))
    }

    /// Executes CAPTURE_STOP: the edge stops the running capture (its end event then
    /// delivers it). A capture that already finished makes this a no-op.
    pub(crate) async fn stop_capture(&self, command: CommandRow) -> TelephonyResult<Executed> {
        let now = self.now();
        let Ok(spec) = serde_json::from_value::<CaptureSpec>(command.payload["capture"].clone())
        else {
            self.finish_command_pub(command.id, "FAILED", "INVALID_CAPTURE", now)
                .await?;
            return Ok(Executed::DefiniteFailure("INVALID_CAPTURE"));
        };
        match self.edge.stop_recording(&spool_name(spec.capture_id)).await {
            Ok(()) | Err(EdgeError::NotFound) => {
                self.finish_command_pub(command.id, "SUCCEEDED", "DONE", now)
                    .await?;
                crate::metrics::inc(
                    "telephony_enterprise_commands_total",
                    &[("kind", "CAPTURE_STOP"), ("result", "DONE")],
                );
                Ok(Executed::Done)
            }
            Err(EdgeError::Refused) => {
                self.finish_command_pub(command.id, "FAILED", "EDGE_REFUSED", now)
                    .await?;
                Ok(Executed::DefiniteFailure("EDGE_REFUSED"))
            }
            Err(EdgeError::Unavailable | EdgeError::Timeout) => {
                self.reschedule_pub(&command, "EDGE_UNAVAILABLE").await
            }
        }
    }

    async fn capture_failed(&self, id: Uuid, code: &str) -> TelephonyResult<()> {
        let now = self.now();
        db::exec(
            &self.db,
            "UPDATE telephony_captures SET status = 'FAILED', failure_code = $2, finished_at = $3, \
               next_attempt_at = $3, updated_at = GREATEST(updated_at, $3) \
             WHERE id = $1 AND status = 'RECORDING'",
            vec![id.into(), code.into(), now.into()],
        )
        .await?;
        Ok(())
    }

    /// The edge reports a spool recording's end (by its name; the leg may already be
    /// over — a caller hanging up mid-message still leaves a message).
    pub(crate) async fn capture_edge_event(
        &self,
        name: &str,
        finished: bool,
    ) -> TelephonyResult<()> {
        let now = self.now();
        let changed = db::exec(
            &self.db,
            "UPDATE telephony_captures SET status = $2, \
               failure_code = CASE WHEN $2 = 'FAILED' THEN 'EDGE_RECORDING_FAILED' END, \
               finished_at = $3, next_attempt_at = $3, updated_at = GREATEST(updated_at, $3) \
             WHERE spool_name = $1 AND status = 'RECORDING'",
            vec![
                name.to_string().into(),
                if finished { "FINISHED" } else { "FAILED" }.into(),
                now.into(),
            ],
        )
        .await?;
        if changed > 0 {
            crate::metrics::inc(
                "telephony_captures_total",
                &[("result", if finished { "FINISHED" } else { "FAILED" })],
            );
        }
        Ok(())
    }

    /// One capture pass (never blocked by kill switches: delivery and spool cleanup always
    /// run): lost end events, due deliveries, spool cleanup.
    pub async fn deliver_captures_once(&self, limit: u64) -> TelephonyResult<CaptureReport> {
        let now = self.now();
        let mut report = CaptureReport::default();
        // RECORDING past its bound: the end event was lost — try to deliver what exists.
        db::exec(
            &self.db,
            "UPDATE telephony_captures SET status = 'FINISHED', finished_at = $1, next_attempt_at = $1, \
               updated_at = GREATEST(updated_at, $1) \
             WHERE status = 'RECORDING' \
               AND created_at + make_interval(secs => max_duration_seconds + $2) <= $1",
            vec![now.into(), (RECORDING_GRACE_SECONDS as f64).into()],
        )
        .await?;
        // Claim due deliveries (SKIP LOCKED + a lease: every instance may run this).
        let txn = self.db.begin().await?;
        let due: Vec<CaptureRow> = db::all(
            &txn,
            &format!(
                "SELECT {CAPTURE_COLUMNS} FROM telephony_captures \
                  WHERE status IN ('FINISHED', 'FAILED') AND next_attempt_at <= $1 \
                  ORDER BY next_attempt_at LIMIT $2 FOR UPDATE SKIP LOCKED"
            ),
            vec![now.into(), (limit.clamp(1, 100) as i64).into()],
        )
        .await?;
        for row in &due {
            db::exec(
                &txn,
                "UPDATE telephony_captures SET next_attempt_at = $2 WHERE id = $1",
                vec![
                    row.id.into(),
                    (now + Duration::seconds(DELIVERY_LEASE_SECONDS)).into(),
                ],
            )
            .await?;
        }
        txn.commit().await?;
        for row in due {
            match self.deliver_capture(row).await? {
                Delivery::Delivered => report.delivered += 1,
                Delivery::Retry => report.retried += 1,
                Delivery::Dead => report.dead += 1,
            }
        }
        // Spool cleanup (also retries a deletion that failed right after delivery).
        #[derive(FromQueryResult)]
        struct Spool {
            id: Uuid,
            spool_name: String,
        }
        let spools: Vec<Spool> = db::all(
            &self.db,
            "SELECT id, spool_name FROM telephony_captures \
              WHERE status IN ('DELIVERED', 'DEAD') AND spool_deleted_at IS NULL \
              ORDER BY updated_at LIMIT $1",
            vec![(limit.clamp(1, 100) as i64).into()],
        )
        .await?;
        for spool in spools {
            if self.delete_spool(spool.id, &spool.spool_name).await? {
                report.spool_deleted += 1;
            }
        }
        Ok(report)
    }

    async fn delete_spool(&self, id: Uuid, name: &str) -> TelephonyResult<bool> {
        match self.edge.delete_recording(name).await {
            Ok(()) => {
                let now = self.now();
                db::exec(
                    &self.db,
                    "UPDATE telephony_captures SET spool_deleted_at = $2, \
                       updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND spool_deleted_at IS NULL",
                    vec![id.into(), now.into()],
                )
                .await?;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }

    async fn deliver_capture(&self, row: CaptureRow) -> TelephonyResult<Delivery> {
        use base64::Engine;
        let mut report = json!({
            "schemaVersion": "1",
            "captureId": row.id,
            "telephonyLegId": row.telephony_leg_id,
            "callId": row.calling_call_id,
            "callLegId": row.calling_call_leg_id,
        });
        let mut measured: Option<(i64, i64)> = None;
        let failure = if row.status == "FINISHED" {
            match self
                .edge
                .fetch_recording(&row.spool_name, MAX_CAPTURE_BYTES)
                .await
            {
                Ok(bytes) => match wav_duration_ms(&bytes) {
                    Some(ms) if bytes.len() > 44 => {
                        use sha2::{Digest, Sha256};
                        let sha = hex::encode(Sha256::digest(&bytes));
                        measured = Some((ms as i64, bytes.len() as i64));
                        report["outcome"] = json!("COMPLETED");
                        report["media"] = json!({
                            "contentType": "audio/wav",
                            "durationMs": ms.min(3_600_000),
                            "sizeBytes": bytes.len(),
                            "sha256": sha,
                            "base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                        });
                        None
                    }
                    // A header-only file: nothing was said.
                    Some(_) => Some("EMPTY_RECORDING"),
                    None => Some("EDGE_RECORDING_UNREADABLE"),
                },
                Err(EdgeError::NotFound) => Some("EDGE_RECORDING_MISSING"),
                Err(EdgeError::Refused) => Some("CAPTURE_TOO_LARGE"),
                Err(EdgeError::Unavailable | EdgeError::Timeout) => {
                    return self.retry_delivery(&row, "EDGE_UNAVAILABLE").await;
                }
            }
        } else {
            Some(row.failure_code.as_deref().unwrap_or("CAPTURE_FAILED"))
        };
        if let Some(code) = failure {
            report["outcome"] = json!("FAILED");
            report["failureCode"] = json!(code);
        }
        match self.calling.deliver_capture(&report).await {
            Ok(_) => {
                let now = self.now();
                db::exec(
                    &self.db,
                    "UPDATE telephony_captures SET status = 'DELIVERED', delivered_at = $2, \
                       delivery_attempts = delivery_attempts + 1, duration_ms = $3, size_bytes = $4, \
                       failure_code = COALESCE(failure_code, $5), last_error = NULL, next_attempt_at = NULL, \
                       updated_at = GREATEST(updated_at, $2) WHERE id = $1",
                    vec![
                        row.id.into(),
                        now.into(),
                        measured.map(|m| m.0).into(),
                        measured.map(|m| m.1).into(),
                        failure.map(str::to_string).into(),
                    ],
                )
                .await?;
                self.delete_spool(row.id, &row.spool_name).await?;
                crate::metrics::inc(
                    "telephony_capture_deliveries_total",
                    &[(
                        "result",
                        if failure.is_none() {
                            "DELIVERED"
                        } else {
                            "FAILURE_DELIVERED"
                        },
                    )],
                );
                tracing::info!(capture_id = %row.id, "edge capture delivered to Calling");
                Ok(Delivery::Delivered)
            }
            Err(PortError::Unavailable) => self.retry_delivery(&row, "CALLING_UNAVAILABLE").await,
            Err(PortError::NotFound) => self.dead_letter(&row, "CALLING_UNKNOWN_CAPTURE").await,
            Err(PortError::Denied(_)) => self.dead_letter(&row, "CALLING_REFUSED").await,
        }
    }

    async fn retry_delivery(
        &self,
        row: &CaptureRow,
        code: &'static str,
    ) -> TelephonyResult<Delivery> {
        let attempts = row.delivery_attempts + 1;
        if attempts >= MAX_DELIVERY_ATTEMPTS {
            return self.dead_letter(row, "DELIVERY_ATTEMPTS_EXHAUSTED").await;
        }
        let now = self.now();
        db::exec(
            &self.db,
            "UPDATE telephony_captures SET delivery_attempts = $2, last_error = $3, next_attempt_at = $4, \
               updated_at = GREATEST(updated_at, $5) WHERE id = $1",
            vec![
                row.id.into(),
                attempts.into(),
                code.into(),
                (now + backoff(attempts)).into(),
                now.into(),
            ],
        )
        .await?;
        crate::metrics::inc("telephony_capture_deliveries_total", &[("result", code)]);
        Ok(Delivery::Retry)
    }

    async fn dead_letter(&self, row: &CaptureRow, code: &'static str) -> TelephonyResult<Delivery> {
        let now: DateTime<Utc> = self.now();
        let txn = self.db.begin().await?;
        db::exec(
            &txn,
            "UPDATE telephony_captures SET status = 'DEAD', delivery_attempts = delivery_attempts + 1, \
               last_error = $2, next_attempt_at = NULL, updated_at = GREATEST(updated_at, $3) \
             WHERE id = $1",
            vec![row.id.into(), code.into(), now.into()],
        )
        .await?;
        incidents::raise(
            &txn,
            &Incident::new(
                "CAPTURE_DELIVERY_DEAD",
                Severity::Medium,
                format!("capture:{}", row.id),
            )
            .evidence(json!({ "reason": code, "attempts": row.delivery_attempts + 1 })),
            now,
        )
        .await?;
        txn.commit().await?;
        // Application nodes never keep audio: the spool copy goes even undelivered.
        self.delete_spool(row.id, &row.spool_name).await?;
        crate::metrics::inc("telephony_capture_deliveries_total", &[("result", "DEAD")]);
        tracing::warn!(capture_id = %row.id, reason = code, "edge capture dead-lettered");
        Ok(Delivery::Dead)
    }
}

enum Delivery {
    Delivered,
    Retry,
    Dead,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(data_len: u32, byte_rate: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&(byte_rate / 2).to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend(std::iter::repeat_n(0u8, data_len as usize));
        out
    }

    #[test]
    fn wav_duration_comes_from_the_header() {
        assert_eq!(wav_duration_ms(&wav(16_000, 16_000)), Some(1000));
        assert_eq!(wav_duration_ms(&wav(8_000, 16_000)), Some(500));
        // A truncated / still-open recording: the bytes present count.
        let mut open = wav(16_000, 16_000);
        open[40..44].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(wav_duration_ms(&open), Some(1000));
        assert_eq!(wav_duration_ms(b"not a wav file at all"), None);
        assert_eq!(wav_duration_ms(&[]), None);
        // An extra chunk before the data is skipped.
        let mut list = wav(16_000, 16_000);
        let data = list.split_off(36);
        list.extend_from_slice(b"LIST");
        list.extend_from_slice(&4u32.to_le_bytes());
        list.extend_from_slice(b"INFO");
        list.extend_from_slice(&data);
        assert_eq!(wav_duration_ms(&list), Some(1000));
    }

    #[test]
    fn spool_names_are_safe_edge_references() {
        let id = Uuid::new_v4();
        let name = spool_name(id);
        assert!(name.starts_with("cfm-cap-"));
        assert_eq!(name.len(), 40);
        assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    }

    #[test]
    fn delivery_backoff_is_bounded() {
        assert_eq!(backoff(1), Duration::seconds(2));
        assert_eq!(backoff(5), Duration::seconds(32));
        assert_eq!(backoff(30), Duration::seconds(600));
    }
}
