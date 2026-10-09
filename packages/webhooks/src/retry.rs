//! Delivery outcome classification and bounded exponential backoff with jitter.
//!
//! Retried: timeouts, connection failures, 408, 429 and 5xx. Not retried: other 4xx
//! (the receiver rejected the delivery), 3xx (redirects are never followed) and SSRF
//! policy refusals. After [`MAX_ATTEMPTS`] attempts a delivery is FAILED (dead letter).

use std::time::Duration;

use rand::Rng;

/// Attempts before a delivery is dead-lettered.
pub const MAX_ATTEMPTS: u32 = 8;
const BASE_SECONDS: u64 = 30;
const CAP_SECONDS: u64 = 6 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// Transient: try again later (bounded).
    Retry(&'static str),
    /// Terminal: the receiver (or policy) refused it.
    Permanent(&'static str),
}

/// Classification of an HTTP status answered by the receiver.
pub fn classify_status(status: u16) -> Outcome {
    match status {
        200..=299 => Outcome::Delivered,
        408 => Outcome::Retry("HTTP_TIMEOUT"),
        429 => Outcome::Retry("HTTP_RATE_LIMITED"),
        500..=599 => Outcome::Retry("HTTP_5XX"),
        300..=399 => Outcome::Permanent("HTTP_REDIRECT_REFUSED"),
        _ => Outcome::Permanent("HTTP_4XX"),
    }
}

/// Classification of a transport failure (no HTTP answer).
pub fn classify_error(error: &reqwest::Error) -> Outcome {
    if crate::endpoint::blocked_by_policy(error) {
        Outcome::Permanent("ENDPOINT_BLOCKED")
    } else if error.is_timeout() {
        Outcome::Retry("TIMEOUT")
    } else {
        Outcome::Retry("CONNECTION")
    }
}

/// Delay before attempt `next_attempt` (2 = the first retry): exponential from 30 s,
/// capped at 6 h, with full jitter in the upper half (never zero, never synchronized).
pub fn backoff(next_attempt: u32) -> Duration {
    let exp = BASE_SECONDS.saturating_mul(1u64 << next_attempt.saturating_sub(2).min(20));
    let ceiling = exp.min(CAP_SECONDS);
    let low = ceiling / 2;
    Duration::from_secs(rand::thread_rng().gen_range(low.max(1)..=ceiling.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_classified() {
        assert_eq!(classify_status(204), Outcome::Delivered);
        for s in [408, 429, 500, 502, 503] {
            assert!(matches!(classify_status(s), Outcome::Retry(_)), "{s}");
        }
        for s in [301, 302, 400, 401, 403, 404, 410, 422] {
            assert!(matches!(classify_status(s), Outcome::Permanent(_)), "{s}");
        }
    }

    #[test]
    fn backoff_is_bounded_and_jittered() {
        for attempt in 2..=40 {
            let d = backoff(attempt).as_secs();
            assert!((1..=CAP_SECONDS).contains(&d), "{attempt}: {d}");
        }
        let first = backoff(2).as_secs();
        assert!((15..=30).contains(&first));
        assert!(backoff(30).as_secs() >= CAP_SECONDS / 2);
    }
}
