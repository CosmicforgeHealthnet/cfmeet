//! The closed event catalogue (Document 4 §5.2): 34 inbound types (30 core, the optional
//! `operator.alert.fired` approved under D-15, and since contract 1.1.0 the three
//! passwordless account codes #32–#34 of PR #97) and the control event
//! `notifications.cancel`. It mirrors `x-cfmeet-catalogue` in the contract
//! `packages/contracts/notifications/events/v1.schema.json`; `tests/contract.rs` fails if
//! the two ever differ.

use serde::{Deserialize, Serialize};

/// A producer allowed to publish notification events (closed allowlist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Producer {
    #[serde(rename = "cfmeet.meeting-service")]
    MeetingService,
    #[serde(rename = "cfmeet.organizations")]
    Organizations,
    #[serde(rename = "cfmeet.communications")]
    Communications,
    #[serde(rename = "cfmeet.calling")]
    Calling,
    #[serde(rename = "cfmeet.messaging")]
    Messaging,
    #[serde(rename = "cfmeet.developer-platform")]
    DeveloperPlatform,
    /// Only the I12 wrapper of Alertmanager webhooks (static bearer, private listener).
    #[serde(rename = "alertmanager")]
    Alertmanager,
}

impl Producer {
    pub const ALL: &'static [Producer] = &[
        Producer::MeetingService,
        Producer::Organizations,
        Producer::Communications,
        Producer::Calling,
        Producer::Messaging,
        Producer::DeveloperPlatform,
        Producer::Alertmanager,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Producer::MeetingService => "cfmeet.meeting-service",
            Producer::Organizations => "cfmeet.organizations",
            Producer::Communications => "cfmeet.communications",
            Producer::Calling => "cfmeet.calling",
            Producer::Messaging => "cfmeet.messaging",
            Producer::DeveloperPlatform => "cfmeet.developer-platform",
            Producer::Alertmanager => "alertmanager",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.as_str() == value)
    }
}

/// Envelope priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Priority {
    Critical,
    High,
    Normal,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Critical => "CRITICAL",
            Priority::High => "HIGH",
            Priority::Normal => "NORMAL",
        }
    }

    /// The service lane an event of this priority is processed in.
    pub fn lane(self) -> Lane {
        match self {
            Priority::Critical => Lane::Critical,
            Priority::High => Lane::High,
            Priority::Normal => Lane::Standard,
        }
    }
}

/// Processing lane (P7: lanes isolate criticality).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Lane {
    Critical,
    High,
    Standard,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Lane::Critical => "CRITICAL",
            Lane::High => "HIGH",
            Lane::Standard => "STANDARD",
        }
    }
}

/// What a sealed part may (and for codes / tokens must) contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealedField {
    Code,
    Token,
    Address,
}

impl SealedField {
    pub fn as_str(self) -> &'static str {
        match self {
            SealedField::Code => "code",
            SealedField::Token => "token",
            SealedField::Address => "address",
        }
    }
}

/// Today's idempotency-key formats (Document 4 §5.1): the new ledger keeps the same
/// dedupe behaviour as the Meeting ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyKey {
    OtpVerify,
    OtpReset,
    /// `otp:signin:` — passwordless sign-in / sign-up code (PR #97).
    OtpSignIn,
    /// `otp:emailchange:` — code to the new address of an email change (PR #97).
    OtpEmailChange,
    /// `otp:stepup:` — action step-up code (PR #97).
    OtpStepUp,
    DeviceStepUp,
    PasswordChanged,
    DeviceLinked,
    MeetingInvitation,
    OrgInvitation,
    MeetingNotification,
    DevplatAccess,
}

fn free_suffix(s: &str) -> bool {
    (1..=140).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.' | b'-'))
}

