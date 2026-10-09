//! Part G §70: `limit.warning` / `limit.exceeded` (ADR 0028).
//!
//! A notice says which limit (`REQUEST_RATE`, `CONCURRENT_CALLS`, `TELECOM_SPEND_DAILY`,
//! `TELECOM_SPEND_MONTHLY`), at which level (`TENANT`, `APPLICATION`, `SERVICE_ACCOUNT`,
//! `CREDENTIAL`, `PRINCIPAL`, `ENVIRONMENT`), over which window, and the numbers
//! (`current` / `ceiling`; a warning also carries its `threshold` percentage). Nothing
//! else: the subject's identifier (which service account, credential or principal) is
//! never part of the payload or of a metric label — only of the private dedupe key.
//!
//! Notices are deduplicated per (kind, level, subject, window, receiving application
//! environment): a burst of refused requests in one minute is ONE `limit.exceeded`, a
//! day's spend crossing 80 % is ONE `limit.warning`. A warning is emitted only when a
//! change CROSSES the threshold, so a steady state emits nothing. Concurrent-call
//! notices (no natural window) are throttled to one per minute.
//!
//! Notices are best-effort signals, never the enforcement: a failed emission is logged
//! and counted, and the decision (allow / refuse) stands.

use chrono::{DateTime, Duration, DurationRound, Utc};
use sea_orm::ConnectionTrait;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{catalog, ingest_public_event, worker::PRODUCER_CALLING};

/// A warning is emitted when usage crosses this percentage of a ceiling.
pub const WARNING_PERCENT: i64 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    RequestRate,
    ConcurrentCalls,
    TelecomSpendDaily,
    TelecomSpendMonthly,
}

impl LimitKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LimitKind::RequestRate => "REQUEST_RATE",
            LimitKind::ConcurrentCalls => "CONCURRENT_CALLS",
            LimitKind::TelecomSpendDaily => "TELECOM_SPEND_DAILY",
            LimitKind::TelecomSpendMonthly => "TELECOM_SPEND_MONTHLY",
        }
    }

    fn window(&self) -> &'static str {
        match self {
            LimitKind::RequestRate | LimitKind::ConcurrentCalls => "MINUTE",
            LimitKind::TelecomSpendDaily => "DAY",
            LimitKind::TelecomSpendMonthly => "MONTH",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitLevel {
    Tenant,
    Application,
    ServiceAccount,
    Credential,
    Principal,
    Environment,
}

impl LimitLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LimitLevel::Tenant => "TENANT",
            LimitLevel::Application => "APPLICATION",
            LimitLevel::ServiceAccount => "SERVICE_ACCOUNT",
            LimitLevel::Credential => "CREDENTIAL",
            LimitLevel::Principal => "PRINCIPAL",
            LimitLevel::Environment => "ENVIRONMENT",
        }
    }
}

/// One crossing of a limit.
#[derive(Debug, Clone)]
pub struct LimitNotice {
    pub kind: LimitKind,
    pub level: LimitLevel,
    /// The tenant / application / credential / environment whose ceiling it is.
    pub subject: Uuid,
    pub exceeded: bool,
    pub current: i64,
    pub ceiling: i64,
    /// Start of the window the notice belongs to (its dedupe bucket).
    pub window_start: DateTime<Utc>,
}

impl LimitNotice {
    pub fn exceeded(
        kind: LimitKind,
        level: LimitLevel,
        subject: Uuid,
        current: i64,
        ceiling: i64,
        window_start: DateTime<Utc>,
    ) -> Self {
        Self {
            kind,
            level,
            subject,
            exceeded: true,
            current,
            ceiling,
            window_start,
        }
    }

    /// A warning when the change `before → after` crosses [`WARNING_PERCENT`] of
    /// `ceiling` (and does not exceed it).
    pub fn warning_if_crossed(
        kind: LimitKind,
        level: LimitLevel,
        subject: Uuid,
        before: i64,
        after: i64,
        ceiling: i64,
        window_start: DateTime<Utc>,
    ) -> Option<Self> {
        crosses_warning(before, after, ceiling).then_some(Self {
            kind,
            level,
            subject,
            exceeded: false,
            current: after,
            ceiling,
            window_start,
        })
    }

    fn source_event_id(&self, scope: (Uuid, Uuid, Uuid)) -> String {
        let mut h = Sha256::new();
        for part in [
            if self.exceeded { "X" } else { "W" },
            self.kind.as_str(),
            self.level.as_str(),
        ] {
            h.update(part.as_bytes());
            h.update(b"|");
        }
        for id in [self.subject, scope.0, scope.1, scope.2] {
            h.update(id.as_bytes());
        }
        h.update(self.window_start.timestamp().to_be_bytes());
        // 4 + 48 = 52 characters (the column holds 64).
        format!("lim_{}", &hex::encode(h.finalize())[..48])
    }

