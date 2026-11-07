//! DateTime utilities
//!
//! All times are stored and processed in UTC.
//! Frontend sends the user's IANA timezone (e.g., "Africa/Lagos") with requests.
//! This module converts local times to UTC for storage and provides UTC helpers.

use chrono::{NaiveDateTime, Utc};
use chrono_tz::Tz;

/// Get current time as NaiveDateTime in UTC
pub fn now_utc() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Convert a local NaiveDateTime to UTC using an IANA timezone string.
///
/// # Arguments
/// * `local_time` - A NaiveDateTime representing the time in the user's local timezone
/// * `timezone` - An IANA timezone string (e.g., "Africa/Lagos", "America/New_York")
///
/// # Returns
/// * `Ok(NaiveDateTime)` - The equivalent UTC time
/// * `Err(String)` - If the timezone string is invalid or the local time is ambiguous/invalid
pub fn local_to_utc(local_time: NaiveDateTime, timezone: &str) -> Result<NaiveDateTime, String> {
    let tz: Tz = timezone
        .parse()
        .map_err(|_| format!("Invalid timezone: {}", timezone))?;

    local_time
        .and_local_timezone(tz)
        .earliest()
        .map(|dt| dt.naive_utc())
        .ok_or_else(|| format!("Invalid or ambiguous local time for timezone: {}", timezone))
}

/// Format a NaiveDateTime (assumed UTC) as an ISO 8601 string with Z suffix.
///
/// Example output: "2026-01-25T14:00:00Z"
pub fn format_utc(dt: NaiveDateTime) -> String {
    format!("{}Z", dt.format("%Y-%m-%dT%H:%M:%S"))
}

/// Format an optional NaiveDateTime as a UTC string, or None.
pub fn format_utc_opt(dt: Option<NaiveDateTime>) -> Option<String> {
    dt.map(format_utc)
}

/// The explicit interpretation of a legacy naive `TIMESTAMP` value (DATA-001): it holds
/// UTC. Use this instead of `naive_local()` / server-zone conversions.
pub fn legacy_as_utc(dt: NaiveDateTime) -> chrono::DateTime<Utc> {
    dt.and_utc()
}

/// Session time zones under which a naive legacy `TIMESTAMP` reads as UTC.
const UTC_ZONES: [&str; 4] = ["UTC", "Etc/UTC", "UCT", "Etc/UCT"];

/// DATA-001: refuse to run when the database session would interpret naive legacy
/// columns in a zone other than UTC (`DEFAULT CURRENT_TIMESTAMP`, `now()` comparisons,
/// `AT TIME ZONE` casts). sqlx starts every session in UTC and the runtime pins it
/// (`main.rs`); this proves it.
pub async fn assert_session_utc<C: sea_orm::ConnectionTrait>(db: &C) -> Result<(), String> {
    let row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT current_setting('TimeZone') AS tz".to_owned(),
        ))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no TimeZone setting".to_string())?;
    let tz: String = row.try_get("", "tz").map_err(|e| e.to_string())?;
    session_zone_is_utc(&tz)
        .then_some(())
        .ok_or_else(|| format!("session TimeZone is {tz:?}, expected UTC"))
}

pub fn session_zone_is_utc(tz: &str) -> bool {
    UTC_ZONES.iter().any(|z| z.eq_ignore_ascii_case(tz.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_values_are_read_as_utc_regardless_of_the_zone() {
        let naive =
            NaiveDateTime::parse_from_str("2026-11-07 10:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        assert_eq!(
            legacy_as_utc(naive).to_rfc3339(),
            "2026-11-07T10:00:00+00:00"
        );
        assert_eq!(format_utc(naive), "2026-11-07T10:00:00Z");
    }

    #[test]
    fn only_utc_session_zones_pass() {
        for z in ["UTC", "utc", "Etc/UTC", " UTC "] {
            assert!(session_zone_is_utc(z), "{z}");
        }
        for z in ["Africa/Lagos", "Europe/London", "GMT+1", "localtime", ""] {
            assert!(!session_zone_is_utc(z), "{z}");
        }
    }

    #[test]
    fn local_to_utc_uses_the_given_zone() {
        let local =
            NaiveDateTime::parse_from_str("2026-01-25 14:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        // Africa/Lagos is UTC+1 all year.
        assert_eq!(
            format_utc(local_to_utc(local, "Africa/Lagos").unwrap()),
            "2026-01-25T13:00:00Z"
        );
        assert!(local_to_utc(local, "Not/AZone").is_err());
    }
}