fn uuid_like(s: &str) -> bool {
    s.len() == 36 && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

impl LegacyKey {
    pub const ALL: &'static [LegacyKey] = &[
        LegacyKey::OtpVerify,
        LegacyKey::OtpReset,
        LegacyKey::OtpSignIn,
        LegacyKey::OtpEmailChange,
        LegacyKey::OtpStepUp,
        LegacyKey::DeviceStepUp,
        LegacyKey::PasswordChanged,
        LegacyKey::DeviceLinked,
        LegacyKey::MeetingInvitation,
        LegacyKey::OrgInvitation,
        LegacyKey::MeetingNotification,
        LegacyKey::DevplatAccess,
    ];

    /// The contract's key name (`x-cfmeet-legacy-idempotency-keys`).
    pub fn name(self) -> &'static str {
        match self {
            LegacyKey::OtpVerify => "otp-verify",
            LegacyKey::OtpReset => "otp-reset",
            LegacyKey::OtpSignIn => "otp-signin",
            LegacyKey::OtpEmailChange => "otp-emailchange",
            LegacyKey::OtpStepUp => "otp-stepup",
            LegacyKey::DeviceStepUp => "device-step-up",
            LegacyKey::PasswordChanged => "password-changed",
            LegacyKey::DeviceLinked => "device-linked",
            LegacyKey::MeetingInvitation => "meeting-invitation",
            LegacyKey::OrgInvitation => "org-invitation",
            LegacyKey::MeetingNotification => "meeting-notification",
            LegacyKey::DevplatAccess => "devplat-access",
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            LegacyKey::OtpVerify => "otp:verify:",
            LegacyKey::OtpReset => "otp:reset:",
            LegacyKey::OtpSignIn => "otp:signin:",
            LegacyKey::OtpEmailChange => "otp:emailchange:",
            LegacyKey::OtpStepUp => "otp:stepup:",
            LegacyKey::DeviceStepUp => "device-step-up:",
            LegacyKey::PasswordChanged => "password-changed:",
            LegacyKey::DeviceLinked => "device-linked:",
            LegacyKey::MeetingInvitation => "meeting-invitation:",
            LegacyKey::OrgInvitation => "org-invitation:",
            LegacyKey::MeetingNotification => "meeting-notification:",
            LegacyKey::DevplatAccess => "devplat-access:",
        }
    }

    /// Whether `key` has this format (the same rule as the contract's pattern).
    pub fn matches(self, key: &str) -> bool {
        let Some(rest) = key.strip_prefix(self.prefix()) else {
            return false;
        };
        match self {
            LegacyKey::DeviceLinked | LegacyKey::MeetingInvitation => uuid_like(rest),
            LegacyKey::OrgInvitation => match rest.split_once(':') {
                Some((id, version)) => {
                    uuid_like(id)
                        && (1..=9).contains(&version.len())
                        && version.bytes().all(|b| b.is_ascii_digit())
                }
                None => false,
            },
            _ => free_suffix(rest),
        }
    }
}

/// The kind of a recipient reference (`<kind>:<id>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientKind {
    /// `cfmeet-user:<account id>`.
    Account,
    /// `meeting-email-principal:<id>`: an address, sealed in the event or resolved by Meeting.
    EmailPrincipal,
}

impl RecipientKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RecipientKind::Account => "cfmeet-user",
            RecipientKind::EmailPrincipal => "meeting-email-principal",
        }
    }

    /// Whether `reference` is of this kind (the id itself is checked separately).
    pub fn matches(self, reference: &str) -> bool {
        reference
            .strip_prefix(self.as_str())
            .is_some_and(|rest| rest.starts_with(':'))
    }
}

/// The closed `purpose` values of `account.step-up-code.requested` (contract
/// `StepUpPurpose`).
pub const STEP_UP_PURPOSES: &[&str] = &["DEACTIVATE_ACCOUNT", "CHANGE_EMAIL"];

/// One catalogue entry.
#[derive(Debug, Clone, Copy)]
pub struct EventSpec {
    pub number: u8,
    pub event_type: &'static str,
    /// `None` only for the control event (any of [`CONTROL_PRODUCERS`]).
    pub producer: Option<Producer>,
    pub priority: Priority,
    /// `None` only for the control event.
    pub category: Option<&'static str>,
    pub subject_kind: Option<&'static str>,
    /// Fields the sealed part must contain (empty: `sealed` is refused).
    pub sealed: &'static [SealedField],
    pub idempotency_key: Option<LegacyKey>,
    pub optional: bool,
    pub required_data: &'static [&'static str],
    pub optional_data: &'static [&'static str],
    /// `Some(kind)`: exactly one recipient, of this kind (contract `recipient`).
    pub recipient: Option<RecipientKind>,
    /// Data keys whose value is a string from a closed set.
    pub closed_data: &'static [(&'static str, &'static [&'static str])],
}

