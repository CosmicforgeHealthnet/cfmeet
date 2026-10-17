//! Outbound safety controls and opt-out (Part IV O; the STOP keywords already apply to
//! inbound messages, Workstream N). Every refusal has a stable reason code:
//!
//! | Reason | Meaning |
//! |---|---|
//! | `RATE_LIMITED` | the environment's per-minute message rate is exhausted (429, not recorded) |
//! | `DESTINATION_RATE_LIMITED` | the recipient's per-hour rate is exhausted (429, not recorded) |
//! | `DESTINATION_NOT_ALLOWED` | the recipient is outside the allowed E.164 prefixes |
//! | `RECIPIENT_OPTED_OUT` | the recipient opted out (STOP) or was blocked |
//! | `SPEND_LIMIT_EXCEEDED` | the environment's daily estimated spend would be exceeded |
//! | `ABUSE_SUSPECTED` | identical content to too many distinct recipients within an hour |
//! | `SENDER_NOT_AUTHORIZED` | Numbering refused the sender (not owned, not usable, principal) |
//! | `PREMIUM_DESTINATION_BLOCKED` | a premium-rate / high-risk destination range |
//! | `TENANT_RATE_LIMITED` / `NUMBER_RATE_LIMITED` / `PRINCIPAL_RATE_LIMITED` | the tenant's / sender number's / principal's per-minute rate (429, not recorded) |
//! | `TENANT_SPEND_LIMIT_EXCEEDED` | the tenant's daily estimated spend would be exceeded |
//! | `REPEATED_DESTINATION` | identical content to the same recipient too often within an hour |
//! | `COST_SPIKE_CIRCUIT_OPEN` | the provider's confirmed cost per segment spiked above the estimate |
//! | `PROVIDER_ANOMALY` | too many recent attempts of the provider were rejected or unknown |
//!
//! Spend is counted conservatively: a message without a confirmed cost counts its
//! estimate, an UNKNOWN one included; only a definitely-unsent message is voided.
//!
//! The full allocation-fraud / investigation engine is a later workstream; these are the
//! transport's own guards.

use chrono::{DateTime, Duration, DurationRound, Utc};
use sea_orm::{ConnectionTrait, DbErr};
use uuid::Uuid;

use super::events::EventScope;
use crate::{config::ControlSettings, db};

pub const RATE_LIMITED: &str = "RATE_LIMITED";
pub const DESTINATION_RATE_LIMITED: &str = "DESTINATION_RATE_LIMITED";
pub const DESTINATION_NOT_ALLOWED: &str = "DESTINATION_NOT_ALLOWED";
pub const RECIPIENT_OPTED_OUT: &str = "RECIPIENT_OPTED_OUT";
pub const SPEND_LIMIT_EXCEEDED: &str = "SPEND_LIMIT_EXCEEDED";
pub const ABUSE_SUSPECTED: &str = "ABUSE_SUSPECTED";
pub const SENDER_NOT_AUTHORIZED: &str = "SENDER_NOT_AUTHORIZED";
pub const PREMIUM_DESTINATION_BLOCKED: &str = "PREMIUM_DESTINATION_BLOCKED";
pub const TENANT_RATE_LIMITED: &str = "TENANT_RATE_LIMITED";
pub const NUMBER_RATE_LIMITED: &str = "NUMBER_RATE_LIMITED";
pub const PRINCIPAL_RATE_LIMITED: &str = "PRINCIPAL_RATE_LIMITED";
pub const TENANT_SPEND_LIMIT_EXCEEDED: &str = "TENANT_SPEND_LIMIT_EXCEEDED";
pub const REPEATED_DESTINATION: &str = "REPEATED_DESTINATION";
pub const COST_SPIKE_CIRCUIT_OPEN: &str = "COST_SPIKE_CIRCUIT_OPEN";
pub const PROVIDER_ANOMALY: &str = "PROVIDER_ANOMALY";

/// Every reason code a refusal may carry.
pub const REASONS: &[&str] = &[
    RATE_LIMITED,
    DESTINATION_RATE_LIMITED,
    DESTINATION_NOT_ALLOWED,
    RECIPIENT_OPTED_OUT,
    SPEND_LIMIT_EXCEEDED,
    ABUSE_SUSPECTED,
    SENDER_NOT_AUTHORIZED,
    PREMIUM_DESTINATION_BLOCKED,
    TENANT_RATE_LIMITED,
    NUMBER_RATE_LIMITED,
    PRINCIPAL_RATE_LIMITED,
    TENANT_SPEND_LIMIT_EXCEEDED,
    REPEATED_DESTINATION,
    COST_SPIKE_CIRCUIT_OPEN,
    PROVIDER_ANOMALY,
];

/// The static reason code of a stored refusal (unknown codes fall back to ABUSE_SUSPECTED,
/// never echoing stored text).
pub fn reason_code(stored: &str) -> &'static str {
    REASONS
        .iter()
        .copied()
        .find(|r| *r == stored)
        .unwrap_or(ABUSE_SUSPECTED)
}

