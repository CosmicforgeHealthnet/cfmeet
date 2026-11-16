//! Service-side routing of the closed event catalogue (Document 4 §5.2, §7): for each of
//! the 31 inbound types, the routing category (the unit of the cutover: one row per
//! channel in `notif_routing`, and the producer-side `notification_routing_state`
//! category), the user preference category, the channels, the template, the inbox action
//! and the email ledger category.
//!
//! Routing categories follow the migration sequence of Document 5 §6: in-app / push
//! categories first, then STANDARD email (meeting notices, developer), invitations,
//! security notices, and the authentication codes last. Email categories are today's
//! Meeting ledger categories, so the ledgers stay comparable during the shadow window.

use cf_notification_client::{spec, EventSpec, Lane};

/// Delivery channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    Email,
    Push,
    InApp,
    Operator,
}

impl Channel {
    pub const ALL: [Channel; 4] = [
        Channel::Email,
        Channel::Push,
        Channel::InApp,
        Channel::Operator,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Email => "EMAIL",
            Channel::Push => "PUSH",
            Channel::InApp => "IN_APP",
            Channel::Operator => "OPERATOR",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == s)
    }
}

/// The eleven user preference categories (+ the fixed SECURITY / OPERATOR categories).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefCategory {
    DirectMessages,
    GroupMessages,
    IncomingCalls,
    MissedCalls,
    ContactRequests,
    GroupInvitations,
    MeetingInvitations,
    MeetingReminders,
    Voicemail,
    OrganizationUpdates,
    DeveloperUpdates,
    /// Fixed on, never stored; ignores mute, category, channel and quiet hours.
    Security,
    /// Operator alerts (static operator group; no user preference).
    Operator,
}

impl PrefCategory {
    pub const USER: [PrefCategory; 11] = [
        PrefCategory::DirectMessages,
        PrefCategory::GroupMessages,
        PrefCategory::IncomingCalls,
        PrefCategory::MissedCalls,
        PrefCategory::ContactRequests,
        PrefCategory::GroupInvitations,
        PrefCategory::MeetingInvitations,
        PrefCategory::MeetingReminders,
        PrefCategory::Voicemail,
        PrefCategory::OrganizationUpdates,
        PrefCategory::DeveloperUpdates,
    ];

    /// Contract `Category` value.
    pub fn as_str(self) -> &'static str {
        match self {
            PrefCategory::DirectMessages => "DIRECT_MESSAGES",
            PrefCategory::GroupMessages => "GROUP_MESSAGES",
            PrefCategory::IncomingCalls => "INCOMING_CALLS",
            PrefCategory::MissedCalls => "MISSED_CALLS",
            PrefCategory::ContactRequests => "CONTACT_REQUESTS",
            PrefCategory::GroupInvitations => "GROUP_INVITATIONS",
            PrefCategory::MeetingInvitations => "MEETING_INVITATIONS",
            PrefCategory::MeetingReminders => "MEETING_REMINDERS",
            PrefCategory::Voicemail => "VOICEMAIL",
            PrefCategory::OrganizationUpdates => "ORGANIZATION_UPDATES",
            PrefCategory::DeveloperUpdates => "DEVELOPER_UPDATES",
            PrefCategory::Security => "SECURITY",
            PrefCategory::Operator => "OPERATOR_ALERTS",
        }
    }

    /// The camelCase key in `NotificationPreferences.categories` (U6/U7).
    pub fn json_key(self) -> &'static str {
        match self {
            PrefCategory::DirectMessages => "directMessages",
            PrefCategory::GroupMessages => "groupMessages",
            PrefCategory::IncomingCalls => "incomingCalls",
            PrefCategory::MissedCalls => "missedCalls",
            PrefCategory::ContactRequests => "contactRequests",
            PrefCategory::GroupInvitations => "groupInvitations",
            PrefCategory::MeetingInvitations => "meetingInvitations",
            PrefCategory::MeetingReminders => "meetingReminders",
            PrefCategory::Voicemail => "voicemail",
            PrefCategory::OrganizationUpdates => "organizationUpdates",
            PrefCategory::DeveloperUpdates => "developerUpdates",
            PrefCategory::Security => "security",
            PrefCategory::Operator => "operatorAlerts",
        }
    }

    /// The `notif_preferences` column.
    pub fn column(self) -> Option<&'static str> {
        Some(match self {
            PrefCategory::DirectMessages => "direct_messages",
            PrefCategory::GroupMessages => "group_messages",
            PrefCategory::IncomingCalls => "incoming_calls",
            PrefCategory::MissedCalls => "missed_calls",
            PrefCategory::ContactRequests => "contact_requests",
            PrefCategory::GroupInvitations => "group_invitations",
            PrefCategory::MeetingInvitations => "meeting_invitations",
            PrefCategory::MeetingReminders => "meeting_reminders",
            PrefCategory::Voicemail => "voicemail",
            PrefCategory::OrganizationUpdates => "organization_updates",
            PrefCategory::DeveloperUpdates => "developer_updates",
            PrefCategory::Security | PrefCategory::Operator => return None,
        })
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::USER
            .iter()
            .copied()
            .chain([PrefCategory::Security, PrefCategory::Operator])
            .find(|c| c.as_str() == s)
    }
}

