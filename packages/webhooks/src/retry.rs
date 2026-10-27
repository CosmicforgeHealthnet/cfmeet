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

/// The receiver's `Retry-After` (RFC 9110 §10.2.3: delta-seconds or an IMF-fixdate HTTP
/// date) relative to `now_unix`, capped at the backoff ceiling. `None` when absent or
/// malformed (the regular backoff applies).
pub fn retry_after(value: Option<&str>, now_unix: i64) -> Option<Duration> {
    let v = value?.trim();
    let secs = if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        v.parse::<u64>().ok()?
    } else {
        let at = parse_imf_fixdate(v)?;
        u64::try_from(at.saturating_sub(now_unix)).unwrap_or(0)
    };
    Some(Duration::from_secs(secs.min(CAP_SECONDS)))
}

/// The delay before attempt `next_attempt`, honouring the receiver's `Retry-After` when it
/// asks for longer than the backoff (never shorter: a receiver cannot make us hammer it).
pub fn next_delay(next_attempt: u32, retry_after: Option<Duration>) -> Duration {
    let b = backoff(next_attempt);
    match retry_after {
        Some(r) if r > b => r,
        _ => b,
    }
}

/// `Sun, 06 Nov 1994 08:49:37 GMT` → unix seconds.
fn parse_imf_fixdate(v: &str) -> Option<i64> {
    let rest = v.split_once(", ")?.1;
    let parts: Vec<&str> = rest.split(' ').collect();
    if parts.len() != 5 || parts[4] != "GMT" {
        return None;
    }
    let day: i64 = parts[0].parse().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == parts[1])? as i64
        + 1;
    let year: i64 = parts[2].parse().ok()?;
    let hms: Vec<i64> = parts[3]
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    if hms.len() != 3 || !(1..=31).contains(&day) || hms[0] > 23 || hms[1] > 59 || hms[2] > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hms[0] * 3600 + hms[1] * 60 + hms[2])
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

    #[test]
    fn retry_after_is_honoured_when_longer_than_the_backoff() {
        let now = 784_111_777; // Sun, 06 Nov 1994 08:49:37 GMT
        assert_eq!(
            retry_after(Some("120"), now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            retry_after(Some("Sun, 06 Nov 1994 08:51:37 GMT"), now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            retry_after(Some("Sun, 06 Nov 1994 08:40:00 GMT"), now),
            Some(Duration::ZERO)
        );
        assert_eq!(
            retry_after(Some("999999999"), now),
            Some(Duration::from_secs(CAP_SECONDS))
        );
        for bad in [
            None,
            Some(""),
            Some("-5"),
            Some("soon"),
            Some("06 Nov 1994 08:49:37 GMT"),
        ] {
            assert_eq!(retry_after(bad, now), None, "{bad:?}");
        }
        assert!(next_delay(2, Some(Duration::from_secs(120))) == Duration::from_secs(120));
        let d = next_delay(2, Some(Duration::from_secs(1)));
        assert!(
            (15..=30).contains(&d.as_secs()),
            "never shorter than the backoff: {d:?}"
        );
        assert!((15..=30).contains(&next_delay(2, None).as_secs()));
    }
}
