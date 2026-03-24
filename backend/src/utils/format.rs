//! Formatting utility functions
//!
//! Helper functions for formatting various data types into human-readable strings.

use crate::models::{
    meetings::MeetingStatus,
    participants::{ParticipantRole, ParticipantStatus},
};

/// Format MeetingStatus enum to string representation
/// 
/// Converts the database enum to user-friendly string for API responses.
/// 
/// # Examples
/// - `MeetingStatus::Scheduled` → "scheduled"
/// - `MeetingStatus::Ongoing` → "live"
/// - `MeetingStatus::Ended` → "ended"
/// - `MeetingStatus::Cancelled` → "cancelled"
pub fn format_status(status: &MeetingStatus) -> String {
    match status {
        MeetingStatus::Scheduled => "scheduled".to_string(),
        MeetingStatus::Ongoing => "live".to_string(),
        MeetingStatus::Ended => "ended".to_string(),
        MeetingStatus::Cancelled => "cancelled".to_string(),
    }
}

/// Format ParticipantRole enum to string representation
/// 
/// Converts the database enum to user-friendly string for API responses.
/// 
/// # Examples
/// - `ParticipantRole::Host` → "host"
/// - `ParticipantRole::Participant` → "participant"
/// - `ParticipantRole::Viewer` → "viewer"
pub fn format_role(role: &ParticipantRole) -> String {
    match role {
        ParticipantRole::Host => "host".to_string(),
        ParticipantRole::Participant => "participant".to_string(),
        ParticipantRole::Viewer => "viewer".to_string(),
    }
}

/// Format ParticipantStatus enum to string representation
/// 
/// Converts the database enum to user-friendly string for API responses.
/// 
/// # Examples
/// - `ParticipantStatus::Waiting` → "waiting"
/// - `ParticipantStatus::Joined` → "joined"
/// - `ParticipantStatus::Kicked` → "kicked"
pub fn format_participant_status(status: &ParticipantStatus) -> String {
    match status {
        ParticipantStatus::Waiting => "waiting".to_string(),
        ParticipantStatus::Joined => "joined".to_string(),
        ParticipantStatus::Kicked => "kicked".to_string(),
    }
}

/// Format duration in minutes to human-readable format
/// 
/// Converts a duration in minutes to a natural language string with weeks, days, hours, and minutes.
/// Handles singular/plural forms correctly and joins multiple units with commas and "and".
/// 
/// # Examples
/// - `30` → "30 minutes"
/// - `60` → "1 hour"
/// - `90` → "1 hour and 30 minutes"
/// - `760` → "12 hours and 40 minutes"
/// - `1440` → "1 day"
/// - `10080` → "1 week"
/// - `11520` → "1 week and 1 day"
/// 
/// # Arguments
/// * `minutes` - Duration in minutes (negative values are treated as 0)
/// 
/// # Returns
/// A human-readable string representation of the duration
pub fn format_duration(minutes: i64) -> String {
    if minutes < 0 {
        return "0 minutes".to_string();
    }

    let weeks = minutes / (7 * 24 * 60);
    let days = (minutes % (7 * 24 * 60)) / (24 * 60);
    let hours = (minutes % (24 * 60)) / 60;
    let mins = minutes % 60;

    let mut parts = Vec::new();

    if weeks > 0 {
        parts.push(format!("{} week{}", weeks, if weeks == 1 { "" } else { "s" }));
    }
    if days > 0 {
        parts.push(format!("{} day{}", days, if days == 1 { "" } else { "s" }));
    }
    if hours > 0 {
        parts.push(format!("{} hour{}", hours, if hours == 1 { "" } else { "s" }));
    }
    if mins > 0 {
        parts.push(format!("{} minute{}", mins, if mins == 1 { "" } else { "s" }));
    }

    if parts.is_empty() {
        "0 minutes".to_string()
    } else if parts.len() == 1 {
        parts[0].clone()
    } else {
        let last = parts.pop().unwrap();
        format!("{} and {}", parts.join(", "), last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration_minutes_only() {
        assert_eq!(format_duration(0), "0 minutes");
        assert_eq!(format_duration(1), "1 minute");
        assert_eq!(format_duration(30), "30 minutes");
        assert_eq!(format_duration(59), "59 minutes");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(60), "1 hour");
        assert_eq!(format_duration(120), "2 hours");
        assert_eq!(format_duration(90), "1 hour and 30 minutes");
    }

    #[test]
    fn test_format_duration_days() {
        assert_eq!(format_duration(1440), "1 day");
        assert_eq!(format_duration(2880), "2 days");
        assert_eq!(format_duration(1500), "1 day and 1 hour");
    }

    #[test]
    fn test_format_duration_weeks() {
        assert_eq!(format_duration(10080), "1 week");
        assert_eq!(format_duration(20160), "2 weeks");
        assert_eq!(format_duration(11520), "1 week and 1 day");
    }

    #[test]
    fn test_format_duration_complex() {
        assert_eq!(format_duration(760), "12 hours and 40 minutes");
        assert_eq!(format_duration(10140), "1 week and 1 hour");
    }

    #[test]
    fn test_format_duration_negative() {
        assert_eq!(format_duration(-10), "0 minutes");
    }
}
