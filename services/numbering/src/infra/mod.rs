//! Reliability foundations (Part IV §C4) that every later Numbering workflow reuses:
//!
//! * [`outbox`] — events committed atomically with the state they describe, published by
//!   a leased, multi-instance-safe publisher with bounded backoff and dead-lettering;
//! * [`jobs`] — a generic leased job queue: claim tokens, lease expiry recovery, bounded
//!   retry, DEAD_LETTER for exhausted work and UNKNOWN_OUTCOME parking for work that may
//!   have reached an external provider (never blindly repeated);
//! * [`idempotency`] — scoped idempotency keys with request fingerprints;
//! * [`platform_events`] — the Developer Platform as the outbox's consumer: public number
//!   lifecycle events of customer scopes (Part VI §53).

pub mod idempotency;
pub mod jobs;
pub mod outbox;
pub mod platform_events;
pub mod usage_export;

use chrono::Duration;

/// Bounded exponential backoff: `base · 2^(attempt-1)`, capped at `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub base: Duration,
    pub max: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: Duration::seconds(1),
            max: Duration::minutes(5),
        }
    }
}

impl RetryPolicy {
    /// The delay before attempt `attempt + 1`, after `attempt` (≥ 1) failed attempts.
    pub fn delay(&self, attempt: i32) -> Duration {
        let exponent = attempt.clamp(1, 32) - 1;
        let factor = 1_i64.checked_shl(exponent as u32).unwrap_or(i64::MAX);
        let millis = self.base.num_milliseconds().saturating_mul(factor);
        Duration::milliseconds(millis.min(self.max.num_milliseconds()))
    }
}

/// Bounded, safe text of a failure (never a payload, secret or provider response).
pub fn bounded_error(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_is_capped() {
        let p = RetryPolicy {
            base: Duration::milliseconds(100),
            max: Duration::seconds(1),
        };
        assert_eq!(p.delay(1), Duration::milliseconds(100));
        assert_eq!(p.delay(2), Duration::milliseconds(200));
        assert_eq!(p.delay(4), Duration::milliseconds(800));
        assert_eq!(p.delay(5), Duration::seconds(1));
        assert_eq!(p.delay(64), Duration::seconds(1));
        assert_eq!(p.delay(0), Duration::milliseconds(100));
    }

    #[test]
    fn errors_are_bounded_and_single_line() {
        let long = format!("line\n{}", "x".repeat(500));
        let b = bounded_error(&long);
        assert_eq!(b.chars().count(), 200);
        assert!(!b.contains('\n'));
    }
}
