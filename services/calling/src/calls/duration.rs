//! Participation time from session connection intervals (Part C §19).
//!
//! Participation is never computed from `participant.created_at → ended_at` or from a
//! single join / leave pair. It is computed from the persisted connection intervals:
//!
//! * a **session's** connected time is the sum of its intervals (they never overlap);
//! * a **participant's** logical participation is the length of the *union* of the
//!   intervals of all its sessions, so two devices connected at the same time are not
//!   counted twice;
//! * an open interval counts up to the `as_of` instant; anything after `as_of` is ignored.

use chrono::{DateTime, Duration, Utc};

/// One connected period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionSpan {
    pub connected_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
}

impl ConnectionSpan {
    /// The span clipped to `[connected_at, min(disconnected_at, as_of)]`, if non-empty.
    fn clipped(&self, as_of: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let end = self.disconnected_at.map_or(as_of, |d| d.min(as_of));
        (end > self.connected_at).then_some((self.connected_at, end))
    }
}

/// Sum of the spans (for one session, whose intervals never overlap).
pub fn total_duration(spans: &[ConnectionSpan], as_of: DateTime<Utc>) -> Duration {
    spans
        .iter()
        .filter_map(|s| s.clipped(as_of))
        .fold(Duration::zero(), |acc, (start, end)| acc + (end - start))
}

/// Length of the union of the spans (for one participant across all its sessions).
pub fn union_duration(spans: &[ConnectionSpan], as_of: DateTime<Utc>) -> Duration {
    let mut clipped: Vec<_> = spans.iter().filter_map(|s| s.clipped(as_of)).collect();
    clipped.sort();
    let mut total = Duration::zero();
    let mut current: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    for (start, end) in clipped {
        match current {
            Some((cs, ce)) if start <= ce => current = Some((cs, ce.max(end))),
            Some((cs, ce)) => {
                total += ce - cs;
                current = Some((start, end));
            }
            None => current = Some((start, end)),
        }
    }
    if let Some((cs, ce)) = current {
        total += ce - cs;
    }
    total
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn t(s: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2031, 1, 1, 0, 0, 0).unwrap() + Duration::seconds(s)
    }

    fn span(a: i64, b: Option<i64>) -> ConnectionSpan {
        ConnectionSpan {
            connected_at: t(a),
            disconnected_at: b.map(t),
        }
    }

    #[test]
    fn single_interval() {
        assert_eq!(
            total_duration(&[span(10, Some(70))], t(1000)),
            Duration::seconds(60)
        );
        assert_eq!(
            union_duration(&[span(10, Some(70))], t(1000)),
            Duration::seconds(60)
        );
    }

    #[test]
    fn reconnect_sums_the_intervals_and_skips_the_gap() {
        let s = [span(0, Some(30)), span(50, Some(80))];
        assert_eq!(total_duration(&s, t(1000)), Duration::seconds(60));
        assert_eq!(union_duration(&s, t(1000)), Duration::seconds(60));
    }

    #[test]
    fn sequential_sessions_add_up() {
        let s = [span(0, Some(20)), span(20, Some(50)), span(60, Some(70))];
        assert_eq!(union_duration(&s, t(1000)), Duration::seconds(60));
    }

    #[test]
    fn overlapping_devices_are_not_double_counted() {
        // Phone 0–60, laptop 30–90, tablet 40–50 (inside both): 90 s of participation.
        let s = [span(0, Some(60)), span(30, Some(90)), span(40, Some(50))];
        assert_eq!(union_duration(&s, t(1000)), Duration::seconds(90));
        assert_eq!(total_duration(&s, t(1000)), Duration::seconds(130));
    }

    #[test]
    fn open_intervals_count_until_the_query_instant() {
        let s = [span(0, Some(10)), span(20, None)];
        assert_eq!(total_duration(&s, t(50)), Duration::seconds(40));
        assert_eq!(union_duration(&s, t(50)), Duration::seconds(40));
        // Nothing after `as_of` counts, and intervals starting later contribute nothing.
        assert_eq!(union_duration(&[span(100, None)], t(50)), Duration::zero());
        assert_eq!(
            total_duration(&[span(0, Some(100))], t(40)),
            Duration::seconds(40)
        );
    }

    #[test]
    fn empty_and_zero_length_intervals() {
        assert_eq!(union_duration(&[], t(10)), Duration::zero());
        assert_eq!(union_duration(&[span(5, Some(5))], t(10)), Duration::zero());
    }
}