/// A keyword the recipient sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptKeyword {
    Stop,
    Start,
}

/// The industry opt-out / opt-in keywords (the whole message, case-insensitive).
pub fn keyword(text: &str) -> Option<OptKeyword> {
    let t = text
        .trim()
        .trim_end_matches(['.', '!'])
        .to_ascii_uppercase();
    match t.as_str() {
        "STOP" | "STOPALL" | "UNSUBSCRIBE" | "CANCEL" | "END" | "QUIT" | "OPTOUT" => {
            Some(OptKeyword::Stop)
        }
        "START" | "UNSTOP" | "YES" => Some(OptKeyword::Start),
        _ => None,
    }
}

/// Counts one hit of `subject` in the fixed window of `width` containing `now`; `true`
/// while the count stays within `limit`.
pub async fn hit<C: ConnectionTrait>(
    db: &C,
    kind: &str,
    subject: &str,
    width: Duration,
    limit: i64,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let window = now.duration_trunc(width).unwrap_or(now);
    let hits = db::scalar_i64(
        db,
        "INSERT INTO sms_rate_windows (subject_kind, subject_key, window_start, hits) VALUES ($1, $2, $3, 1) \
         ON CONFLICT (subject_kind, subject_key, window_start) DO UPDATE SET hits = sms_rate_windows.hits + 1 \
         RETURNING hits::bigint",
        vec![kind.into(), subject.into(), window.into()],
    )
    .await?;
    Ok(hits <= limit)
}

/// The current count of `subject`'s fixed window (read-only pre-check).
pub async fn window_count<C: ConnectionTrait>(
    db: &C,
    kind: &str,
    subject: &str,
    width: Duration,
    now: DateTime<Utc>,
) -> Result<i64, DbErr> {
    let window = now.duration_trunc(width).unwrap_or(now);
    db::scalar_i64(
        db,
        "SELECT COALESCE(max(hits), 0)::bigint FROM sms_rate_windows \
          WHERE subject_kind = $1 AND subject_key = $2 AND window_start = $3",
        vec![kind.into(), subject.into(), window.into()],
    )
    .await
}

/// Whether the recipient is blocked for this number (or for every number of the
/// environment).
pub async fn opted_out<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    number_resource_id: Uuid,
    remote_fingerprint: &str,
) -> Result<bool, DbErr> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM sms_optout_blocks WHERE environment_id = $1 AND remote_fingerprint = $3 \
           AND lifted_at IS NULL AND (number_resource_id IS NULL OR number_resource_id = $2)",
        vec![
            environment_id.into(),
            number_resource_id.into(),
            remote_fingerprint.into(),
        ],
    )
    .await?
        > 0)
}

/// Blocks the recipient for `number` (idempotent).
pub async fn block<C: ConnectionTrait>(
    db: &C,
    scope: EventScope,
    number_resource_id: Option<Uuid>,
    remote_fingerprint: &str,
    reason: &str,
    source_transport_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    Ok(db::exec(
        db,
        "INSERT INTO sms_optout_blocks (id, tenant_id, application_id, environment_id, number_resource_id, \
           remote_fingerprint, reason, source_transport_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            number_resource_id.into(),
            remote_fingerprint.into(),
            reason.into(),
            source_transport_id.into(),
            now.into(),
        ],
    )
    .await?
        > 0)
}

/// Lifts the recipient's STOP block for `number` (an opt-in keyword). Customer and abuse
/// blocks are not lifted by the recipient.
pub async fn lift_stop<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    number_resource_id: Uuid,
    remote_fingerprint: &str,
    now: DateTime<Utc>,
) -> Result<u64, DbErr> {
    db::exec(
        db,
        "UPDATE sms_optout_blocks SET lifted_at = $4 WHERE environment_id = $1 AND number_resource_id = $2 \
           AND remote_fingerprint = $3 AND reason = 'STOP_KEYWORD' AND lifted_at IS NULL",
        vec![
            environment_id.into(),
            number_resource_id.into(),
            remote_fingerprint.into(),
            now.into(),
        ],
    )
    .await
}

/// Estimated outbound spend of the environment since the start of the UTC day.
pub async fn spent_today<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<i64, DbErr> {
    let day = now.duration_trunc(Duration::days(1)).unwrap_or(now);
    db::scalar_i64(
        db,
        "SELECT COALESCE(sum(COALESCE(confirmed_cost_minor, estimated_cost_minor)), 0)::bigint FROM sms_usage_records \
         WHERE environment_id = $1 AND direction = 'OUTBOUND' AND status <> 'VOIDED' AND created_at >= $2",
        vec![environment_id.into(), day.into()],
    )
    .await
}