/// The closed inbox action kinds (contract `ActionKind`).
pub const ACTION_KINDS: &[&str] = &[
    "OPEN_CONVERSATION",
    "OPEN_CALL_HISTORY",
    "OPEN_VOICEMAIL",
    "OPEN_RECORDING",
    "OPEN_MEETING",
    "OPEN_ORGANIZATION",
    "OPEN_ORGANIZATION_INVITATION",
    "OPEN_CONTACT_REQUEST",
    "OPEN_CONTACT",
    "OPEN_SECURITY_SETTINGS",
    "OPEN_DEVELOPER_APPLICATION",
    "NONE",
];

/// How the service handles one event type.
#[derive(Debug, Clone, Copy)]
pub struct TypeRoute {
    pub event_type: &'static str,
    pub route_category: &'static str,
    pub pref: PrefCategory,
    pub channels: &'static [Channel],
    pub template: &'static str,
    pub action: &'static str,
    /// The `data` key whose UUID is the inbox action reference.
    pub action_ref: Option<&'static str>,
    /// The Meeting ledger category used for email (`None`: no email).
    pub email_category: Option<&'static str>,
    /// Sender profile (configuration decides the From header).
    pub sender_profile: &'static str,
    /// Inbox item lifetime (days).
    pub inbox_days: i64,
}

impl TypeRoute {
    pub fn spec(&self) -> &'static EventSpec {
        spec(self.event_type).expect("routed types are in the catalogue")
    }

    pub fn lane(&self) -> Lane {
        self.spec().lane()
    }

    pub fn has(&self, channel: Channel) -> bool {
        self.channels.contains(&channel)
    }
}

use Channel::{Email as E, InApp as I, Operator as O, Push as P};
use PrefCategory as C;

const fn r(
    event_type: &'static str,
    route_category: &'static str,
    pref: PrefCategory,
    channels: &'static [Channel],
    template: &'static str,
    action: &'static str,
    action_ref: Option<&'static str>,
) -> TypeRoute {
    TypeRoute {
        event_type,
        route_category,
        pref,
        channels,
        template,
        action,
        action_ref,
        email_category: None,
        sender_profile: "default",
        inbox_days: 90,
    }
}

