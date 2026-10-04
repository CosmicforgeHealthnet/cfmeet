//! Phase 2 meeting-domain value types.
//!
//! Every enum here is persisted as `TEXT` (with a matching `CHECK` constraint in
//! `m20261003_000001_phase2_meeting_domain`) and serialized as SCREAMING_SNAKE_CASE
//! in the `/api/v2` contract.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum, Serialize, Deserialize, ToSchema)]
        #[sea_orm(rs_type = "String", db_type = "Text")]
        pub enum $name {
            $(
                #[sea_orm(string_value = $s)]
                #[serde(rename = $s)]
                $variant,
            )+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value { $($s => Some($name::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(MeetingType {
    Standard = "STANDARD",
    Healthcare = "HEALTHCARE",
    Webinar = "WEBINAR",
    TownHall = "TOWN_HALL",
    Training = "TRAINING",
});

text_enum!(SpaceStatus {
    Active = "ACTIVE",
    Archived = "ARCHIVED",
    Deleted = "DELETED",
});

text_enum!(AccessPolicy {
    Public = "PUBLIC",
    PublicPasscode = "PUBLIC_PASSCODE",
    Authenticated = "AUTHENTICATED",
    InviteOnly = "INVITE_ONLY",
    OrganizationOnly = "ORGANIZATION_ONLY",
    DomainRestricted = "DOMAIN_RESTRICTED",
    HealthcareAppointment = "HEALTHCARE_APPOINTMENT",
    Custom = "CUSTOM",
});

text_enum!(WaitingRoomPolicy {
    Off = "OFF",
    Everyone = "EVERYONE",
    GuestsOnly = "GUESTS_ONLY",
    UnauthenticatedOnly = "UNAUTHENTICATED_ONLY",
    NonMembersOnly = "NON_MEMBERS_ONLY",
    ExternalOnly = "EXTERNAL_ONLY",
    Custom = "CUSTOM",
});

text_enum!(SecurityProfile {
    Standard = "STANDARD",
    Business = "BUSINESS",
    Enterprise = "ENTERPRISE",
    Healthcare = "HEALTHCARE",
    HighSecurity = "HIGH_SECURITY",
});

text_enum!(
    /// Recording and transcription policy share one representation.
    MediaCapturePolicy {
        Disabled = "DISABLED",
        HostOptional = "HOST_OPTIONAL",
        AutoStart = "AUTO_START",
        OrganizationControlled = "ORGANIZATION_CONTROLLED",
    }
);

text_enum!(RecordOrigin {
    V2 = "V2",
    Legacy = "LEGACY",
});

text_enum!(ConferenceStatus {
    Created = "CREATED",
    Preparing = "PREPARING",
    Waiting = "WAITING",
    Live = "LIVE",
    Ending = "ENDING",
    Ended = "ENDED",
    Cancelled = "CANCELLED",
    Failed = "FAILED",
});

impl ConferenceStatus {
    /// States from which no new join authority may ever be issued.
    pub fn is_terminal_or_closing(&self) -> bool {
        matches!(
            self,
            Self::Ending | Self::Ended | Self::Cancelled | Self::Failed
        )
    }
}

text_enum!(ConferenceKind {
    Instant = "INSTANT",
    Scheduled = "SCHEDULED",
    Recurring = "RECURRING",
    Legacy = "LEGACY",
});

text_enum!(TerminationReason {
    HostEnded = "HOST_ENDED",
    ScheduledEnd = "SCHEDULED_END",
    Admin = "ADMIN",
    Failure = "FAILURE",
    EmptyTimeout = "EMPTY_TIMEOUT",
    Cancelled = "CANCELLED",
    Legacy = "LEGACY",
});

text_enum!(
    /// Built-in Phase 2 roles, ordered from most to least privileged.
    Role {
        Owner = "OWNER",
        Host = "HOST",
        CoHost = "CO_HOST",
        Moderator = "MODERATOR",
        Presenter = "PRESENTER",
        Participant = "PARTICIPANT",
        Viewer = "VIEWER",
    }
);

impl Role {
    /// Privilege rank used by grant-authority rules (higher = more privileged).
    pub fn rank(&self) -> u8 {
        match self {
            Role::Owner => 7,
            Role::Host => 6,
            Role::CoHost => 5,
            Role::Moderator => 4,
            Role::Presenter => 3,
            Role::Participant => 2,
            Role::Viewer => 1,
        }
    }

    pub fn max(a: Role, b: Role) -> Role {
        if a.rank() >= b.rank() {
            a
        } else {
            b
        }
    }
}

text_enum!(RoleSource {
    Owner = "OWNER",
    Membership = "MEMBERSHIP",
    Invitation = "INVITATION",
    Default = "DEFAULT",
    Assigned = "ASSIGNED",
    Legacy = "LEGACY",
});

text_enum!(IdentityType {
    RegisteredUser = "REGISTERED_USER",
    InvitedExternal = "INVITED_EXTERNAL",
    Guest = "GUEST",
    Phone = "PHONE",
    Service = "SERVICE",
    Bot = "BOT",
});

text_enum!(AdmissionState {
    Waiting = "WAITING",
    Admitted = "ADMITTED",
    Denied = "DENIED",
    Removed = "REMOVED",
    Banned = "BANNED",
});

text_enum!(SessionState {
    Pending = "PENDING",
    Connected = "CONNECTED",
    Disconnected = "DISCONNECTED",
    Replaced = "REPLACED",
    Terminated = "TERMINATED",
    Expired = "EXPIRED",
    Historical = "HISTORICAL",
});

impl SessionState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Pending | Self::Connected)
    }
}

text_enum!(PrincipalType {
    User = "USER",
    Email = "EMAIL",
});

text_enum!(MemberStatus {
    Active = "ACTIVE",
    Revoked = "REVOKED",
});

text_enum!(InvitationStatus {
    Active = "ACTIVE",
    Revoked = "REVOKED",
});

text_enum!(BanScope {
    Space = "SPACE",
    Conference = "CONFERENCE",
});

text_enum!(BanSubjectType {
    User = "USER",
    External = "EXTERNAL",
    Guest = "GUEST",
});

text_enum!(RecurrenceStatus {
    Active = "ACTIVE",
    Ended = "ENDED",
});

text_enum!(OutboxStatus {
    Pending = "PENDING",
    Published = "PUBLISHED",
    Failed = "FAILED",
});

text_enum!(
    /// Client realtime events vs internal provider-control commands (never sent to clients).
    OutboxChannel {
        Realtime = "REALTIME",
        ProviderCommand = "PROVIDER_COMMAND",
    }
);

text_enum!(OrganizationStatus {
    Active = "ACTIVE",
    Suspended = "SUSPENDED",
});

text_enum!(OrganizationRole {
    Owner = "OWNER",
    Admin = "ADMIN",
    Member = "MEMBER",
});

text_enum!(OrganizationMemberStatus {
    Active = "ACTIVE",
    Suspended = "SUSPENDED",
    Removed = "REMOVED",
});

text_enum!(RecurrenceExceptionKind {
    Cancelled = "CANCELLED",
    Rescheduled = "RESCHEDULED",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_ranks_are_strictly_ordered() {
        let ranks: Vec<u8> = Role::ALL.iter().map(Role::rank).collect();
        assert!(ranks.windows(2).all(|w| w[0] > w[1]));
    }

    #[test]
    fn text_enum_roundtrip() {
        for p in AccessPolicy::ALL {
            assert_eq!(AccessPolicy::parse(p.as_str()), Some(*p));
            let json = serde_json::to_string(p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
        }
        assert_eq!(Role::parse("owner"), None);
    }

    #[test]
    fn closing_states_block_join() {
        assert!(ConferenceStatus::Ending.is_terminal_or_closing());
        assert!(ConferenceStatus::Ended.is_terminal_or_closing());
        assert!(ConferenceStatus::Cancelled.is_terminal_or_closing());
        assert!(!ConferenceStatus::Live.is_terminal_or_closing());
    }
}
