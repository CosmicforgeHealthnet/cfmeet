//! Gap C10: DCN voice usage and DCN risk signals for Numbering (ADR 0043 decisions 7, 8).
//!
//! Numbering owns number-level cost evidence and number-level risk; the carriers report
//! what they observed. For voice that carrier is Telephony:
//!
//! * **Usage (`number-usage.report`).** When a DCN leg's usage is finalized
//!   ([`crate::usage`]) and it was billable (at least one second), a `USAGE` report is
//!   written in the SAME transaction: `VOICE_INBOUND_SECONDS` / `VOICE_OUTBOUND_SECONDS`
//!   of the leg's Number Resource and assignment, the billable seconds, the provider cost
//!   ONLY when the provider reported it (an estimate is never sent as cost evidence), and
//!   the connected period. Source key `voice-leg:<leg>` (Numbering prefixes the reporter):
//!   retries, restarts and two workers never duplicate.
//! * **Risk (`number-risk.report`).** The abuse scan ([`crate::abuse`]) also looks at DCN
//!   legs: one DCN originating too many calls in a window (`SPAM_CALLS`) or too many
//!   unanswered / sub-six-second calls (`NUISANCE_CALLS`). The signal's magnitude grows
//!   with the multiple of the threshold (50 at the threshold, 100 at twice it); Numbering
//!   scores it and decides (a restricting score suspends the number with source RISK).
//!   Restrictive only — Telephony never blocks a call on it.
//!
//! **Never blocking.** Reports are a durable outbox (`telephony_numbering_reports`)
//! delivered by the workers with capped backoff. A Numbering outage, a missing trust
//! grant (`401` / `403`) or rate limiting only delays them; a permanent refusal (unknown
//! number / assignment, a conflicting source key, a kind this service may not report) or
//! exhausting [`MAX_REPORT_ATTEMPTS`] dead-letters the report with one deduplicated
//! `OUTBOX_DEAD_LETTER` incident per reason. Calls never wait for or depend on any of it.
//!
//! **Off by default.** `TELEPHONY_NUMBERING_USAGE_REPORTS_ENABLED` and
//! `TELEPHONY_NUMBERING_RISK_SIGNALS_ENABLED` (both `false`): enabling them in an
//! environment needs Numbering's trust bundle to grant `cfmeet.telephony` the
//! `number-usage.report` / `number-risk.report` permissions first (operator step). While
//! a switch is off nothing is written, so turning it on never floods Numbering with a
//! backlog; reports already queued are held (not dropped) until it is on again.
//!
//! Payloads carry CF Meet ids, counts and timestamps only — never a telephone number,
//! provider id or provider call id.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    config::{invalid, ConfigError},
    db,
    error::TelephonyResult,
    incidents::{self, Incident, Severity},
    legs::LegRow,
    ports::PortError,
    service::TelephonyService,
};

/// Delivery attempts before a report is dead-lettered (with the 300 s backoff cap, about
/// seventeen hours of Numbering being unavailable or the permission not yet granted).
pub const MAX_REPORT_ATTEMPTS: i32 = 200;
/// The longest wait between two attempts.
pub const REPORT_BACKOFF_CAP_SECONDS: i64 = 300;
/// How long a claimed report is held before another worker may retry it.
const CLAIM_LEASE_SECONDS: i64 = 60;
/// DELIVERED reports are kept this long (evidence), then purged.
pub const DELIVERED_RETENTION_DAYS: i64 = 30;
/// A connected call shorter than this counts as a nuisance call.
const SHORT_CALL_SECONDS: i64 = 6;

/// The Numbering reporting switches and the DCN call-pattern thresholds.
///
/// | Variable | Default | Meaning |
/// |---|---|---|
/// | `TELEPHONY_NUMBERING_USAGE_REPORTS_ENABLED` | `false` | report finalized DCN voice seconds to Numbering |
/// | `TELEPHONY_NUMBERING_RISK_SIGNALS_ENABLED` | `false` | report DCN call-pattern signals to Numbering |
/// | `TELEPHONY_DCN_CALL_VELOCITY_THRESHOLD` / `…_WINDOW_MINUTES` | 30 / 10 | outbound calls from one DCN → `SPAM_CALLS` |
/// | `TELEPHONY_DCN_NUISANCE_THRESHOLD` / `…_WINDOW_MINUTES` | 20 / 60 | unanswered or sub-6 s outbound calls from one DCN → `NUISANCE_CALLS` |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberingReportSettings {
    pub usage_reports_enabled: bool,
    pub risk_signals_enabled: bool,
    /// (threshold, window minutes)
    pub dcn_call_velocity: (i64, i64),
    /// (threshold, window minutes)
    pub dcn_nuisance: (i64, i64),
}

