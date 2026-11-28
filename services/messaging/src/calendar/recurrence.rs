//! The RRULE subset of events (ADR 0071 decision 6): `FREQ=DAILY|WEEKLY|MONTHLY`,
//! `INTERVAL`, `COUNT` or `UNTIL` (a local date, inclusive) and `BYDAY` (WEEKLY only).
//!
//! Occurrences are expanded in the event's IANA time zone: the local wall-clock time of
//! the first occurrence is kept (a weekly 10:00 stays 10:00 across daylight-saving
//! changes), a local time skipped by a change moves forward to the first valid minute, an
//! ambiguous one takes its earlier instant, and a MONTHLY day missing from a month is
//! skipped (RFC 5545). The span of every occurrence is the first one's LOCAL span, so an
//! all-day event stays midnight to midnight.

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// Upper bound of `INTERVAL`.
pub const MAX_INTERVAL: u32 = 99;
/// Upper bound of `COUNT`.
pub const MAX_COUNT: u32 = 730;
/// `UNTIL` is at most this many days after the start.
pub const MAX_UNTIL_DAYS: i64 = 5 * 366;
/// Candidate dates examined by one expansion at most (bounds every request).
pub const MAX_STEPS: usize = 40_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
}

impl Frequency {
    pub fn as_str(self) -> &'static str {
        match self {
            Frequency::Daily => "DAILY",
            Frequency::Weekly => "WEEKLY",
            Frequency::Monthly => "MONTHLY",
        }
    }
}

/// A recurrence as the API carries it (`until` is a local `YYYY-MM-DD` date).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Recurrence {
    pub frequency: Frequency,
    #[serde(default = "one")]
    pub interval: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_day: Vec<String>,
}

fn one() -> u32 {
    1
}