    fn public_event(&self) -> catalog::PublicEvent {
        let mut raw = serde_json::json!({
            "limit": self.kind.as_str(),
            "scope": self.level.as_str(),
            "window": self.kind.window(),
            "current": self.current.max(0),
            "ceiling": self.ceiling.max(0),
        });
        if !self.exceeded {
            raw["threshold"] = Value::from(WARNING_PERCENT);
        }
        let mut data = Map::new();
        catalog::safe_fields(&[&raw], &mut data);
        catalog::PublicEvent {
            event_type: if self.exceeded {
                "limit.exceeded"
            } else {
                "limit.warning"
            },
            data,
        }
    }
}

/// Whether `before → after` crosses the warning threshold of `ceiling` without
/// exceeding it.
pub fn crosses_warning(before: i64, after: i64, ceiling: i64) -> bool {
    ceiling > 0
        && before * 100 < ceiling * WARNING_PERCENT
        && after * 100 >= ceiling * WARNING_PERCENT
        && after <= ceiling
}

/// The one-minute bucket of `now` (window of rate and concurrency notices).
pub fn minute_of(now: DateTime<Utc>) -> DateTime<Utc> {
    now.duration_trunc(Duration::minutes(1)).unwrap_or(now)
}

/// Emits the notices from their own task, on their own connection: for a refusal whose
/// transaction is about to roll back. The caller never waits for (or holds a second pool
/// connection for) the emission — a refused burst cannot starve the pool.
pub fn emit_detached(
    db: sea_orm::DatabaseConnection,
    now: DateTime<Utc>,
    scope: (Uuid, Uuid, Uuid),
    notices: Vec<LimitNotice>,
) {
    if notices.is_empty() {
        return;
    }
    tokio::spawn(async move { emit(&db, now, scope, &notices).await });
}

/// Emits the notices to the subscriptions of the application environment `scope`
/// (tenant, application, environment) — on `db` (a transaction makes them atomic with
/// the change that crossed the threshold). Errors are logged, never returned.
pub async fn emit<C: ConnectionTrait>(
    db: &C,
    now: DateTime<Utc>,
    scope: (Uuid, Uuid, Uuid),
    notices: &[LimitNotice],
) {
    for notice in notices {
        let event = notice.public_event();
        let result = ingest_public_event(
            db,
            now,
            PRODUCER_CALLING,
            &notice.source_event_id(scope),
            scope,
            now,
            None,
            &event,
        )
        .await;
        let outcome = match result {
            Ok(_) => "EMITTED",
            Err(_) => {
                tracing::warn!(
                    limit = notice.kind.as_str(),
                    level = notice.level.as_str(),
                    "limit notice not emitted"
                );
                "FAILED"
            }
        };
        crate::metrics::inc(
            "calling_limit_notices_total",
            &[
                ("type", event.event_type),
                ("limit", notice.kind.as_str()),
                ("result", outcome),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_fires_only_on_the_crossing() {
        assert!(crosses_warning(7, 8, 10));
        assert!(!crosses_warning(8, 9, 10), "already above: steady state");
        assert!(!crosses_warning(6, 7, 10));
        assert!(!crosses_warning(7, 11, 10), "exceeded is not a warning");
        assert!(!crosses_warning(0, 0, 0));
        assert!(crosses_warning(0, 4_000, 5_000));
    }

    #[test]
    fn notices_are_bounded_and_deduplicated_by_window() {
        let t = DateTime::parse_from_rfc3339("2026-10-09T10:00:30Z")
            .unwrap()
            .with_timezone(&Utc);
        let scope = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let subject = Uuid::new_v4();
        let n = |at| {
            LimitNotice::exceeded(
                LimitKind::RequestRate,
                LimitLevel::Credential,
                subject,
                11,
                10,
                minute_of(at),
            )
        };
        let a = n(t).source_event_id(scope);
        assert_eq!(a, n(t + Duration::seconds(20)).source_event_id(scope));
        assert_ne!(a, n(t + Duration::seconds(40)).source_event_id(scope));
        assert!(a.len() <= 64);
        let event = n(t).public_event();
        assert_eq!(event.event_type, "limit.exceeded");
        assert_eq!(
            Value::Object(event.data),
            serde_json::json!({"limit": "REQUEST_RATE", "scope": "CREDENTIAL",
                               "window": "MINUTE", "current": 11, "ceiling": 10})
        );
    }
}
