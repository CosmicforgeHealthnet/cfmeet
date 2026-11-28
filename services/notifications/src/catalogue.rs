//! Service-side routing of the closed event catalogue (Document 4 §5.2, §7): for each of
//! the 44 inbound types, the routing category (the unit of the cutover: one row per
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
    // ADR 0069 (common contract 1.8.0): a Messaging status (ref = the status id).
    "OPEN_STATUS",
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
    // Gap audit G19 (event contract 1.4.0): mirrors organization.suspended.
    r(
        "organizations.organization.archived",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I, P],
        "org.organization.archived",
        "OPEN_ORGANIZATION",
        Some("organizationId"),
    ),
    r(
        "organizations.organization.reactivated",
        "ORGANIZATION_UPDATES",
        C::OrganizationUpdates,
        &[I, P],
        "org.organization.reactivated",
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
    // Gap audit M12: meeting notices also reach the in-app inbox and push (routing row
    // MEETING_NOTIFICATION / PUSH, seeded OFF), under the meeting preference categories.
    TypeRoute {
        email_category: Some("MEETING_NOTIFICATION"),
        ..r(
            "meeting-service.conference.scheduled",
            "MEETING_NOTIFICATION",
            C::MeetingReminders,
            &[E, I, P],
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
            &[E, I, P],
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
            &[E, I, P],
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
    // Passwordless account codes (contract 1.2.0, PR #97): the same routing categories and
    // ledger categories as their Meeting enqueue points, so the fence category that routes
    // them on the Meeting side is the one that routes them here.
    TypeRoute {
        email_category: Some("AUTH_VERIFICATION"),
        ..r(
            "account.sign-in-code.requested",
            "AUTH_VERIFICATION",
            C::Security,
            &[E],
            "account.sign_in_code",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("AUTH_VERIFICATION"),
        ..r(
            "account.email-change-code.requested",
            "AUTH_VERIFICATION",
            C::Security,
            &[E],
            "account.email_change_code",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("SECURITY_DEVICE_STEP_UP"),
        ..r(
            "account.step-up-code.requested",
            "SECURITY_DEVICE_STEP_UP",
            C::Security,
            &[E],
            "account.step_up_code",
            "NONE",
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
    // Account notices (contract 1.3.0, owner decision A): one routing category,
    // ACCOUNT_NOTICES (the Meeting fence category that routes them), email only, SECURITY;
    // the ledger categories are the ones Meeting records them under.
    TypeRoute {
        email_category: Some("AUTH_WELCOME"),
        ..r(
            "account.welcome.requested",
            "ACCOUNT_NOTICES",
            C::Security,
            &[E],
            "account.welcome",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("NOTIFICATION"),
        ..r(
            "account.security.new-sign-in",
            "ACCOUNT_NOTICES",
            C::Security,
            &[E],
            "account.new_sign_in",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("NOTIFICATION"),
        ..r(
            "account.security.email-in-use",
            "ACCOUNT_NOTICES",
            C::Security,
            &[E],
            "account.email_in_use",
            "NONE",
            None,
        )
    },
    TypeRoute {
        email_category: Some("NOTIFICATION"),
        ..r(
            "account.security.email-changed",
            "ACCOUNT_NOTICES",
            C::Security,
            &[E],
            "account.email_changed",
            "NONE",
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
    // ADR 0068 A4 (event contract 1.6.0): the first-party workspace owner invitation, email
    // only, to the sealed address of its one email principal (the owner may have no account
    // yet); the single-use claim link is rebuilt from the sealed token.
    TypeRoute {
        email_category: Some("DEVELOPER_PLATFORM"),
        ..r(
            "developer-platform.workspace.owner-invitation.email-requested",
            "DEVELOPER_PLATFORM",
            C::DeveloperUpdates,
            &[E],
            "developer.workspace_owner_invitation",
            "NONE",
            None,
        )
    },
    // ADR 0068 A4 (event contract 1.7.0): an organization export's single-use download
    // link, email only, to the owner's account (X1 resolves the verified address and
    // authorizes the send); the link is rebuilt from the sealed token. SECURITY preference
    // (fixed on): a link HQ requested for the owner is never dropped by a mute or quiet
    // hours — it would expire and lose the export.
    TypeRoute {
        email_category: Some("ORGANIZATION_EXPORT"),
        ..r(
            "organizations.organization.export_ready",
            "ORGANIZATIONS",
            C::Security,
            &[E],
            "org.export_ready",
            "NONE",
            None,
        )
    },
    // ADR 0069 (event contract 1.9.0): Status in Messaging. One routing category,
    // MESSAGING_STATUS (in-app + push, seeded OFF by
    // m20261220_000001_messaging_status_notices), to the one CF Meet account concerned (a
    // mentioned person inside the audience, or the author whose status was shared), under
    // the direct-message preference. OPEN_STATUS on statusId.
    r(
        "messaging.status.mentioned",
        "MESSAGING_STATUS",
        C::DirectMessages,
        &[I, P],
        "status.mentioned",
        "OPEN_STATUS",
        Some("statusId"),
    ),
    r(
        "messaging.status.shared",
        "MESSAGING_STATUS",
        C::DirectMessages,
        &[I, P],
        "status.shared",
        "OPEN_STATUS",
        Some("statusId"),
    ),
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
    // Push for meeting notices (M12) is listed by their routes above; the extra rows stay
    // so the seed matches even if a route changes.
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
        assert_eq!(inbound.len(), 44);
        for t in &inbound {
            assert_eq!(
                ROUTES.iter().filter(|r| r.event_type == *t).count(),
                1,
                "{t}"
            );
        }
        assert_eq!(ROUTES.len(), 44);
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

    /// Gap audit G19: archive / reactivation route exactly like organization.suspended
    /// (ORGANIZATION_UPDATES, in-app + push, OPEN_ORGANIZATION on organizationId).
    #[test]
    fn organization_lifecycle_types_mirror_suspension() {
        let base = route("organizations.organization.suspended").unwrap();
        for (t, template) in [
            (
                "organizations.organization.archived",
                "org.organization.archived",
            ),
            (
                "organizations.organization.reactivated",
                "org.organization.reactivated",
            ),
        ] {
            let r = route(t).expect(t);
            assert_eq!(r.route_category, base.route_category, "{t}");
            assert_eq!(r.pref, PrefCategory::OrganizationUpdates, "{t}");
            assert_eq!(r.channels, &[Channel::InApp, Channel::Push], "{t}");
            assert_eq!(r.template, template);
            assert_eq!(r.action, "OPEN_ORGANIZATION");
            assert_eq!(r.action_ref, Some("organizationId"));
            assert_eq!(r.email_category, None);
            let spec = r.spec();
            assert_eq!(
                spec.producer,
                Some(cf_notification_client::Producer::Organizations)
            );
            assert_eq!(spec.priority, base.spec().priority);
            assert_eq!(spec.category, Some("ORGANIZATION_UPDATES"));
            assert_eq!(spec.subject_kind, Some("ORGANIZATION"));
            assert_eq!(spec.required_data, &["organizationId"]);
            let (title, body) = crate::templates::inbox_text(template, &serde_json::json!({}));
            assert_ne!(title, "Notification", "{template} has its own text");
            assert!(!body.is_empty());
        }
    }

    /// ADR 0069: the two Messaging status notices share MESSAGING_STATUS (in-app + push,
    /// no email), open the status and go to exactly one CF Meet account.
    #[test]
    fn messaging_status_notices_route_to_messaging_status() {
        for (t, template) in [
            ("messaging.status.mentioned", "status.mentioned"),
            ("messaging.status.shared", "status.shared"),
        ] {
            let r = route(t).expect(t);
            assert_eq!(r.route_category, "MESSAGING_STATUS", "{t}");
            assert_eq!(r.pref, PrefCategory::DirectMessages, "{t}");
            assert_eq!(r.channels, &[Channel::InApp, Channel::Push], "{t}");
            assert_eq!(r.template, template);
            assert_eq!(r.action, "OPEN_STATUS");
            assert_eq!(r.action_ref, Some("statusId"));
            assert_eq!(r.email_category, None);
            let spec = r.spec();
            assert_eq!(
                spec.producer,
                Some(cf_notification_client::Producer::Messaging)
            );
            assert_eq!(spec.priority, cf_notification_client::Priority::Normal);
            assert_eq!(spec.category, Some("DIRECT_MESSAGES"));
            assert_eq!(spec.subject_kind, Some("STATUS"));
            assert_eq!(spec.required_data, &["statusId"]);
            assert!(spec.sealed.is_empty());
            assert_eq!(
                spec.recipient,
                Some(cf_notification_client::RecipientKind::Account)
            );
            let (title, body) = crate::templates::inbox_text(template, &serde_json::json!({}));
            assert_ne!(title, "Notification", "{template} has its own text");
            assert!(!body.is_empty());
        }
    }

    #[test]
    fn routing_rows_match_the_migration_seed() {
        // The foundation seed, then the rows later migrations add (ACCOUNT_NOTICES,
        // ORGANIZATIONS, MESSAGING_STATUS).
        let seed = [
            include_str!("../migration/src/m20261116_000001_notifications_foundation.rs"),
            include_str!("../migration/src/m20261118_000001_account_notices.rs"),
            include_str!("../migration/src/m20261121_000001_organization_exports.rs"),
            include_str!("../migration/src/m20261220_000001_messaging_status_notices.rs"),
        ]
        .concat();
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