pub fn weekday_code(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

pub fn parse_weekday(code: &str) -> Option<Weekday> {
    Some(match code {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// Parses an IANA zone name (the bundled tz database).
pub fn zone(name: &str) -> Option<Tz> {
    if name.is_empty() || name.len() > 64 {
        return None;
    }
    name.parse::<Tz>().ok()
}

/// The UTC instant of a local wall-clock time: an ambiguous time takes its earlier
/// instant; a time skipped by a change moves forward (minute by minute, at most 3 hours).
pub fn to_utc(tz: Tz, local: NaiveDateTime) -> DateTime<Utc> {
    let mut t = local;
    for _ in 0..=180 {
        match tz.from_local_datetime(&t) {
            LocalResult::Single(x) => return x.with_timezone(&Utc),
            LocalResult::Ambiguous(a, _) => return a.with_timezone(&Utc),
            LocalResult::None => t += Duration::minutes(1),
        }
    }
    Utc.from_utc_datetime(&local)
}

/// One event's series: its first occurrence, zone and optional rule.
#[derive(Debug, Clone)]
pub struct Series {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub tz: Tz,
    pub rule: Option<Recurrence>,
}

/// Why a recurrence is refused (the message is shown to the user).
pub type Invalid = String;

impl Series {
    fn local_start(&self) -> NaiveDateTime {
        self.start.with_timezone(&self.tz).naive_local()
    }

    fn local_span(&self) -> Duration {
        self.end.with_timezone(&self.tz).naive_local() - self.local_start()
    }

    /// Checks the rule against the first occurrence.
    pub fn validate(&self) -> Result<(), Invalid> {
        let Some(r) = &self.rule else {
            return Ok(());
        };
        if r.interval == 0 || r.interval > MAX_INTERVAL {
            return Err(format!("interval must be 1 to {MAX_INTERVAL}"));
        }
        if r.count.is_some() && r.until.is_some() {
            return Err("A repeat ends after a number of times or on a date, not both".into());
        }
        if let Some(c) = r.count {
            if c == 0 || c > MAX_COUNT {
                return Err(format!("count must be 1 to {MAX_COUNT}"));
            }
        }
        let first = self.local_start().date();
        if let Some(u) = r.until {
            if u < first {
                return Err("The repeat can't end before the event starts".into());
            }
            if (u - first).num_days() > MAX_UNTIL_DAYS {
                return Err("The repeat can end at most 5 years after the start".into());
            }
        }
        if !r.by_day.is_empty() {
            if r.frequency != Frequency::Weekly {
                return Err("Repeat days are for weekly events only".into());
            }
            if r.by_day.len() > 7 {
                return Err("At most 7 repeat days".into());
            }
            let mut seen = Vec::new();
            for d in &r.by_day {
                let day = parse_weekday(d).ok_or_else(|| format!("Unknown repeat day {d}"))?;
                if seen.contains(&day) {
                    return Err("A repeat day is listed twice".into());
                }
                seen.push(day);
            }
            if !seen.contains(&first.weekday()) {
                return Err("The event must start on one of its repeat days".into());
            }
        }
        Ok(())
    }

    /// The local dates of the series, in order (the first one is the start's date).
    fn dates(&self) -> impl Iterator<Item = NaiveDate> + '_ {
        let first = self.local_start().date();
        let rule = self.rule.clone();
        let mut step: i64 = 0;
        let mut queue: Vec<NaiveDate> = Vec::new();
        let mut steps = 0usize;
        let mut done = false;
        std::iter::from_fn(move || {
            let Some(r) = &rule else {
                if done {
                    return None;
                }
                done = true;
                return Some(first);
            };
            let interval = i64::from(r.interval.max(1));
            loop {
                if let Some(d) = queue.pop() {
                    return Some(d);
                }
                if steps >= MAX_STEPS {
                    return None;
                }
                steps += 1;
                let k = step;
                step += 1;
                match r.frequency {
                    Frequency::Daily => {
                        return first.checked_add_signed(Duration::days(k * interval));
                    }
                    Frequency::Weekly => {
                        let monday = first
                            - Duration::days(i64::from(first.weekday().num_days_from_monday()));
                        let week = monday.checked_add_signed(Duration::days(k * interval * 7))?;
                        let mut days: Vec<u32> = if r.by_day.is_empty() {
                            vec![first.weekday().num_days_from_monday()]
                        } else {
                            r.by_day
                                .iter()
                                .filter_map(|c| parse_weekday(c))
                                .map(|d| d.num_days_from_monday())
                                .collect()
                        };
                        days.sort_unstable();
                        days.dedup();
                        // Pushed in reverse so `pop` yields them in order.
                        for d in days.into_iter().rev() {
                            let date = week + Duration::days(i64::from(d));
                            if date >= first {
                                queue.push(date);
                            }
                        }
                    }
                    Frequency::Monthly => {
                        let months = i64::from(first.month0()) + k * interval;
                        let year = i64::from(first.year()) + months.div_euclid(12);
                        let month = months.rem_euclid(12) as u32 + 1;
                        if year > 9999 {
                            return None;
                        }
                        if let Some(d) = NaiveDate::from_ymd_opt(year as i32, month, first.day()) {
                            return Some(d);
                        }
                    }
                }
            }
        })
    }

    /// Every occurrence `(start, end)` in order, the first being the stored start / end.
    /// Bounded by the rule's COUNT / UNTIL and by [`MAX_STEPS`].
    pub fn occurrences(&self) -> impl Iterator<Item = (DateTime<Utc>, DateTime<Utc>)> + '_ {
        let time = self.local_start().time();
        let span = self.local_span();
        let count = self.rule.as_ref().and_then(|r| r.count);
        let until = self.rule.as_ref().and_then(|r| r.until);
        let mut emitted: u32 = 0;
        let mut first = true;
        self.dates()
            .take_while(move |d| until.is_none_or(|u| *d <= u))
            .map_while(move |d| {
                if count.is_some_and(|c| emitted >= c) {
                    return None;
                }
                emitted += 1;
                if first {
                    first = false;
                    return Some((self.start, self.end));
                }
                let local = d.and_time(time);
                Some((to_utc(self.tz, local), to_utc(self.tz, local + span)))
            })
    }

    /// Whether the series ends (COUNT or UNTIL, or a single event).
    pub fn bounded(&self) -> bool {
        self.rule
            .as_ref()
            .is_none_or(|r| r.count.is_some() || r.until.is_some())
    }

    /// The end of the last occurrence (`None` for an open series).
    pub fn last_end(&self) -> Option<DateTime<Utc>> {
        if !self.bounded() {
            return None;
        }
        self.occurrences().map(|(_, e)| e).max()
    }

    /// The occurrences overlapping `[from, to)` in order (at most `limit`).
    pub fn between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: usize,
    ) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
        self.occurrences()
            .take_while(|(s, _)| *s < to)
            .filter(|(_, e)| *e > from)
            .take(limit)
            .collect()
    }

    /// The first occurrence not yet ended at `now`.
    pub fn next_after(&self, now: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        self.occurrences().find(|(_, e)| *e > now)
    }

    /// The first occurrence starting strictly after `t`.
    pub fn first_start_after(&self, t: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        self.occurrences().find(|(s, _)| *s > t)
    }

    /// The last `limit` occurrences ended at `now` (newest first).
    pub fn ended_before(
        &self,
        before: DateTime<Utc>,
        limit: usize,
    ) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
        let mut ring: std::collections::VecDeque<(DateTime<Utc>, DateTime<Utc>)> =
            std::collections::VecDeque::with_capacity(limit + 1);
        for o in self.occurrences().take_while(|(s, _)| *s < before) {
            if o.1 <= before {
                ring.push_back(o);
                if ring.len() > limit {
                    ring.pop_front();
                }
            }
        }
        ring.into_iter().rev().collect()
    }

    /// The RFC 5545 `RRULE` value (`UNTIL` as the UTC end of the local date).
    pub fn rrule(&self) -> Option<String> {
        let r = self.rule.as_ref()?;
        let mut out = format!("FREQ={}", r.frequency.as_str());
        if r.interval > 1 {
            out.push_str(&format!(";INTERVAL={}", r.interval));
        }
        if let Some(c) = r.count {
            out.push_str(&format!(";COUNT={c}"));
        }
        if let Some(u) = r.until {
            let last = u
                .succ_opt()
                .map(|n| to_utc(self.tz, n.and_hms_opt(0, 0, 0).unwrap_or_default()))
                .map(|t| t - Duration::seconds(1))
                .unwrap_or(self.end);
            out.push_str(&format!(";UNTIL={}", last.format("%Y%m%dT%H%M%SZ")));
        }
        if !r.by_day.is_empty() {
            let mut days: Vec<Weekday> = r.by_day.iter().filter_map(|c| parse_weekday(c)).collect();
            days.sort_by_key(|d| d.num_days_from_monday());
            let codes: Vec<&str> = days.into_iter().map(weekday_code).collect();
            out.push_str(&format!(";BYDAY={}", codes.join(",")));
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn series(start: &str, end: &str, tz: &str, rule: Option<Recurrence>) -> Series {
        Series {
            start: utc(start),
            end: utc(end),
            tz: zone(tz).unwrap(),
            rule,
        }
    }

    fn rule(f: Frequency, interval: u32, count: Option<u32>, by_day: &[&str]) -> Recurrence {
        Recurrence {
            frequency: f,
            interval,
            count,
            until: None,
            by_day: by_day.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_single_event_has_one_occurrence() {
        let s = series("2026-12-01T10:00:00Z", "2026-12-01T11:00:00Z", "UTC", None);
        let all: Vec<_> = s.occurrences().collect();
        assert_eq!(all, vec![(s.start, s.end)]);
        assert_eq!(s.last_end(), Some(s.end));
        assert!(s.rrule().is_none());
    }

    #[test]
    fn weekly_keeps_the_local_time_across_daylight_saving() {
        // 10:00 London: GMT in March before the 29th, BST after.
        let s = series(
            "2026-03-23T10:00:00Z",
            "2026-03-23T11:00:00Z",
            "Europe/London",
            Some(rule(Frequency::Weekly, 1, Some(3), &[])),
        );
        let all: Vec<_> = s.occurrences().collect();
        assert_eq!(all.len(), 3);
        assert_eq!(all[1].0, utc("2026-03-30T09:00:00Z"));
        assert_eq!(all[1].1, utc("2026-03-30T10:00:00Z"));
        assert_eq!(all[2].0, utc("2026-04-06T09:00:00Z"));
        assert_eq!(s.last_end(), Some(utc("2026-04-06T10:00:00Z")));
        assert_eq!(s.rrule().unwrap(), "FREQ=WEEKLY;COUNT=3");
    }

    #[test]
    fn weekly_by_day_in_order_with_interval() {
        // Monday 2026-11-30 09:00 UTC, every 2 weeks on MO, WE.
        let s = series(
            "2026-11-30T09:00:00Z",
            "2026-11-30T09:30:00Z",
            "UTC",
            Some(rule(Frequency::Weekly, 2, Some(5), &["WE", "MO"])),
        );
        s.validate().unwrap();
        let starts: Vec<_> = s.occurrences().map(|o| o.0).collect();
        assert_eq!(
            starts,
            vec![
                utc("2026-11-30T09:00:00Z"),
                utc("2026-12-02T09:00:00Z"),
                utc("2026-12-14T09:00:00Z"),
                utc("2026-12-16T09:00:00Z"),
                utc("2026-12-28T09:00:00Z"),
            ]
        );
        assert_eq!(
            s.rrule().unwrap(),
            "FREQ=WEEKLY;INTERVAL=2;COUNT=5;BYDAY=MO,WE"
        );
    }

    #[test]
    fn by_day_must_include_the_start_and_is_weekly_only() {
        let s = series(
            "2026-11-30T09:00:00Z",
            "2026-11-30T10:00:00Z",
            "UTC",
            Some(rule(Frequency::Weekly, 1, None, &["TU"])),
        );
        assert!(s.validate().is_err());
        let s = series(
            "2026-11-30T09:00:00Z",
            "2026-11-30T10:00:00Z",
            "UTC",
            Some(rule(Frequency::Daily, 1, None, &["MO"])),
        );
        assert!(s.validate().is_err());
    }

    #[test]
    fn monthly_skips_missing_days() {
        let s = series(
            "2027-01-31T12:00:00Z",
            "2027-01-31T13:00:00Z",
            "UTC",
            Some(rule(Frequency::Monthly, 1, Some(3), &[])),
        );
        let starts: Vec<_> = s.occurrences().map(|o| o.0).collect();
        assert_eq!(
            starts,
            vec![
                utc("2027-01-31T12:00:00Z"),
                utc("2027-03-31T12:00:00Z"),
                utc("2027-05-31T12:00:00Z"),
            ]
        );
    }

    #[test]
    fn until_is_an_inclusive_local_date() {
        let mut r = rule(Frequency::Daily, 1, None, &[]);
        r.until = Some(NaiveDate::from_ymd_opt(2026, 12, 3).unwrap());
        // 23:30 New York is 04:30 UTC the next day.
        let s = series(
            "2026-12-02T04:30:00Z",
            "2026-12-02T05:00:00Z",
            "America/New_York",
            Some(r),
        );
        s.validate().unwrap();
        let all: Vec<_> = s.occurrences().collect();
        assert_eq!(all.len(), 3, "Dec 1, 2 and 3 at 23:30 local");
        assert_eq!(all[1].0, utc("2026-12-03T04:30:00Z"));
        assert_eq!(all[2].0, utc("2026-12-04T04:30:00Z"));
        assert!(s.rrule().unwrap().contains("UNTIL=20261204T045959Z"));
    }

    #[test]
    fn a_skipped_local_time_moves_forward() {
        // 02:30 New York does not exist on 2027-03-14.
        let s = series(
            "2027-03-13T07:30:00Z",
            "2027-03-13T08:00:00Z",
            "America/New_York",
            Some(rule(Frequency::Daily, 1, Some(2), &[])),
        );
        let all: Vec<_> = s.occurrences().collect();
        assert_eq!(all[1].0, utc("2027-03-14T07:00:00Z"), "03:00 EDT");
    }

    #[test]
    fn all_day_stays_midnight_to_midnight() {
        // Sat 2026-10-24 all day in London (BST), weekly, crosses the Oct 25 change.
        let s = series(
            "2026-10-23T23:00:00Z",
            "2026-10-24T23:00:00Z",
            "Europe/London",
            Some(rule(Frequency::Weekly, 1, Some(2), &[])),
        );
        let all: Vec<_> = s.occurrences().collect();
        assert_eq!(
            all[1],
            (utc("2026-10-31T00:00:00Z"), utc("2026-11-01T00:00:00Z"))
        );
    }

    #[test]
    fn windows_and_open_series() {
        let s = series(
            "2026-01-01T08:00:00Z",
            "2026-01-01T09:00:00Z",
            "UTC",
            Some(rule(Frequency::Daily, 1, None, &[])),
        );
        assert!(!s.bounded());
        assert_eq!(s.last_end(), None);
        let w = s.between(utc("2026-02-01T08:30:00Z"), utc("2026-02-03T00:00:00Z"), 10);
        assert_eq!(
            w.iter().map(|o| o.0).collect::<Vec<_>>(),
            vec![utc("2026-02-01T08:00:00Z"), utc("2026-02-02T08:00:00Z")]
        );
        assert_eq!(
            s.next_after(utc("2026-02-01T09:00:00Z")).unwrap().0,
            utc("2026-02-02T08:00:00Z")
        );
        let past = s.ended_before(utc("2026-01-03T08:30:00Z"), 5);
        assert_eq!(
            past.iter().map(|o| o.0).collect::<Vec<_>>(),
            vec![utc("2026-01-02T08:00:00Z"), utc("2026-01-01T08:00:00Z")]
        );
        assert_eq!(
            s.first_start_after(utc("2026-01-01T08:00:00Z")).unwrap().0,
            utc("2026-01-02T08:00:00Z")
        );
    }

    #[test]
    fn rule_bounds() {
        let base = |r| {
            series(
                "2026-12-01T10:00:00Z",
                "2026-12-01T11:00:00Z",
                "UTC",
                Some(r),
            )
        };
        assert!(base(rule(Frequency::Daily, 0, None, &[]))
            .validate()
            .is_err());
        assert!(base(rule(Frequency::Daily, 100, None, &[]))
            .validate()
            .is_err());
        assert!(base(rule(Frequency::Daily, 1, Some(731), &[]))
            .validate()
            .is_err());
        let mut r = rule(Frequency::Daily, 1, Some(3), &[]);
        r.until = Some(NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());
        assert!(base(r).validate().is_err());
        let mut r = rule(Frequency::Daily, 1, None, &[]);
        r.until = Some(NaiveDate::from_ymd_opt(2026, 11, 30).unwrap());
        assert!(base(r).validate().is_err());
        assert!(zone("Mars/Base").is_none());
        assert!(zone("Europe/Paris").is_some());
    }
}
