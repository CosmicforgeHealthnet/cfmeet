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
