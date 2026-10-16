//! Collection of application (RTC) captures (§60 voicemail, §61 call recording) through
//! the neutral [`CallMediaCapture`](super::CallMediaCapture) interface.
//!
//! ```text
//! voicemail RECORDING / recording RECORDING|STOPPING|PROCESSING (capture_source APPLICATION)
//!   ─▶ adapter poll ─▶ Running + stop due (caller gone / max length / STOPPING)
//!                       ─▶ adapter stop (idempotent; retried while still Running) ─▶ poll
//!                  ─▶ Finished(media) ─▶ deliver_capture (the SAME protected-storage path
//!                     as SIP captures: validation, digest, private object, READY)
//!                     ─▶ adapter release (its intermediate media is removed)
//!                  ─▶ Failed(code)    ─▶ FAILED (the Call is not affected) ─▶ release
//!                  ─▶ Pending         ─▶ next pass (a capture never delivered is FAILED
//!                                        CAPTURE_LOST by the voicemail / recording passes)
//! FAILED within the delivery window ─▶ stop + release only (an abandoned capture never
//!                                       becomes READY and leaves no intermediate media)
//! ```
//!
//! Never blocked by a kill switch: collection, stop and cleanup always run.

use chrono::Duration;
use sea_orm::ConnectionTrait;
use uuid::Uuid;

use super::{stmt, CaptureStatus};
use crate::calls::{
    app::AppCallService,
    errors::{CallError, CallResult},
};

/// Grace of a voicemail after its maximum length (as `RECORDING_GRACE_SECONDS`).
const VOICEMAIL_GRACE_SECONDS: i64 = 30;
/// How long an abandoned (FAILED) capture is still stopped / released.
const ABANDONED_WINDOW_MINUTES: i64 = 30;

const TERMINAL: &str = "('ENDING', 'ENDED', 'FAILED', 'CANCELLED', 'DECLINED', 'NO_ANSWER', \
                        'BUSY', 'DENIED')";

struct Due {
    id: Uuid,
    call_id: Uuid,
    abandoned: bool,
    stop_due: bool,
}

impl AppCallService {
    /// Worker pass: collects finished application captures. Returns (stored, failed).
    pub(crate) async fn capture_tick(&self, limit: u64) -> CallResult<(usize, usize)> {
        if !self.capture.available() {
            return Ok((0, 0));
        }
        let now = self.calls.now();
        let abandoned_after = now - Duration::minutes(ABANDONED_WINDOW_MINUTES);
        let mut due = Vec::new();
        for sql in [
            // A voicemail stops when the caller is gone or its maximum length is reached
            // (`recording_deadline` = received + maximum + grace).
            format!(
                "SELECT m.id, m.call_id, m.state = 'FAILED', \
                        (c.current_state IN {TERMINAL} OR m.recording_deadline <= $1) \
                   FROM voicemail_messages m JOIN calls c ON c.id = m.call_id \
                  WHERE m.capture_source = 'APPLICATION' \
                    AND (m.state IN ('RECORDING', 'PROCESSING') \
                         OR (m.state = 'FAILED' AND m.failed_at > $2)) \
                  ORDER BY m.received_at LIMIT $3"
            ),
            // A recording is stopped by the recording pass / an explicit stop (STOPPING);
            // a STARTING one is still being started by its requester.
            "SELECT r.id, r.call_id, r.state = 'FAILED', r.state = 'STOPPING' \
               FROM call_recordings r \
              WHERE r.capture_source = 'APPLICATION' \
                AND (r.state IN ('RECORDING', 'STOPPING', 'PROCESSING') \
                     OR (r.state = 'FAILED' AND r.failed_at > $2)) \
              ORDER BY r.started_at LIMIT $3"
                .to_string(),
        ] {
            let rows = self
                .db()
                .query_all(stmt(
                    &sql,
                    vec![
                        (now + Duration::seconds(VOICEMAIL_GRACE_SECONDS)).into(),
                        abandoned_after.into(),
                        (limit as i64).into(),
                    ],
                ))
                .await?;
            for r in rows {
                due.push(Due {
                    id: r.try_get_by_index(0)?,
                    call_id: r.try_get_by_index(1)?,
                    abandoned: r.try_get_by_index(2)?,
                    stop_due: r.try_get_by_index(3)?,
                });
            }
        }
        let mut out = (0, 0);
        for capture in due {
            let mut status = self.capture.poll(capture.call_id, capture.id).await;
            // A stop is (re-)issued only while the adapter reports the capture running:
            // an earlier stop that was lost is retried, a successful one never repeated.
            if matches!(status, CaptureStatus::Running) && (capture.abandoned || capture.stop_due) {
                match self.capture.stop(capture.call_id, capture.id).await {
                    Ok(()) => status = self.capture.poll(capture.call_id, capture.id).await,
                    // Retried on the next pass (an unreachable media server never blocks).
                    Err(code) => {
                        tracing::warn!(capture_id = %capture.id, code, "capture stop pending; retried")
                    }
                }
            }
            match status {
                CaptureStatus::Running | CaptureStatus::Pending => {}
                CaptureStatus::Finished(_) | CaptureStatus::Failed(_) if capture.abandoned => {
                    self.release_capture(&capture).await;
                }
                CaptureStatus::Finished(media) => {
                    match self
                        .deliver_capture(capture.id, capture.call_id, media)
                        .await
                    {
                        Ok(_) | Err(CallError::Conflict(_)) => {
                            self.release_capture(&capture).await;
                            out.0 += 1;
                            collected("STORED");
                        }
                        // Protected storage is unavailable: the media stays with the
                        // adapter and the next pass retries.
                        Err(CallError::ArtifactStorageUnavailable) => {
                            collected("STORAGE_RETRY");
                        }
                        Err(CallError::Validation(_)) => {
                            self.capture_failed(capture.id, "CAPTURE_INVALID").await?;
                            self.release_capture(&capture).await;
                            out.1 += 1;
                            collected("INVALID");
                        }
                        Err(e) => return Err(e),
                    }
                }
                CaptureStatus::Failed(code) => {
                    self.capture_failed(capture.id, code).await?;
                    self.release_capture(&capture).await;
                    out.1 += 1;
                    collected("FAILED");
                }
            }
        }
        Ok(out)
    }

    async fn release_capture(&self, capture: &Due) {
        if let Err(code) = self.capture.release(capture.call_id, capture.id).await {
            tracing::warn!(capture_id = %capture.id, code, "capture media release pending; retried");
        }
    }
}

fn collected(result: &'static str) {
    crate::metrics::inc(
        "calling_enterprise_capture_collections_total",
        &[("result", result)],
    );
}