impl Default for NumberingReportSettings {
    fn default() -> Self {
        Self {
            usage_reports_enabled: false,
            risk_signals_enabled: false,
            dcn_call_velocity: (30, 10),
            dcn_nuisance: (20, 60),
        }
    }
}

impl NumberingReportSettings {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|k: &str| std::env::var(k).ok())
    }

    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let value = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let flag = |var: &'static str| match value(var).as_deref() {
            None | Some("false") => Ok(false),
            Some("true") => Ok(true),
            Some(_) => Err(invalid(var, "must be true or false")),
        };
        let bounded = |var: &'static str, current: i64, max: i64| match value(var) {
            None => Ok(current),
            Some(v) => v
                .parse::<i64>()
                .ok()
                .filter(|n| (1..=max).contains(n))
                .ok_or_else(|| invalid(var, format!("must be an integer between 1 and {max}"))),
        };
        let d = Self::default();
        Ok(Self {
            usage_reports_enabled: flag("TELEPHONY_NUMBERING_USAGE_REPORTS_ENABLED")?,
            risk_signals_enabled: flag("TELEPHONY_NUMBERING_RISK_SIGNALS_ENABLED")?,
            dcn_call_velocity: (
                bounded(
                    "TELEPHONY_DCN_CALL_VELOCITY_THRESHOLD",
                    d.dcn_call_velocity.0,
                    100_000,
                )?,
                bounded(
                    "TELEPHONY_DCN_CALL_VELOCITY_WINDOW_MINUTES",
                    d.dcn_call_velocity.1,
                    1_440,
                )?,
            ),
            dcn_nuisance: (
                bounded(
                    "TELEPHONY_DCN_NUISANCE_THRESHOLD",
                    d.dcn_nuisance.0,
                    100_000,
                )?,
                bounded(
                    "TELEPHONY_DCN_NUISANCE_WINDOW_MINUTES",
                    d.dcn_nuisance.1,
                    1_440,
                )?,
            ),
        })
    }
}

#[derive(Debug, FromQueryResult)]
struct DcnEvidence {
    dcn_number_resource_id: Option<Uuid>,
    dcn_assignment_id: Option<Uuid>,
}

/// Writes the `USAGE` report of a finalized leg (inside the finalizing transaction) when
/// usage reporting is on, the leg carried a DCN and it was billable. Returns whether a
/// report was written.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn enqueue_leg_usage<C: ConnectionTrait>(
    txn: &C,
    settings: &NumberingReportSettings,
    leg: &LegRow,
    billable_seconds: i32,
    provider_cost: Option<(i64, String)>,
    now: DateTime<Utc>,
) -> TelephonyResult<bool> {
    if !settings.usage_reports_enabled || billable_seconds <= 0 {
        return Ok(false);
    }
    let evidence: Option<DcnEvidence> = db::one(
        txn,
        "SELECT dcn_number_resource_id, dcn_assignment_id FROM telephony_legs WHERE id = $1",
        vec![leg.id.into()],
    )
    .await?;
    let Some((number_resource_id, assignment_id)) =
        evidence.and_then(|e| e.dcn_number_resource_id.zip(e.dcn_assignment_id))
    else {
        return Ok(false);
    };
    let usage_kind = if leg.direction == "INBOUND" {
        "VOICE_INBOUND_SECONDS"
    } else {
        "VOICE_OUTBOUND_SECONDS"
    };
    let period_end = leg.ended_at.unwrap_or(now);
    let period_start = leg
        .connected_at
        .or(leg.answered_at)
        .unwrap_or(period_end)
        .min(period_end);
    let source_key = format!("voice-leg:{}", leg.id);
    let mut payload = json!({
        "sourceKey": source_key,
        "numberResourceId": number_resource_id,
        "assignmentId": assignment_id,
        "usageKind": usage_kind,
        "quantity": billable_seconds,
        "periodStart": period_start,
        "periodEnd": period_end,
    });
    if let Some((cost, currency)) = provider_cost {
        payload["providerCostMinor"] = json!(cost);
        payload["currency"] = json!(currency);
    }
    let inserted = insert_report(
        txn,
        "USAGE",
        &source_key,
        number_resource_id,
        Some(leg.id),
        payload,
        now,
    )
    .await?;
    Ok(inserted)
}