pub const ROUTES: &[TypeRoute] = &[
    r(
        "organizations.invitation.created",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I, P],
        "org.invitation.created",
        "OPEN_ORGANIZATION_INVITATION",
        Some("invitationId"),
    ),
    r(
        "organizations.invitation.accepted",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I],
        "org.invitation.accepted",
        "OPEN_ORGANIZATION",
        Some("organizationId"),
    ),
    r(
        "organizations.invitation.declined",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I],
        "org.invitation.declined",
        "OPEN_ORGANIZATION",
        Some("organizationId"),
    ),
    r(
        "organizations.invitation.revoked",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I],
        "org.invitation.revoked",
        "NONE",
        None,
    ),
    r(
        "organizations.invitation.expired",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I],
        "org.invitation.expired",
        "NONE",
        None,
    ),
    r(
        "organizations.membership.role_changed",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I, P],
        "org.membership.role_changed",
        "OPEN_ORGANIZATION",
        Some("organizationId"),
    ),
    r(
        "organizations.membership.suspended",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I, P],
        "org.membership.suspended",
        "OPEN_ORGANIZATION",
        Some("organizationId"),
    ),
    r(
        "organizations.membership.removed",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I, P],
        "org.membership.removed",
        "NONE",
        None,
    ),
    r(
        "organizations.ownership.transferred",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I, P],
        "org.ownership.transferred",
        "OPEN_ORGANIZATION",
        Some("organizationId"),
    ),
    r(
        "organizations.organization.suspended",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I, P],
        "org.organization.suspended",
        "OPEN_ORGANIZATION",
        Some("organizationId"),
    ),
    r(
        "communications.connection.requested",
        "CONTACT_REQUESTS",
        C::ContactRequests,
        &[I, P],
        "contact.requested",
        "OPEN_CONTACT_REQUEST",
        Some("connectionId"),
    ),
    r(
        "communications.connection.connected",
        "CONTACT_REQUESTS",
        C::ContactRequests,
        &[I, P],
        "contact.connected",
        "OPEN_CONTACT",
        Some("connectionId"),
    ),
    // The preference category (direct / group) is chosen from `conversationKind`.
    TypeRoute {
        inbox_days: 1,
        ..r(
            "messaging.message.created",
            "MESSAGE_WAKEUPS",
            C::DirectMessages,
            &[P],
            "message.created",
            "OPEN_CONVERSATION",
            Some("conversationId"),
        )
    },
    r(
        "messaging.member.invited",
        "GROUP_INVITATIONS",
        C::GroupInvitations,
        &[I, P],
        "message.member_invited",
        "OPEN_CONVERSATION",
        Some("conversationId"),
    ),
    r(
        "calling.call.participant-missed",
        "MISSED_CALLS",
        C::MissedCalls,
        &[I, P],
        "call.missed",
        "OPEN_CALL_HISTORY",
        Some("callId"),
    ),
    r(
        "calling.voicemail.ready",
        "VOICEMAIL",
        C::Voicemail,
        &[I, P],
        "call.voicemail",
        "OPEN_VOICEMAIL",
        Some("voiceMessageId"),
    ),
    r(
        "calling.recording.ready",
        "VOICEMAIL",
        C::Voicemail,
        &[I, P],
        "call.recording",
        "OPEN_RECORDING",
        Some("recordingId"),
    ),
    TypeRoute {
        email_category: Some("MEETING_NOTIFICATION"),
        ..r(
            "meeting-service.conference.scheduled",
            "MEETING_NOTIFICATION",
            C::MeetingReminders,
            &[E, I],
            "meeting.reminder",
            "OPEN_MEETING",
            Some("conferenceId"),
        )
    },
    TypeRoute {
        email_category: Some("MEETING_NOTIFICATION"),
        ..r(
            "meeting-service.conference.rescheduled",
            "MEETING_NOTIFICATION",
            C::MeetingInvitations,
            &[E, I],
            "meeting.rescheduled",
            "OPEN_MEETING",
            Some("conferenceId"),
        )
    },
    TypeRoute {
        email_category: Some("MEETING_NOTIFICATION"),
        ..r(
            "meeting-service.conference.cancelled",
            "MEETING_NOTIFICATION",
            C::MeetingInvitations,
            &[E, I],
            "meeting.cancelled",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("MEETING_INVITATION"),
        ..r(
            "meeting-service.invitation.email-requested",
            "MEETING_INVITATION",
            C::MeetingInvitations,
            &[E],
            "meeting.invitation",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("ORGANIZATION_INVITATION"),
        sender_profile: "organization-invitation",
        ..r(
            "meeting-service.organization-invitation.email-requested",
            "ORGANIZATION_INVITATION",
            C::OrganizationUpdates,
            &[E],
            "org.invitation.email",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("AUTH_VERIFICATION"),
        ..r(
            "account.verification-code.requested",
            "AUTH_VERIFICATION",
            C::Security,
            &[E],
            "account.verification_code",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("AUTH_PASSWORD_RESET"),
        ..r(
            "account.password-reset-code.requested",
            "AUTH_PASSWORD_RESET",
            C::Security,
            &[E],
            "account.password_reset_code",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("SECURITY_DEVICE_STEP_UP"),
        ..r(
            "account.device-step-up-code.requested",
            "SECURITY_DEVICE_STEP_UP",
            C::Security,
            &[E],
            "account.device_step_up_code",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("SECURITY_PASSWORD_CHANGED"),
        ..r(
            "account.security.password-changed",
            "SECURITY_PASSWORD_CHANGED",
            C::Security,
            &[E, I],
            "account.password_changed",
            "OPEN_SECURITY_SETTINGS",
            None,
        )
    },
    // Today's Meeting ledger records device notices under NOTIFICATION.
    TypeRoute {
        email_category: Some("NOTIFICATION"),
        ..r(
            "account.security.device-linked",
            "SECURITY_DEVICE_LINKED",
            C::Security,
            &[E, I],
            "account.device_linked",
            "OPEN_SECURITY_SETTINGS",
            None,
        )
    },
    TypeRoute {
        email_category: Some("DEVELOPER_PLATFORM"),
        ..r(
            "developer-platform.credential.expiring-soon",
            "DEVELOPER_PLATFORM",
            C::DeveloperUpdates,
            &[E, I],
            "developer.credential_expiring",
            "OPEN_DEVELOPER_APPLICATION",
            Some("applicationId"),
        )
    },
    TypeRoute {
        email_category: Some("DEVELOPER_PLATFORM"),
        ..r(
            "developer-platform.credential.expired",
            "DEVELOPER_PLATFORM",
            C::DeveloperUpdates,
            &[E, I],
            "developer.credential_expired",
            "OPEN_DEVELOPER_APPLICATION",
            Some("applicationId"),
        )
    },
    TypeRoute {
        email_category: Some("DEVELOPER_PLATFORM"),
        ..r(
            "developer-platform.production-access.decided",
            "DEVELOPER_PLATFORM",
            C::DeveloperUpdates,
            &[E, I],
            "developer.production_access",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("OPERATIONS_ALERT"),
        ..r(
            "operator.alert.fired",
            "OPERATOR_ALERTS",
            C::Operator,
            &[O],
            "operator.alert",
            "NONE",
            None,
        )
    },
];

/// Every routing category with the channels it can use (the `notif_routing` seed rows),
/// plus `CALL_INVITATIONS` / PUSH for the I3 fast lane.
pub fn routing_rows() -> Vec<(&'static str, Channel)> {
    let mut rows: Vec<(&'static str, Channel)> = Vec::new();
    for t in ROUTES {
        for c in t.channels {
            if !rows.contains(&(t.route_category, *c)) {
                rows.push((t.route_category, *c));
            }
        }
    }
    // Push is routable for meeting notices (FE4 mobile reminders) and every in-app
    // category with a push channel already listed above.
    for extra in [
        ("MEETING_NOTIFICATION", Channel::Push),
        ("CALL_INVITATIONS", Channel::Push),
    ] {
        if !rows.contains(&extra) {
            rows.push(extra);
        }
    }
    rows
}

pub fn route(event_type: &str) -> Option<&'static TypeRoute> {
    ROUTES.iter().find(|r| r.event_type == event_type)
}

/// The routing category of an event type (control event: `CONTROL`).
pub fn route_category(event_type: &str) -> &'static str {
    route(event_type)
        .map(|r| r.route_category)
        .unwrap_or("CONTROL")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cf_notification_client::CATALOGUE;

    #[test]
    fn every_inbound_type_is_routed_exactly_once() {
        let inbound: Vec<&str> = CATALOGUE
            .iter()
            .filter(|s| !s.is_control())
            .map(|s| s.event_type)
            .collect();
        assert_eq!(inbound.len(), 31);
        for t in &inbound {
            assert_eq!(
                ROUTES.iter().filter(|r| r.event_type == *t).count(),
                1,
                "{t}"
            );
        }
        assert_eq!(ROUTES.len(), 31);
    }

    #[test]
    fn critical_types_are_security_email_only_with_legacy_categories() {
        for r in ROUTES {
            if r.lane() == Lane::Critical {
                assert_eq!(r.pref, PrefCategory::Security, "{}", r.event_type);
                assert!(r.has(Channel::Email));
                assert!(r.email_category.is_some());
            }
            assert!(ACTION_KINDS.contains(&r.action), "{}", r.action);
            assert_eq!(
                r.has(Channel::Email) || r.has(Channel::Operator),
                r.email_category.is_some(),
                "{}",
                r.event_type
            );
            if let Some(key) = r.action_ref {
                assert!(
                    r.spec().required_data.contains(&key) || r.spec().optional_data.contains(&key),
                    "{} action ref {key}",
                    r.event_type
                );
            }
        }
    }

    #[test]
    fn routing_rows_match_the_migration_seed() {
        let seed = include_str!("../migration/src/m20261116_000001_notifications_foundation.rs");
        let rows = routing_rows();
        for (cat, ch) in &rows {
            assert!(
                seed.contains(&format!("('{cat}', '{}', 'OFF', 'migration')", ch.as_str())),
                "missing seed row {cat}/{}",
                ch.as_str()
            );
        }
        assert_eq!(seed.matches("'OFF', 'migration')").count(), rows.len());
    }
}