/// Distinct recipients of identical content from the environment within the last hour.
pub async fn identical_recipients<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    content_fingerprint: &str,
    remote_fingerprint: &str,
    now: DateTime<Utc>,
) -> Result<i64, DbErr> {
    db::scalar_i64(
        db,
        "SELECT count(DISTINCT remote_fingerprint) FROM sms_transports WHERE environment_id = $1 \
           AND direction = 'OUTBOUND' AND content_fingerprint = $2 AND created_at >= $3 \
           AND remote_fingerprint <> $4 AND blocked_reason IS NULL",
        vec![
            environment_id.into(),
            content_fingerprint.into(),
            (now - Duration::hours(1)).into(),
            remote_fingerprint.into(),
        ],
    )
    .await
}

/// Whether a canonical number is a premium / high-risk destination (blocked whatever the
/// allowed prefixes say).
pub fn premium(e164: &str, blocked_prefixes: &[String]) -> bool {
    blocked_prefixes
        .iter()
        .any(|p| e164.starts_with(p.as_str()))
}

/// The environment's (or tenant's) estimated + confirmed outbound spend today
/// (conservative: an unconfirmed message counts its estimate, an UNKNOWN one too).
pub async fn spent_today_by<C: ConnectionTrait>(
    db: &C,
    column: &'static str,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<i64, DbErr> {
    debug_assert!(matches!(column, "environment_id" | "tenant_id"));
    let day = now.duration_trunc(Duration::days(1)).unwrap_or(now);
    db::scalar_i64(
        db,
        &format!(
            "SELECT COALESCE(sum(COALESCE(confirmed_cost_minor, estimated_cost_minor)), 0)::bigint \
               FROM sms_usage_records \
              WHERE {column} = $1 AND direction = 'OUTBOUND' AND status <> 'VOIDED' AND created_at >= $2"
        ),
        vec![id.into(), day.into()],
    )
    .await
}

/// Identical content already sent (or being sent) to the same recipient within the hour.
pub async fn repeated_to_recipient<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    content_fingerprint: &str,
    remote_fingerprint: &str,
    now: DateTime<Utc>,
) -> Result<i64, DbErr> {
    db::scalar_i64(
        db,
        "SELECT count(*) FROM sms_transports WHERE environment_id = $1 AND direction = 'OUTBOUND' \
           AND content_fingerprint = $2 AND remote_fingerprint = $3 AND created_at >= $4 \
           AND blocked_reason IS NULL",
        vec![
            environment_id.into(),
            content_fingerprint.into(),
            remote_fingerprint.into(),
            (now - Duration::hours(1)).into(),
        ],
    )
    .await
}

/// Cost-spike circuit breaker: the environment's provider-CONFIRMED cost per segment over
/// the last hour exceeds `factor` × the configured estimate (with enough samples).
pub async fn cost_spike<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    settings: &ControlSettings,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    #[derive(sea_orm::FromQueryResult)]
    struct Row {
        samples: i64,
        cost: i64,
        segments: i64,
    }
    let r = db::one::<Row, _>(
        db,
        "SELECT count(*)::bigint AS samples, COALESCE(sum(confirmed_cost_minor), 0)::bigint AS cost, \
                COALESCE(sum(segments), 0)::bigint AS segments \
           FROM sms_usage_records \
          WHERE environment_id = $1 AND direction = 'OUTBOUND' AND status = 'CONFIRMED' AND updated_at >= $2",
        vec![environment_id.into(), (now - Duration::hours(1)).into()],
    )
    .await?;
    Ok(r.is_some_and(|r| {
        r.samples >= settings.cost_spike_min_samples
            && r.cost
                > r.segments
                    .saturating_mul(settings.segment_cost_minor)
                    .saturating_mul(settings.cost_spike_factor)
    }))
}

/// Provider anomaly signal: among the provider's recent finished attempts, too many were
/// rejected or ended UNKNOWN.
pub async fn provider_anomaly<C: ConnectionTrait>(
    db: &C,
    provider_id: &str,
    settings: &ControlSettings,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    #[derive(sea_orm::FromQueryResult)]
    struct Row {
        total: i64,
        bad: i64,
    }
    let r = db::one::<Row, _>(
        db,
        "SELECT count(*)::bigint AS total, \
                count(*) FILTER (WHERE status IN ('REJECTED', 'UNKNOWN'))::bigint AS bad \
           FROM sms_attempts WHERE provider_id = $1 AND status <> 'CLAIMED' AND started_at >= $2",
        vec![provider_id.into(), (now - Duration::minutes(15)).into()],
    )
    .await?;
    Ok(r.is_some_and(|r| {
        r.total >= settings.anomaly_min_attempts
            && r.bad * 100 >= r.total * settings.anomaly_percent
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_the_whole_message_only() {
        assert_eq!(keyword(" stop "), Some(OptKeyword::Stop));
        assert_eq!(keyword("Unsubscribe."), Some(OptKeyword::Stop));
        assert_eq!(keyword("START"), Some(OptKeyword::Start));
        assert_eq!(keyword("please stop calling"), None);
        assert_eq!(keyword("stopping by later"), None);
    }
}