async fn insert_report<C: ConnectionTrait>(
    db_: &C,
    kind: &'static str,
    source_key: &str,
    number_resource_id: Uuid,
    leg: Option<Uuid>,
    payload: Value,
    now: DateTime<Utc>,
) -> TelephonyResult<bool> {
    let n = db::exec(
        db_,
        "INSERT INTO telephony_numbering_reports (id, report_kind, source_key, number_resource_id, \
           telephony_leg_id, payload, status, attempts, next_attempt_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'PENDING', 0, $7, $7) ON CONFLICT (source_key) DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            kind.into(),
            source_key.into(),
            number_resource_id.into(),
            leg.into(),
            payload.into(),
            now.into(),
        ],
    )
    .await?;
    if n == 1 {
        crate::metrics::inc(
            "telephony_numbering_reports_queued_total",
            &[("kind", kind)],
        );
    }
    Ok(n == 1)
}

/// The magnitude of a call-pattern signal: 50 at the threshold, 100 at twice it.
pub fn signal_magnitude(count: i64, threshold: i64) -> i32 {
    let threshold = threshold.max(1);
    (count.saturating_mul(50) / threshold).clamp(0, 100) as i32
}

#[derive(Debug, FromQueryResult)]
struct DcnHit {
    subject: Uuid,
    n: i64,
}

#[derive(Debug, FromQueryResult)]
struct ReportRow {
    id: Uuid,
    report_kind: String,
    payload: Value,
    attempts: i32,
}

/// What one delivery pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReportDelivery {
    pub delivered: usize,
    pub retried: usize,
    pub dead_lettered: usize,
}

impl TelephonyService {
    /// The DCN call-pattern scan (run from the rate-limited abuse scan). Raises the local
    /// `ABUSE_SIGNAL` incident for operators and, when risk signals are on, queues one
    /// Numbering signal per DCN / kind / window bucket / magnitude band. Returns the number
    /// of hits.
    pub(crate) async fn dcn_risk_scan(&self, now: DateTime<Utc>) -> TelephonyResult<usize> {
        let s = self.numbering_reports.clone();
        let mut hits_total = 0;
        for (kind, signal, (threshold, minutes), sql) in [
            (
                "DCN_CALL_VELOCITY",
                "SPAM_CALLS",
                s.dcn_call_velocity,
                "SELECT dcn_number_resource_id AS subject, count(*) AS n FROM telephony_legs \
                 WHERE direction = 'OUTBOUND' AND dcn_number_resource_id IS NOT NULL AND created_at > $1 \
                 GROUP BY dcn_number_resource_id HAVING count(*) >= $2"
                    .to_string(),
            ),
            (
                "DCN_NUISANCE_CALLS",
                "NUISANCE_CALLS",
                s.dcn_nuisance,
                format!(
                    "SELECT dcn_number_resource_id AS subject, count(*) AS n FROM telephony_legs \
                     WHERE direction = 'OUTBOUND' AND dcn_number_resource_id IS NOT NULL AND ended_at > $1 \
                       AND (end_reason = 'NO_ANSWER' OR (connected_at IS NOT NULL \
                            AND ended_at < connected_at + interval '{SHORT_CALL_SECONDS} seconds')) \
                     GROUP BY dcn_number_resource_id HAVING count(*) >= $2"
                ),
            ),
        ] {
            let hits: Vec<DcnHit> = db::all(
                &self.db,
                &sql,
                vec![(now - Duration::minutes(minutes)).into(), threshold.into()],
            )
            .await?;
            for hit in hits {
                hits_total += 1;
                self.raise_abuse_signal(kind, hit.subject, hit.n, minutes, now)
                    .await?;
                if !s.risk_signals_enabled {
                    continue;
                }
                let magnitude = signal_magnitude(hit.n, threshold);
                // One signal per DCN / kind / window bucket / magnitude band (a growing
                // pattern escalates; a repeated scan of the same pattern does not).
                let bucket = now.timestamp() / (minutes * 60);
                let source_key = format!(
                    "dcn-{}:{}:{bucket}:{}",
                    signal.to_ascii_lowercase().replace('_', "-"),
                    hit.subject.simple(),
                    magnitude / 25
                );
                insert_report(
                    &self.db,
                    "RISK_SIGNAL",
                    &source_key,
                    hit.subject,
                    None,
                    json!({
                        "numberResourceId": hit.subject,
                        "signalKind": signal,
                        "magnitude": magnitude,
                        "sourceKey": source_key,
                    }),
                    now,
                )
                .await?;
            }
        }
        Ok(hits_total)
    }

