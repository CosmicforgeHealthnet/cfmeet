//! RFC 5545 iCalendar rendering for the first-party calendar (Blueprint Section 10,
//! MTG-007). Pure functions, no I/O.
//!
//! * Lines end with CRLF and are folded at 75 octets (never inside a UTF-8 sequence).
//! * Text values are escaped (`\\`, `\;`, `\,`, `\n`).
//! * Times are UTC (`…Z`) for a UTC calendar; otherwise `TZID=<IANA zone>` local times
//!   with a generated `VTIMEZONE` whose observances are the zone's real transitions over
//!   the exported window (no client-side guessing of the rules).
//! * The renderer never sees passcodes, invitation tokens or provider identifiers: the
//!   caller passes only the fields below.

use chrono::{DateTime, Duration, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::{OffsetComponents, OffsetName, Tz};

pub const PRODID: &str = "-//CF Meet//Meetings v2//EN";
/// Upper bound of the VTIMEZONE window, so a far-future series cannot make the
/// transition scan unbounded.
const MAX_TIMEZONE_WINDOW_DAYS: i64 = 366 * 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IcsStatus {
    Confirmed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct IcsEvent {
    pub uid: String,
    pub sequence: i32,
    pub summary: String,
    pub description: Option<String>,
    pub url: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub status: IcsStatus,
    /// RRULE body (series master only), UNTIL already in UTC.
    pub rrule: Option<String>,
    pub exdates: Vec<DateTime<Utc>>,
    /// Original start of the overridden instance (series override only).
    pub recurrence_id: Option<DateTime<Utc>>,
    pub created: Option<DateTime<Utc>>,
    pub last_modified: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct IcsCalendar {
    /// The meeting timezone; `None` / UTC renders UTC times without a VTIMEZONE.
    pub timezone: Option<Tz>,
    pub name: Option<String>,
    pub events: Vec<IcsEvent>,
    /// End of the window the VTIMEZONE must cover (e.g. a series' last occurrence).
    pub timezone_until: Option<DateTime<Utc>>,
}

/// Escape a TEXT value (RFC 5545 §3.3.11).
pub fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Fold one content line at 75 octets (RFC 5545 §3.1).
fn fold(line: &str, out: &mut String) {
    let mut count = 0usize;
    for c in line.chars() {
        let len = c.len_utf8();
        if count + len > 75 {
            out.push_str("\r\n ");
            count = 1;
        }
        out.push(c);
        count += len;
    }
    out.push_str("\r\n");
}

fn utc_stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn effective_zone(tz: Option<Tz>) -> Option<Tz> {
    tz.filter(|z| *z != Tz::UTC && *z != Tz::Etc__UTC && z.name() != "UTC")
}

/// `NAME;TZID=Zone:local` or `NAME:utcZ`.
fn time_property(name: &str, t: DateTime<Utc>, tz: Option<Tz>) -> String {
    match effective_zone(tz) {
        Some(z) => format!(
            "{name};TZID={}:{}",
            z.name(),
            t.with_timezone(&z).format("%Y%m%dT%H%M%S")
        ),
        None => format!("{name}:{}", utc_stamp(t)),
    }
}

fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let s = seconds.abs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if sec == 0 {
        format!("{sign}{h:02}{m:02}")
    } else {
        format!("{sign}{h:02}{m:02}{sec:02}")
    }
}

/// One observance onset: the UTC instant, the offsets around it and its name.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub at: DateTime<Utc>,
    pub from: i32,
    pub to: i32,
    pub daylight: bool,
    pub name: Option<String>,
}

fn observance(tz: Tz, t: DateTime<Utc>) -> (i32, bool, Option<String>) {
    let o = tz.offset_from_utc_datetime(&t.naive_utc());
    (
        o.fix().local_minus_utc(),
        o.dst_offset().num_seconds() != 0,
        o.abbreviation().map(str::to_string),
    )
}

/// The zone's offset transitions in `[from, to)` (day scan + bisection to the second).
pub fn transitions(tz: Tz, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Transition> {
    let to = to.min(from + Duration::days(MAX_TIMEZONE_WINDOW_DAYS));
    let mut out = vec![];
    let mut cursor = from;
    let mut current = observance(tz, cursor);
    while cursor < to {
        let next = (cursor + Duration::days(1)).min(to);
        let probe = observance(tz, next);
        if probe.0 != current.0 || probe.1 != current.1 {
            // Bisect on whole seconds: onsets are always at a whole second.
            let (mut lo, mut hi) = (cursor.timestamp(), next.timestamp());
            let at = |secs: i64| DateTime::<Utc>::from_timestamp(secs, 0).unwrap_or(next);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                let m = observance(tz, at(mid));
                if m.0 == current.0 && m.1 == current.1 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let hi = at(hi);
            let after = observance(tz, hi);
            out.push(Transition {
                at: hi,
                from: current.0,
                to: after.0,
                daylight: after.1,
                name: after.2.clone(),
            });
            current = after;
        }
        cursor = next;
    }
    out
}

fn vtimezone(tz: Tz, from: DateTime<Utc>, to: DateTime<Utc>, out: &mut String) {
    fold("BEGIN:VTIMEZONE", out);
    fold(&format!("TZID:{}", tz.name()), out);
    let (offset, daylight, name) = observance(tz, from);
    let mut components = vec![(from, offset, offset, daylight, name)];
    for t in transitions(tz, from, to) {
        components.push((t.at, t.from, t.to, t.daylight, t.name));
    }
    for (at, from_offset, to_offset, daylight, name) in components {
        let kind = if daylight { "DAYLIGHT" } else { "STANDARD" };
        fold(&format!("BEGIN:{kind}"), out);
        // DTSTART is the onset in the local time of the offset in force before it.
        let local: NaiveDateTime = at.naive_utc() + Duration::seconds(from_offset as i64);
        fold(&format!("DTSTART:{}", local.format("%Y%m%dT%H%M%S")), out);
        fold(&format!("TZOFFSETFROM:{}", format_offset(from_offset)), out);
        fold(&format!("TZOFFSETTO:{}", format_offset(to_offset)), out);
        if let Some(n) = name.filter(|n| !n.is_empty() && !n.starts_with(['+', '-'])) {
            fold(&format!("TZNAME:{}", escape_text(&n)), out);
        }
        fold(&format!("END:{kind}"), out);
    }
    fold("END:VTIMEZONE", out);
}

/// Render a complete `VCALENDAR` (METHOD:PUBLISH).
pub fn render(cal: &IcsCalendar, now: DateTime<Utc>) -> String {
    let mut out = String::new();
    fold("BEGIN:VCALENDAR", &mut out);
    fold("VERSION:2.0", &mut out);
    fold(&format!("PRODID:{PRODID}"), &mut out);
    fold("CALSCALE:GREGORIAN", &mut out);
    fold("METHOD:PUBLISH", &mut out);
    if let Some(name) = &cal.name {
        fold(&format!("X-WR-CALNAME:{}", escape_text(name)), &mut out);
    }
    let zone = effective_zone(cal.timezone);
    if let Some(z) = zone {
        fold(&format!("X-WR-TIMEZONE:{}", z.name()), &mut out);
        let earliest = cal
            .events
            .iter()
            .flat_map(|e| std::iter::once(e.start).chain(e.recurrence_id))
            .chain(cal.events.iter().flat_map(|e| e.exdates.iter().copied()))
            .min()
            .unwrap_or(now);
        let latest = cal
            .events
            .iter()
            .map(|e| e.end)
            .chain(cal.timezone_until)
            .max()
            .unwrap_or(now)
            .max(now);
        vtimezone(
            z,
            earliest - Duration::days(1),
            latest + Duration::days(1),
            &mut out,
        );
    }
    for e in &cal.events {
        fold("BEGIN:VEVENT", &mut out);
        fold(&format!("UID:{}", escape_text(&e.uid)), &mut out);
        fold(&format!("DTSTAMP:{}", utc_stamp(now)), &mut out);
        if let Some(rid) = e.recurrence_id {
            fold(&time_property("RECURRENCE-ID", rid, zone), &mut out);
        }
        fold(&time_property("DTSTART", e.start, zone), &mut out);
        fold(&time_property("DTEND", e.end, zone), &mut out);
        if let Some(rule) = &e.rrule {
            fold(&format!("RRULE:{rule}"), &mut out);
        }
        for ex in &e.exdates {
            fold(&time_property("EXDATE", *ex, zone), &mut out);
        }
        fold(&format!("SEQUENCE:{}", e.sequence.max(0)), &mut out);
        fold(&format!("SUMMARY:{}", escape_text(&e.summary)), &mut out);
        if let Some(d) = &e.description {
            fold(&format!("DESCRIPTION:{}", escape_text(d)), &mut out);
        }
        if let Some(u) = &e.url {
            fold(&format!("URL:{u}"), &mut out);
        }
        fold(
            match e.status {
                IcsStatus::Confirmed => "STATUS:CONFIRMED",
                IcsStatus::Cancelled => "STATUS:CANCELLED",
            },
            &mut out,
        );
        fold("TRANSP:OPAQUE", &mut out);
        if let Some(c) = e.created {
            fold(&format!("CREATED:{}", utc_stamp(c)), &mut out);
        }
        fold(
            &format!("LAST-MODIFIED:{}", utc_stamp(e.last_modified)),
            &mut out,
        );
        fold("END:VEVENT", &mut out);
    }
    fold("END:VCALENDAR", &mut out);
    out
}

/// Rewrite an RRULE body so `UNTIL` is UTC (required with a `TZID` DTSTART).
pub fn rrule_with_utc_until(body: &str, until: Option<DateTime<Utc>>) -> String {
    body.split(';')
        .filter(|p| !p.is_empty())
        .map(|p| match (p.strip_prefix("UNTIL="), until) {
            (Some(_), Some(u)) => format!("UNTIL={}", utc_stamp(u)),
            _ => p.to_string(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unfold(s: &str) -> String {
        s.replace("\r\n ", "")
    }

    fn event(start: DateTime<Utc>) -> IcsEvent {
        IcsEvent {
            uid: "conference-1@cfmeet.com".into(),
            sequence: 2,
            summary: "Board; review, Q4\\notes".into(),
            description: Some("Line one\nLine two".into()),
            url: Some("https://app.example/abc-defg-hij".into()),
            start,
            end: start + Duration::hours(1),
            status: IcsStatus::Confirmed,
            rrule: None,
            exdates: vec![],
            recurrence_id: None,
            created: None,
            last_modified: start,
        }
    }

    #[test]
    fn utc_calendar_has_crlf_escaping_and_no_timezone() {
        let start = Utc.with_ymd_and_hms(2026, 11, 20, 14, 0, 0).unwrap();
        let ics = render(
            &IcsCalendar {
                timezone: Some(Tz::UTC),
                name: None,
                events: vec![event(start)],
                timezone_until: None,
            },
            start,
        );
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        assert!(
            !ics.replace("\r\n", "").contains('\n'),
            "only CRLF line ends"
        );
        assert!(!ics.contains("VTIMEZONE"));
        assert!(ics.contains("DTSTART:20261120T140000Z\r\n"));
        assert!(ics.contains("DTEND:20261120T150000Z\r\n"));
        assert!(ics.contains("SUMMARY:Board\\; review\\, Q4\\\\notes\r\n"));
        assert!(ics.contains("DESCRIPTION:Line one\\nLine two\r\n"));
        assert!(ics.contains("SEQUENCE:2\r\n"));
        for line in ics.split("\r\n") {
            assert!(line.len() <= 75, "folded: {line}");
        }
    }

    #[test]
    fn long_lines_fold_without_splitting_utf8() {
        let start = Utc.with_ymd_and_hms(2026, 11, 20, 14, 0, 0).unwrap();
        let mut e = event(start);
        e.summary = "é".repeat(100);
        let ics = render(
            &IcsCalendar {
                timezone: None,
                name: None,
                events: vec![e],
                timezone_until: None,
            },
            start,
        );
        assert!(ics.split("\r\n").all(|l| l.len() <= 75));
        assert!(unfold(&ics).contains(&format!("SUMMARY:{}", "é".repeat(100))));
    }

    #[test]
    fn zoned_series_has_tzid_vtimezone_rrule_and_exdate() {
        let tz: Tz = "America/New_York".parse().unwrap();
        let start = tz
            .with_ymd_and_hms(2026, 10, 26, 9, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut e = event(start);
        e.rrule = Some("FREQ=WEEKLY;COUNT=6".into());
        // The next Monday 09:00 is EST (after the 1 Nov fall-back): 14:00Z.
        e.exdates = vec![tz
            .with_ymd_and_hms(2026, 11, 2, 9, 0, 0)
            .unwrap()
            .with_timezone(&Utc)];
        let ics = unfold(&render(
            &IcsCalendar {
                timezone: Some(tz),
                name: Some("Weekly".into()),
                events: vec![e],
                timezone_until: Some(start + Duration::weeks(6)),
            },
            start,
        ));
        assert!(ics.contains("TZID:America/New_York\r\n"));
        assert!(ics.contains("DTSTART;TZID=America/New_York:20261026T090000\r\n"));
        assert!(ics.contains("EXDATE;TZID=America/New_York:20261102T090000\r\n"));
        assert!(ics.contains("RRULE:FREQ=WEEKLY;COUNT=6\r\n"));
        // The 1 Nov 2026 fall-back onset: 02:00 EDT (-0400) → EST (-0500).
        assert!(ics.contains(
            "BEGIN:STANDARD\r\nDTSTART:20261101T020000\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nTZNAME:EST\r\nEND:STANDARD"
        ));
    }

    #[test]
    fn transitions_are_exact_and_zones_without_dst_have_none() {
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let from = Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap();
        let t = transitions(tz, from, from + Duration::days(365));
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!(t[0].at, Utc.with_ymd_and_hms(2027, 3, 28, 1, 0, 0).unwrap());
        assert_eq!((t[0].from, t[0].to, t[0].daylight), (3600, 7200, true));
        assert_eq!(
            t[1].at,
            Utc.with_ymd_and_hms(2027, 10, 31, 1, 0, 0).unwrap()
        );
        let lagos: Tz = "Africa/Lagos".parse().unwrap();
        assert!(transitions(lagos, from, from + Duration::days(365)).is_empty());
    }

    #[test]
    fn until_is_rewritten_to_utc() {
        let u = Utc.with_ymd_and_hms(2026, 12, 31, 14, 0, 0).unwrap();
        assert_eq!(
            rrule_with_utc_until("FREQ=DAILY;UNTIL=20261231T090000", Some(u)),
            "FREQ=DAILY;UNTIL=20261231T140000Z"
        );
        assert_eq!(
            rrule_with_utc_until("FREQ=DAILY;COUNT=3", None),
            "FREQ=DAILY;COUNT=3"
        );
    }
}
