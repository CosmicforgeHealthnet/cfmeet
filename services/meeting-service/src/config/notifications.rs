//! Blueprint v1.0 Section 11 notification settings (MTG-006, ACC-020).
//!
//! Everything is OFF by default: no notification email is produced, queued or sent until
//! `NOTIFICATIONS_EMAIL_ENABLED=true`. Delivery reuses the existing email queue and worker
//! (`SMTP_*`); push channels are not implemented here (provider-gated, see the Section 11
//! operations record).

use super::{env_bool, env_num};

#[derive(Debug, Clone)]
pub struct NotificationConfig {
    /// `NOTIFICATIONS_EMAIL_ENABLED` (default false): invitation emails on request,
    /// reschedule/cancellation notices and reminders.
    pub email_enabled: bool,
    /// `NOTIFICATIONS_REMINDER_LEAD_MINUTES` (default 15, 0 disables reminders, at most
    /// 1440): a reminder is sent once per scheduled start when the start is this close.
    pub reminder_lead_minutes: i64,
    /// `NOTIFICATIONS_MAX_RECIPIENTS` (default 200, 1-1000): recipients per notification.
    pub max_recipients: usize,
    /// `NOTIFICATIONS_POLL_SECS` (default 30, at least 5): worker cadence.
    pub poll_secs: u64,
    /// `NOTIFICATIONS_INVITE_EMAILS_PER_HOUR` (default 50, 1-1000): invitation emails one
    /// account may request per hour (meeting and organization invitations together).
    pub invite_emails_per_hour: i64,
    /// Jobs claimed per worker tick.
    pub batch: i64,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            email_enabled: false,
            reminder_lead_minutes: 15,
            max_recipients: 200,
            poll_secs: 30,
            invite_emails_per_hour: 50,
            batch: 20,
        }
    }
}

impl NotificationConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            email_enabled: env_bool("NOTIFICATIONS_EMAIL_ENABLED", d.email_enabled),
            reminder_lead_minutes: env_num(
                "NOTIFICATIONS_REMINDER_LEAD_MINUTES",
                d.reminder_lead_minutes,
            )
            .clamp(0, 24 * 60),
            max_recipients: env_num("NOTIFICATIONS_MAX_RECIPIENTS", d.max_recipients)
                .clamp(1, 1000),
            poll_secs: env_num("NOTIFICATIONS_POLL_SECS", d.poll_secs).max(5),
            invite_emails_per_hour: env_num(
                "NOTIFICATIONS_INVITE_EMAILS_PER_HOUR",
                d.invite_emails_per_hour,
            )
            .clamp(1, 1000),
            batch: d.batch,
        }
    }

    /// Reminders are produced only with email enabled and a positive lead time.
    pub fn reminders_enabled(&self) -> bool {
        self.email_enabled && self.reminder_lead_minutes > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_are_off_by_default() {
        let c = NotificationConfig::default();
        assert!(!c.email_enabled);
        assert!(!c.reminders_enabled());
        assert!(NotificationConfig {
            email_enabled: true,
            ..Default::default()
        }
        .reminders_enabled());
        assert!(!NotificationConfig {
            email_enabled: true,
            reminder_lead_minutes: 0,
            ..Default::default()
        }
        .reminders_enabled());
    }
}
