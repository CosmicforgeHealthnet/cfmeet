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
//!
//! The full allocation-fraud / investigation engine is a later workstream; these are the
//! transport's own guards.

use chrono::{DateTime, Duration, DurationRound, Utc};
use sea_orm::{ConnectionTrait, DbErr};
use uuid::Uuid;

use super::events::EventScope;
use crate::db;

pub const RATE_LIMITED: &str = "RATE_LIMITED";
pub const DESTINATION_RATE_LIMITED: &str = "DESTINATION_RATE_LIMITED";
pub const DESTINATION_NOT_ALLOWED: &str = "DESTINATION_NOT_ALLOWED";
pub const RECIPIENT_OPTED_OUT: &str = "RECIPIENT_OPTED_OUT";
pub const SPEND_LIMIT_EXCEEDED: &str = "SPEND_LIMIT_EXCEEDED";
pub const ABUSE_SUSPECTED: &str = "ABUSE_SUSPECTED";
pub const SENDER_NOT_AUTHORIZED: &str = "SENDER_NOT_AUTHORIZED";

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
