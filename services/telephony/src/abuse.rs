//! Abuse signals (Part E §162): hooks and metrics, not fraud analytics (Part H).
//!
//! A rate-limited scan over Telephony's own legs raises an `ABUSE_SIGNAL` incident (and
//! counts `telephony_abuse_signals_total{kind}`) when, within its window:
//!
//! * one protected contact keeps not answering (`REPEATED_NO_ANSWER`);
//! * one protected contact is dialed again and again (`SAME_DESTINATION_VELOCITY`);
//! * one application's originations keep failing (`FAILED_ORIGINATIONS`);
//! * one Privacy Relay session keeps requesting callbacks (`RAPID_CALLBACKS`).
//!
//! Evidence holds opaque identifiers and counts only (never a number). Nothing is
//! blocked here: enforcement stays with the spend / velocity policy in Calling and the
//! kill switches; these signals are for operators and later analytics.

use chrono::{DateTime, Duration, Timelike, Utc};
use sea_orm::FromQueryResult;
use serde_json::json;
use uuid::Uuid;

use crate::{
    db,
    error::TelephonyResult,
    incidents::{self, Incident, Severity},
    service::TelephonyService,
};

/// How often an instance runs the scan.
pub const ABUSE_SCAN_EVERY_SECONDS: i64 = 60;
pub const REPEATED_NO_ANSWER: (i64, i64) = (3, 60);
pub const SAME_DESTINATION_VELOCITY: (i64, i64) = (5, 10);
pub const FAILED_ORIGINATIONS: (i64, i64) = (10, 10);
pub const RAPID_CALLBACKS: (i64, i64) = (5, 10);

#[derive(Debug, FromQueryResult)]
struct Hit {
    subject: Uuid,
    n: i64,
}

impl TelephonyService {
    /// One abuse scan (at most every `ABUSE_SCAN_EVERY_SECONDS` per instance). Returns
    /// the number of signals raised.
    pub async fn abuse_scan_once(&self) -> TelephonyResult<usize> {
        let now = self.now();
        {
            let mut last = self
                .last_abuse_scan
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if last.is_some_and(|at| now - at < Duration::seconds(ABUSE_SCAN_EVERY_SECONDS)) {
                return Ok(0);
            }
            *last = Some(now);
        }
        let mut raised = 0;
        for (kind, (threshold, minutes), sql) in [
            (
                "REPEATED_NO_ANSWER",
                REPEATED_NO_ANSWER,
                "SELECT contact_reference AS subject, count(*) AS n FROM telephony_legs \
                 WHERE direction = 'OUTBOUND' AND end_reason = 'NO_ANSWER' AND ended_at > $1 \
                   AND contact_reference IS NOT NULL GROUP BY contact_reference HAVING count(*) >= $2",
            ),
            (
                "SAME_DESTINATION_VELOCITY",
                SAME_DESTINATION_VELOCITY,
                "SELECT contact_reference AS subject, count(*) AS n FROM telephony_legs \
                 WHERE direction = 'OUTBOUND' AND created_at > $1 AND contact_reference IS NOT NULL \
                 GROUP BY contact_reference HAVING count(*) >= $2",
            ),
            (
                "FAILED_ORIGINATIONS",
                FAILED_ORIGINATIONS,
                "SELECT application_id AS subject, count(*) AS n FROM telephony_legs \
                 WHERE direction = 'OUTBOUND' AND status = 'FAILED' AND ended_at > $1 \
                 GROUP BY application_id HAVING count(*) >= $2",
            ),
            (
                "RAPID_CALLBACKS",
                RAPID_CALLBACKS,
                "SELECT relay_session_id AS subject, count(DISTINCT calling_call_id) AS n FROM telephony_legs \
                 WHERE route_class = 'CALLBACK_RELAY' AND created_at > $1 AND relay_session_id IS NOT NULL \
                 GROUP BY relay_session_id HAVING count(DISTINCT calling_call_id) >= $2",
            ),
        ] {
            let hits: Vec<Hit> = db::all(
                &self.db,
                sql,
                vec![(now - Duration::minutes(minutes)).into(), threshold.into()],
            )
            .await?;
            for hit in hits {
                self.raise_abuse_signal(kind, hit.subject, hit.n, minutes, now)
                    .await?;
                raised += 1;
            }
        }
        Ok(raised)
    }

    async fn raise_abuse_signal(
        &self,
        kind: &'static str,
        subject: Uuid,
        count: i64,
        window_minutes: i64,
        now: DateTime<Utc>,
    ) -> TelephonyResult<()> {
        crate::metrics::inc("telephony_abuse_signals_total", &[("kind", kind)]);
        // One incident per kind / subject / hour (repeats count as occurrences).
        let hour = now
            .with_minute(0)
            .and_then(|t| t.with_second(0))
            .and_then(|t| t.with_nanosecond(0))
            .unwrap_or(now);
        incidents::raise(
            &self.db,
            &Incident::new(
                "ABUSE_SIGNAL",
                Severity::Medium,
                format!("abuse:{kind}:{subject}:{}", hour.timestamp()),
            )
            .evidence(json!({
                "kind": kind,
                "subject": subject,
                "count": count,
                "windowMinutes": window_minutes,
            })),
            now,
        )
        .await?;
        tracing::warn!(kind, count, window_minutes, "abuse signal raised");
        Ok(())
    }
}