impl EventSpec {
    pub fn lane(&self) -> Lane {
        self.priority.lane()
    }

    pub fn is_control(&self) -> bool {
        self.event_type == CONTROL_EVENT
    }

    pub fn sealed_allowed(&self) -> bool {
        !self.sealed.is_empty()
    }

    pub fn allows_producer(&self, producer: Producer) -> bool {
        match self.producer {
            Some(p) => p == producer,
            None => CONTROL_PRODUCERS.contains(&producer),
        }
    }
}

pub const CONTROL_EVENT: &str = "notifications.cancel";

/// Producers that may publish the control event.
pub const CONTROL_PRODUCERS: &[Producer] = &[
    Producer::MeetingService,
    Producer::Organizations,
    Producer::Communications,
    Producer::Calling,
    Producer::Messaging,
    Producer::DeveloperPlatform,
];

use LegacyKey as K;
use Priority::{Critical, High, Normal};
use Producer::{
    Alertmanager as AM, Calling as CAL, Communications as HUB, DeveloperPlatform as DEV,
    MeetingService as MEET, Messaging as MSG, Organizations as ORG,
};
use SealedField::{Address, Code, Token};

const fn e(
    number: u8,
    event_type: &'static str,
    producer: Producer,
    priority: Priority,
    category: &'static str,
    subject_kind: &'static str,
    required_data: &'static [&'static str],
) -> EventSpec {
    EventSpec {
        number,
        event_type,
        producer: Some(producer),
        priority,
        category: Some(category),
        subject_kind: Some(subject_kind),
        sealed: &[],
        idempotency_key: None,
        optional: false,
        required_data,
        optional_data: &[],
        recipient: None,
        closed_data: &[],
    }
}

const ORG_INV: &[&str] = &["invitationId", "organizationId"];
const MEMBERSHIP: &[&str] = &["membershipId", "organizationId"];
const CONNECTION: &[&str] = &["actorRef", "connectionId"];
const CONFERENCE: &[&str] = &["conferenceId", "startsAt"];
const CREDENTIAL: &[&str] = &["applicationId", "credentialId", "expiresAt"];