    /// Delivers due Numbering reports (bounded batch). Reports of a kind whose switch is
    /// off are held, not dropped.
    pub async fn deliver_numbering_reports_once(
        &self,
        limit: u64,
    ) -> TelephonyResult<ReportDelivery> {
        let s = &self.numbering_reports;
        let mut out = ReportDelivery::default();
        let kinds: Vec<&str> = [
            ("USAGE", s.usage_reports_enabled),
            ("RISK_SIGNAL", s.risk_signals_enabled),
        ]
        .into_iter()
        .filter_map(|(k, on)| on.then_some(k))
        .collect();
        if kinds.is_empty() {
            return Ok(out);
        }
        let now = self.now();
        let kinds_sql = kinds
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(", ");
        // Claim (SKIP LOCKED + a short lease): two workers never send the same report at
        // once; a worker that dies mid-delivery leaves it to be retried after the lease.
        let due: Vec<ReportRow> = db::all(
            &self.db,
            &format!(
                "UPDATE telephony_numbering_reports SET next_attempt_at = $2 \
                 WHERE id IN (SELECT id FROM telephony_numbering_reports \
                   WHERE status = 'PENDING' AND report_kind IN ({kinds_sql}) AND next_attempt_at <= $1 \
                   ORDER BY next_attempt_at LIMIT $3 FOR UPDATE SKIP LOCKED) \
                 RETURNING id, report_kind, payload, attempts"
            ),
            vec![
                now.into(),
                (now + Duration::seconds(CLAIM_LEASE_SECONDS)).into(),
                (limit.clamp(1, 500) as i64).into(),
            ],
        )
        .await?;
        for row in due {
            let result = match row.report_kind.as_str() {
                "USAGE" => self.numbering.report_usage(&row.payload).await,
                _ => self.numbering.report_risk_signal(&row.payload).await,
            };
            let now = self.now();
            let failure = match result {
                Ok(()) => {
                    db::exec(
                        &self.db,
                        "UPDATE telephony_numbering_reports SET status = 'DELIVERED', delivered_at = $2, \
                           attempts = attempts + 1, last_error = NULL WHERE id = $1 AND status = 'PENDING'",
                        vec![row.id.into(), now.into()],
                    )
                    .await?;
                    crate::metrics::inc(
                        "telephony_numbering_reports_total",
                        &[
                            ("kind", kind_label(&row.report_kind)),
                            ("result", "DELIVERED"),
                        ],
                    );
                    out.delivered += 1;
                    continue;
                }
                Err(e) => e,
            };
            let (code, permanent) = match &failure {
                PortError::Unavailable => ("NUMBERING_UNAVAILABLE".to_string(), false),
                PortError::NotFound => ("NOT_FOUND".to_string(), true),
                PortError::Denied(code) => (bounded_code(code), true),
            };
            let attempts = row.attempts + 1;
            if !permanent && attempts < MAX_REPORT_ATTEMPTS {
                let backoff = (1i64 << attempts.clamp(0, 9)).min(REPORT_BACKOFF_CAP_SECONDS);
                db::exec(
                    &self.db,
                    "UPDATE telephony_numbering_reports SET attempts = $2, last_error = $3, \
                       next_attempt_at = $4 WHERE id = $1 AND status = 'PENDING'",
                    vec![
                        row.id.into(),
                        attempts.into(),
                        code.into(),
                        (now + Duration::seconds(backoff)).into(),
                    ],
                )
                .await?;
                crate::metrics::inc(
                    "telephony_numbering_reports_total",
                    &[("kind", kind_label(&row.report_kind)), ("result", "RETRY")],
                );
                out.retried += 1;
                continue;
            }
            let reason = if permanent {
                "NUMBERING_REFUSED"
            } else {
                "ATTEMPTS_EXHAUSTED"
            };
            let txn = self.db.begin().await?;
            let marked = db::exec(
                &txn,
                "UPDATE telephony_numbering_reports SET status = 'DEAD_LETTER', dead_lettered_at = $2, \
                   attempts = $3, last_error = $4 WHERE id = $1 AND status = 'PENDING'",
                vec![
                    row.id.into(),
                    now.into(),
                    attempts.into(),
                    code.clone().into(),
                ],
            )
            .await?;
            if marked == 1 {
                incidents::raise(
                    &txn,
                    &Incident::new(
                        "OUTBOX_DEAD_LETTER",
                        Severity::Medium,
                        format!("numbering-report-dead-letter:{}:{reason}", row.report_kind),
                    )
                    .evidence(json!({
                        "destination": "NUMBERING",
                        "reportKind": row.report_kind,
                        "reason": reason,
                        "code": code,
                        "attempts": attempts,
                    })),
                    now,
                )
                .await?;
            }
            txn.commit().await?;
            crate::metrics::inc(
                "telephony_numbering_reports_total",
                &[
                    ("kind", kind_label(&row.report_kind)),
                    ("result", "DEAD_LETTER"),
                ],
            );
            tracing::warn!(
                reason,
                code = code.as_str(),
                "Numbering report dead-lettered; it will not be retried"
            );
            out.dead_lettered += 1;
        }
        // Delivered evidence is kept for a bounded time.
        db::exec(
            &self.db,
            "DELETE FROM telephony_numbering_reports WHERE id IN (SELECT id FROM telephony_numbering_reports \
               WHERE status = 'DELIVERED' AND delivered_at < $1 LIMIT 500)",
            vec![(now - Duration::days(DELIVERED_RETENTION_DAYS)).into()],
        )
        .await?;
        Ok(out)
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "USAGE" => "USAGE",
        _ => "RISK_SIGNAL",
    }
}

