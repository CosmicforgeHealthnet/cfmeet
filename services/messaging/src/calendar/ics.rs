//! RFC 5545 export of events (ADR 0071 decision 9): CRLF line endings, lines folded at 75
//! octets, escaped text, `TZID` local times (`Z` for UTC), `VALUE=DATE` for all-day events,
//! `RRULE`, `SEQUENCE`, `STATUS:CANCELLED`, the meeting link. No names, e-mail addresses or
//! principal ids are exported.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use uuid::Uuid;

/// What one `VEVENT` shows.
#[derive(Debug, Clone)]
pub struct IcsEvent {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub meeting_url: Option<String>,
    pub meeting_id: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub tz: Tz,
    pub all_day: bool,
    pub rrule: Option<String>,
    pub cancelled: bool,
    pub sequence: i32,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

/// Escapes a TEXT value (RFC 5545 §3.3.11).
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
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

/// Folds one content line at 75 octets (never inside a UTF-8 character) and ends it
/// with CRLF.
pub fn fold(line: &str, out: &mut String) {
    let mut used = 0;
    for c in line.chars() {
        let n = c.len_utf8();
        if used + n > 75 {
            out.push_str("\r\n ");
            used = 1;
        }
        out.push(c);
        used += n;
    }
    out.push_str("\r\n");
}

fn stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn when(name: &str, t: DateTime<Utc>, e: &IcsEvent) -> String {
    let local = t.with_timezone(&e.tz);
    if e.all_day {
        return format!("{name};VALUE=DATE:{}", local.format("%Y%m%d"));
    }
    if e.tz == chrono_tz::UTC {
        return format!("{name}:{}", stamp(t));
    }
    format!(
        "{name};TZID={}:{}",
        e.tz.name(),
        local.format("%Y%m%dT%H%M%S")
    )
}

/// A complete `VCALENDAR` of `events`.
pub fn calendar(name: &str, events: &[IcsEvent], now: DateTime<Utc>) -> String {
    let mut out = String::new();
    for l in [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//CF Meet//Events//EN",
        "CALSCALE:GREGORIAN",
        "METHOD:PUBLISH",
    ] {
        fold(l, &mut out);
    }
    fold(&format!("X-WR-CALNAME:{}", escape(name)), &mut out);
    for e in events {
        fold("BEGIN:VEVENT", &mut out);
        fold(&format!("UID:{}@events.cfmeet.com", e.id), &mut out);
        fold(&format!("DTSTAMP:{}", stamp(now)), &mut out);
        fold(&format!("CREATED:{}", stamp(e.created)), &mut out);
        fold(&format!("LAST-MODIFIED:{}", stamp(e.updated)), &mut out);
        fold(&format!("SEQUENCE:{}", e.sequence.max(0)), &mut out);
        fold(&when("DTSTART", e.start, e), &mut out);
        fold(&when("DTEND", e.end, e), &mut out);
        if let Some(r) = &e.rrule {
            fold(&format!("RRULE:{r}"), &mut out);
        }
        fold(&format!("SUMMARY:{}", escape(&e.title)), &mut out);
        if let Some(d) = &e.description {
            fold(&format!("DESCRIPTION:{}", escape(d)), &mut out);
        }
        if let Some(l) = &e.location {
            fold(&format!("LOCATION:{}", escape(l)), &mut out);
        }
        if let Some(u) = &e.meeting_url {
            fold(&format!("URL:{}", u.replace(['\r', '\n'], "")), &mut out);
        }
        if let Some(m) = &e.meeting_id {
            fold(&format!("X-CFMEET-MEETING-ID:{}", escape(m)), &mut out);
        }
        fold(
            if e.cancelled {
                "STATUS:CANCELLED"
            } else {
                "STATUS:CONFIRMED"
            },
            &mut out,
        );
        fold("END:VEVENT", &mut out);
    }
    fold("END:VCALENDAR", &mut out);
    out
}

/// A safe file name: the title's letters / digits, else `event`.
pub fn filename(title: &str) -> String {
    let base: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base: String = base.chars().take(60).collect();
    if base.is_empty() {
        "event.ics".into()
    } else {
        format!("{base}.ics")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn event() -> IcsEvent {
        IcsEvent {
            id: Uuid::nil(),
            title: "Team, sync; weekly\\".into(),
            description: Some("Line one\nLine two".into()),
            location: None,
            meeting_url: Some("https://cfmeet.com/abc-defg-hij".into()),
            meeting_id: Some("abc".into()),
            start: utc("2026-12-01T09:00:00Z"),
            end: utc("2026-12-01T10:00:00Z"),
            tz: "Europe/Paris".parse().unwrap(),
            all_day: false,
            rrule: Some("FREQ=WEEKLY;COUNT=3".into()),
            cancelled: false,
            sequence: 2,
            created: utc("2026-11-28T09:00:00Z"),
            updated: utc("2026-11-28T10:00:00Z"),
        }
    }

    #[test]
    fn a_calendar_is_rfc_5545_shaped() {
        let ics = calendar("CF Meet", &[event()], utc("2026-11-28T12:00:00Z"));
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        assert!(ics.contains("DTSTART;TZID=Europe/Paris:20261201T100000\r\n"));
        assert!(ics.contains("SUMMARY:Team\\, sync\\; weekly\\\\\r\n"));
        assert!(ics.contains("DESCRIPTION:Line one\\nLine two\r\n"));
        assert!(ics.contains("RRULE:FREQ=WEEKLY;COUNT=3\r\n"));
        assert!(ics.contains("SEQUENCE:2\r\n"));
        assert!(ics.contains("STATUS:CONFIRMED\r\n"));
        assert!(!ics.contains("\n\n"));
        for line in ics.split("\r\n") {
            assert!(line.len() <= 75, "{line}");
        }
    }

    #[test]
    fn all_day_utc_and_cancelled() {
        let mut e = event();
        e.all_day = true;
        e.start = utc("2026-11-30T23:00:00Z");
        e.end = utc("2026-12-01T23:00:00Z");
        e.cancelled = true;
        let ics = calendar("x", &[e.clone()], utc("2026-11-28T12:00:00Z"));
        assert!(ics.contains("DTSTART;VALUE=DATE:20261201\r\n"));
        assert!(ics.contains("DTEND;VALUE=DATE:20261202\r\n"));
        assert!(ics.contains("STATUS:CANCELLED\r\n"));
        e.all_day = false;
        e.tz = chrono_tz::UTC;
        let ics = calendar("x", &[e], utc("2026-11-28T12:00:00Z"));
        assert!(ics.contains("DTSTART:20261130T230000Z\r\n"));
    }

    #[test]
    fn long_lines_fold_on_character_boundaries() {
        let mut out = String::new();
        fold(&format!("SUMMARY:{}", "é".repeat(60)), &mut out);
        for line in out.trim_end_matches("\r\n").split("\r\n") {
            assert!(line.len() <= 75);
        }
        let unfolded = out.replace("\r\n ", "");
        assert_eq!(unfolded, format!("SUMMARY:{}\r\n", "é".repeat(60)));
    }

    #[test]
    fn filenames_are_safe() {
        assert_eq!(filename("Team sync / Q4!"), "Team-sync-Q4.ics");
        assert_eq!(filename("///"), "event.ics");
    }
}