/// The catalogue, in contract order (#1–#34, then the control event).
pub const CATALOGUE: &[EventSpec] = &[
    EventSpec {
        optional_data: &["role"],
        ..e(
            1,
            "organizations.invitation.created",
            ORG,
            Normal,
            "ORGANIZATION_UPDATES",
            "ORGANIZATION_INVITATION",
            ORG_INV,
        )
    },
    EventSpec {
        optional_data: &["role"],
        ..e(
            2,
            "organizations.invitation.accepted",
            ORG,
            Normal,
            "ORGANIZATION_UPDATES",
            "ORGANIZATION_INVITATION",
            ORG_INV,
        )
    },
    EventSpec {
        optional_data: &["role"],
        ..e(
            3,
            "organizations.invitation.declined",
            ORG,
            Normal,
            "ORGANIZATION_UPDATES",
            "ORGANIZATION_INVITATION",
            ORG_INV,
        )
    },
    EventSpec {
        optional_data: &["role"],
        ..e(
            4,
            "organizations.invitation.revoked",
            ORG,
            Normal,
            "ORGANIZATION_UPDATES",
            "ORGANIZATION_INVITATION",
            ORG_INV,
        )
    },
    EventSpec {
        optional_data: &["role"],
        ..e(
            5,
            "organizations.invitation.expired",
            ORG,
            Normal,
            "ORGANIZATION_UPDATES",
            "ORGANIZATION_INVITATION",
            ORG_INV,
        )
    },
    e(
        6,
        "organizations.membership.role_changed",
        ORG,
        Normal,
        "ORGANIZATION_UPDATES",
        "ORGANIZATION_MEMBERSHIP",
        &["membershipId", "organizationId", "role"],
    ),
    e(
        7,
        "organizations.membership.suspended",
        ORG,
        Normal,
        "ORGANIZATION_UPDATES",
        "ORGANIZATION_MEMBERSHIP",
        MEMBERSHIP,
    ),
    e(
        8,
        "organizations.membership.removed",
        ORG,
        Normal,
        "ORGANIZATION_UPDATES",
        "ORGANIZATION_MEMBERSHIP",
        MEMBERSHIP,
    ),
    e(
        9,
        "organizations.ownership.transferred",
        ORG,
        Normal,
        "ORGANIZATION_UPDATES",
        "ORGANIZATION",
        &["organizationId"],
    ),
    e(
        10,
        "organizations.organization.suspended",
        ORG,
        Normal,
        "ORGANIZATION_UPDATES",
        "ORGANIZATION",
        &["organizationId"],
    ),
    e(
        11,
        "communications.connection.requested",
        HUB,
        Normal,
        "CONTACT_REQUESTS",
        "CONNECTION",
        CONNECTION,
    ),
    e(
        12,
        "communications.connection.connected",
        HUB,
        Normal,
        "CONTACT_REQUESTS",
        "CONNECTION",
        CONNECTION,
    ),
    e(
        13,
        "messaging.message.created",
        MSG,
        High,
        "MESSAGES",
        "CONVERSATION",
        &[
            "conversationId",
            "conversationKind",
            "eventSequence",
            "securityProfile",
        ],
    ),
    e(
        14,
        "messaging.member.invited",
        MSG,
        Normal,
        "GROUP_INVITATIONS",
        "CONVERSATION",
        &["conversationId", "eventSequence"],
    ),
    e(
        15,
        "calling.call.participant-missed",
        CAL,
        High,
        "MISSED_CALLS",
        "CALL",
        &["callId", "callKind"],
    ),
    e(
        16,
        "calling.voicemail.ready",
        CAL,
        Normal,
        "VOICEMAIL",
        "CALL",
        &["callId", "voiceMessageId"],
    ),
    e(
        17,
        "calling.recording.ready",
        CAL,
        Normal,
        "VOICEMAIL",
        "CALL",
        &["callId", "recordingId"],
    ),
    EventSpec {
        idempotency_key: Some(K::MeetingNotification),
        ..e(
            18,
            "meeting-service.conference.scheduled",
            MEET,
            Normal,
            "MEETING_INVITATIONS",
            "CONFERENCE",
            CONFERENCE,
        )
    },
    EventSpec {
        idempotency_key: Some(K::MeetingNotification),
        ..e(
            19,
            "meeting-service.conference.rescheduled",
            MEET,
            Normal,
            "MEETING_INVITATIONS",
            "CONFERENCE",
            CONFERENCE,
        )
    },
    EventSpec {
        idempotency_key: Some(K::MeetingNotification),
        ..e(
            20,
            "meeting-service.conference.cancelled",
            MEET,
            Normal,
            "MEETING_INVITATIONS",
            "CONFERENCE",
            CONFERENCE,
        )
    },
    EventSpec {
        sealed: &[Token],
        idempotency_key: Some(K::MeetingInvitation),
        ..e(
            21,
            "meeting-service.invitation.email-requested",
            MEET,
            Normal,
            "MEETING_INVITATIONS",
            "MEETING_INVITATION",
            &["conferenceId", "invitationId"],
        )
    },
    EventSpec {
        sealed: &[Token, Address],
        idempotency_key: Some(K::OrgInvitation),
        ..e(
            22,
            "meeting-service.organization-invitation.email-requested",
            MEET,
            Normal,
            "ORGANIZATION_UPDATES",
            "ORGANIZATION_INVITATION",
            ORG_INV,
        )
    },
    EventSpec {
        sealed: &[Code],
        idempotency_key: Some(K::OtpVerify),
        ..e(
            23,
            "account.verification-code.requested",
            MEET,
            Critical,
            "SECURITY",
            "ACCOUNT",
            &["ttlMinutes"],
        )
    },
    EventSpec {
        sealed: &[Code],
        idempotency_key: Some(K::OtpReset),
        ..e(
            24,
            "account.password-reset-code.requested",
            MEET,
            Critical,
            "SECURITY",
            "ACCOUNT",
            &["ttlMinutes"],
        )
    },
    EventSpec {
        sealed: &[Code],
        idempotency_key: Some(K::DeviceStepUp),
        ..e(
            25,
            "account.device-step-up-code.requested",
            MEET,
            Critical,
            "SECURITY",
            "ACCOUNT",
            &["accountDeviceId", "ttlMinutes"],
        )
    },
    EventSpec {
        idempotency_key: Some(K::PasswordChanged),
        ..e(
            26,
            "account.security.password-changed",
            MEET,
            Critical,
            "SECURITY",
            "ACCOUNT",
            &["changedAt"],
        )
    },
    EventSpec {
        idempotency_key: Some(K::DeviceLinked),
        ..e(
            27,
            "account.security.device-linked",
            MEET,
            Critical,
            "SECURITY",
            "ACCOUNT",
            &["accountDeviceId", "linkId"],
        )
    },
    e(
        28,
        "developer-platform.credential.expiring-soon",
        DEV,
        Normal,
        "DEVELOPER_UPDATES",
        "CREDENTIAL",
        CREDENTIAL,
    ),
    e(
        29,
        "developer-platform.credential.expired",
        DEV,
        Normal,
        "DEVELOPER_UPDATES",
        "CREDENTIAL",
        CREDENTIAL,
    ),
    EventSpec {
        idempotency_key: Some(K::DevplatAccess),
        ..e(
            30,
            "developer-platform.production-access.decided",
            DEV,
            Normal,
            "DEVELOPER_UPDATES",
            "PRODUCTION_ACCESS_REQUEST",
            &["decision", "requestId"],
        )
    },
    EventSpec {
        optional: true,
        ..e(
            31,
            "operator.alert.fired",
            AM,
            High,
            "OPERATOR_ALERTS",
            "OPERATOR_ALERT",
            &["alertName", "severity", "status"],
        )
    },
    // Passwordless account codes (contract 1.1.0, PR #97). The sign-in code may be for an
    // address with no account yet and the email-change code goes to an address that is not
    // (yet) the account's: both name one email principal and seal its address.
    EventSpec {
        sealed: &[Code, Token, Address],
        idempotency_key: Some(K::OtpSignIn),
        optional_data: &["requestClient", "requestNetwork"],
        recipient: Some(RecipientKind::EmailPrincipal),
        ..e(
            32,
            "account.sign-in-code.requested",
            MEET,
            Critical,
            "SECURITY",
            "ACCOUNT",
            &["requestedAt", "ttlMinutes"],
        )
    },
    EventSpec {
        sealed: &[Code, Address],
        idempotency_key: Some(K::OtpEmailChange),
        recipient: Some(RecipientKind::EmailPrincipal),
        ..e(
            33,
            "account.email-change-code.requested",
            MEET,
            Critical,
            "SECURITY",
            "ACCOUNT",
            &["ttlMinutes"],
        )
    },
    EventSpec {
        sealed: &[Code],
        idempotency_key: Some(K::OtpStepUp),
        recipient: Some(RecipientKind::Account),
        closed_data: &[("purpose", STEP_UP_PURPOSES)],
        ..e(
            34,
            "account.step-up-code.requested",
            MEET,
            Critical,
            "SECURITY",
            "ACCOUNT",
            &["purpose", "ttlMinutes"],
        )
    },
    EventSpec {
        number: 0,
        event_type: CONTROL_EVENT,
        producer: None,
        priority: Normal,
        category: None,
        subject_kind: None,
        sealed: &[],
        idempotency_key: None,
        optional: false,
        required_data: &[],
        optional_data: &[],
        recipient: None,
        closed_data: &[],
    },
];

/// The catalogue entry for `event_type`, if it exists.
pub fn spec(event_type: &str) -> Option<&'static EventSpec> {
    CATALOGUE.iter().find(|s| s.event_type == event_type)
}