/// A stable, bounded error code (Numbering's envelope code, else a generic one).
fn bounded_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        code.to_string()
    } else {
        "DENIED".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn switches_default_off_and_parse_strictly() {
        let none = NumberingReportSettings::from_lookup(&|_| None).unwrap();
        assert_eq!(none, NumberingReportSettings::default());
        assert!(!none.usage_reports_enabled && !none.risk_signals_enabled);
        let env: HashMap<&str, &str> = [
            ("TELEPHONY_NUMBERING_USAGE_REPORTS_ENABLED", "true"),
            ("TELEPHONY_NUMBERING_RISK_SIGNALS_ENABLED", "true"),
            ("TELEPHONY_DCN_CALL_VELOCITY_THRESHOLD", "5"),
            ("TELEPHONY_DCN_NUISANCE_WINDOW_MINUTES", "30"),
        ]
        .into();
        let on =
            NumberingReportSettings::from_lookup(&|k| env.get(k).map(|v| v.to_string())).unwrap();
        assert!(on.usage_reports_enabled && on.risk_signals_enabled);
        assert_eq!(on.dcn_call_velocity, (5, 10));
        assert_eq!(on.dcn_nuisance, (20, 30));
        for (var, bad) in [
            ("TELEPHONY_NUMBERING_USAGE_REPORTS_ENABLED", "yes"),
            ("TELEPHONY_DCN_CALL_VELOCITY_THRESHOLD", "0"),
            ("TELEPHONY_DCN_NUISANCE_WINDOW_MINUTES", "2000"),
        ] {
            assert!(
                NumberingReportSettings::from_lookup(&|k| (k == var).then(|| bad.to_string()))
                    .is_err(),
                "{var}={bad}"
            );
        }
    }

    #[test]
    fn magnitude_grows_with_the_multiple_of_the_threshold() {
        assert_eq!(signal_magnitude(30, 30), 50);
        assert_eq!(signal_magnitude(45, 30), 75);
        assert_eq!(signal_magnitude(60, 30), 100);
        assert_eq!(signal_magnitude(600, 30), 100);
        assert_eq!(signal_magnitude(1, 0), 50);
    }

    #[test]
    fn error_codes_are_bounded() {
        assert_eq!(
            bounded_code("USAGE_SOURCE_CONFLICT"),
            "USAGE_SOURCE_CONFLICT"
        );
        assert_eq!(bounded_code("not a code"), "DENIED");
        assert_eq!(bounded_code(&"A".repeat(65)), "DENIED");
    }
}
