//! Roles, capabilities and the default-deny authorization decision (contract §3).
//!
//! * A role grants the capabilities of the matrix; anything not listed is denied.
//! * A non-ACTIVE membership grants nothing.
//! * A SUSPENDED or ARCHIVED organization grants nothing except `organization.read` to its
//!   OWNERs (so they can see the status).
//! * ADMINs manage memberships except OWNER / ADMIN targets and never assign OWNER /
//!   ADMIN ([`can_manage_target`], [`can_assign`]).

use serde::Serialize;

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value { $($s => Some($name::$variant),)+ _ => None }
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum!(
    /// Organization role (highest first).
    Role { Owner = "OWNER", Admin = "ADMIN", Manager = "MANAGER", Member = "MEMBER", Guest = "GUEST" }
);
string_enum!(LifecycleStatus { Active = "ACTIVE", Suspended = "SUSPENDED", Archived = "ARCHIVED" });
string_enum!(
    /// Role inside one team (Section 6). Distinct from the organization role: a
    /// TEAM_MANAGER manages that team only, and only while their organization role still
    /// holds `organization.teams.manage`.
    TeamRole { TeamManager = "TEAM_MANAGER", TeamMember = "TEAM_MEMBER" }
);
string_enum!(MembershipStatus { Active = "ACTIVE", Suspended = "SUSPENDED", Removed = "REMOVED" });
string_enum!(
    /// Capabilities of the contract matrix.
    Capability {
        OrganizationRead = "organization.read",
        MembersRead = "organization.members.read",
        ProfileWrite = "organization.profile.write",
        MembersInvite = "organization.members.invite",
        MembersManage = "organization.members.manage",
        OwnershipTransfer = "organization.ownership.transfer",
        LifecycleManage = "organization.lifecycle.manage",
        AuditRead = "organization.audit.read",
        PoliciesManage = "organization.policies.manage",
        TeamsManage = "organization.teams.manage",
        MeetingsCreate = "meetings.organization.create",
        MeetingsManage = "meetings.organization.manage",
        CallingEnterpriseManage = "calling.enterprise.manage",
        MessagingModerate = "messaging.organization.moderate",
    }
);

impl Role {
    /// Rank: OWNER 4 … GUEST 0.
    pub fn rank(&self) -> u8 {
        match self {
            Role::Owner => 4,
            Role::Admin => 3,
            Role::Manager => 2,
            Role::Member => 1,
            Role::Guest => 0,
        }
    }
}

/// The role → capability matrix (contract §3). Default deny.
pub fn role_grants(role: Role, capability: Capability) -> bool {
    use Capability::*;
    use Role::*;
    match capability {
        OrganizationRead => true,
        MembersRead => role != Guest,
        ProfileWrite
        | MembersManage
        | AuditRead
        | PoliciesManage
        | MeetingsManage
        | CallingEnterpriseManage
        | MessagingModerate => matches!(role, Owner | Admin),
        MembersInvite | TeamsManage => matches!(role, Owner | Admin | Manager),
        OwnershipTransfer | LifecycleManage => role == Owner,
        MeetingsCreate => role != Guest,
    }
}

/// Why a capability is (not) granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionReason {
    Allowed,
    NotAMember,
    MembershipSuspended,
    OrganizationSuspended,
    OrganizationArchived,
    CapabilityNotGranted,
    /// The role grants the capability but the organization policy removes it.
    PolicyRestricted,
}

impl DecisionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionReason::Allowed => "ALLOWED",
            DecisionReason::NotAMember => "NOT_A_MEMBER",
            DecisionReason::MembershipSuspended => "MEMBERSHIP_SUSPENDED",
            DecisionReason::OrganizationSuspended => "ORGANIZATION_SUSPENDED",
            DecisionReason::OrganizationArchived => "ORGANIZATION_ARCHIVED",
            DecisionReason::CapabilityNotGranted => "CAPABILITY_NOT_GRANTED",
            DecisionReason::PolicyRestricted => "POLICY_RESTRICTED",
        }
    }

    pub fn allowed(&self) -> bool {
        *self == DecisionReason::Allowed
    }
}

/// The decision for a membership (`None`: no non-removed membership) of an organization in
/// `org_status`.
pub fn evaluate(
    membership: Option<(Role, MembershipStatus)>,
    org_status: LifecycleStatus,
    capability: Capability,
) -> DecisionReason {
    let Some((role, status)) = membership else {
        return DecisionReason::NotAMember;
    };
    match status {
        MembershipStatus::Removed => return DecisionReason::NotAMember,
        MembershipStatus::Suspended => return DecisionReason::MembershipSuspended,
        MembershipStatus::Active => {}
    }
    match org_status {
        LifecycleStatus::Active => {}
        _ if role == Role::Owner && capability == Capability::OrganizationRead => {
            return DecisionReason::Allowed
        }
        LifecycleStatus::Suspended => return DecisionReason::OrganizationSuspended,
        LifecycleStatus::Archived => return DecisionReason::OrganizationArchived,
    }
    if role_grants(role, capability) {
        DecisionReason::Allowed
    } else {
        DecisionReason::CapabilityNotGranted
    }
}

/// [`evaluate`] followed by the organization policy (which can only remove a capability
/// the role holds).
pub fn evaluate_with_policy(
    membership: Option<(Role, MembershipStatus)>,
    org_status: LifecycleStatus,
    capability: Capability,
    policy: &crate::policy::PolicyDocument,
) -> DecisionReason {
    let decision = evaluate(membership, org_status, capability);
    match (decision, membership) {
        (DecisionReason::Allowed, Some((role, _))) if policy.restricts(role, capability) => {
            DecisionReason::PolicyRestricted
        }
        _ => decision,
    }
}

/// Every capability a membership effectively holds now under `policy`.
pub fn effective_capabilities_with_policy(
    role: Role,
    status: MembershipStatus,
    org_status: LifecycleStatus,
    policy: &crate::policy::PolicyDocument,
) -> Vec<Capability> {
    Capability::ALL
        .iter()
        .copied()
        .filter(|c| evaluate_with_policy(Some((role, status)), org_status, *c, policy).allowed())
        .collect()
}

/// Every capability a membership effectively holds now (sorted as the matrix).
pub fn effective_capabilities(
    role: Role,
    status: MembershipStatus,
    org_status: LifecycleStatus,
) -> Vec<Capability> {
    Capability::ALL
        .iter()
        .copied()
        .filter(|c| evaluate(Some((role, status)), org_status, *c).allowed())
        .collect()
}

/// Whether `actor` may change a membership whose current role is `target`
/// (OWNER: anyone; ADMIN: not OWNER / ADMIN targets).
pub fn can_manage_target(actor: Role, target: Role) -> bool {
    match actor {
        Role::Owner => true,
        Role::Admin => target.rank() < Role::Admin.rank(),
        _ => false,
    }
}

/// Whether `actor` may assign `role` to another membership (no escalation beyond the
/// actor's own authority: ADMINs never assign OWNER / ADMIN).
pub fn can_assign(actor: Role, role: Role) -> bool {
    match actor {
        Role::Owner => true,
        Role::Admin => role.rank() < Role::Admin.rank(),
        _ => false,
    }
}

/// Whether `actor` may invite (or revoke an invitation) with `role`. OWNER is never granted
/// by invitation (ownership is transferred or assigned by an OWNER).
pub fn can_invite(actor: Role, role: Role) -> bool {
    role != Role::Owner
        && role.rank() < actor.rank()
        && role_grants(actor, Capability::MembersInvite)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Capability::*;
    use Role::*;

    #[test]
    fn matrix_is_exactly_the_contract_table() {
        // Columns: OWNER ADMIN MANAGER MEMBER GUEST.
        let table: &[(Capability, [bool; 5])] = &[
            (OrganizationRead, [true, true, true, true, true]),
            (MembersRead, [true, true, true, true, false]),
            (ProfileWrite, [true, true, false, false, false]),
            (MembersInvite, [true, true, true, false, false]),
            (MembersManage, [true, true, false, false, false]),
            (OwnershipTransfer, [true, false, false, false, false]),
            (LifecycleManage, [true, false, false, false, false]),
            (AuditRead, [true, true, false, false, false]),
            (PoliciesManage, [true, true, false, false, false]),
            (TeamsManage, [true, true, true, false, false]),
            (MeetingsCreate, [true, true, true, true, false]),
            (MeetingsManage, [true, true, false, false, false]),
            (CallingEnterpriseManage, [true, true, false, false, false]),
            (MessagingModerate, [true, true, false, false, false]),
        ];
        assert_eq!(table.len(), Capability::ALL.len());
        for (cap, row) in table {
            for (role, expected) in [Owner, Admin, Manager, Member, Guest].iter().zip(row) {
                assert_eq!(role_grants(*role, *cap), *expected, "{role} {cap}");
            }
        }
    }

    #[test]
    fn suspended_or_archived_organizations_grant_only_owner_read() {
        for status in [LifecycleStatus::Suspended, LifecycleStatus::Archived] {
            for role in Role::ALL {
                let caps = effective_capabilities(*role, MembershipStatus::Active, status);
                if *role == Owner {
                    assert_eq!(caps, vec![OrganizationRead]);
                } else {
                    assert!(caps.is_empty(), "{role}");
                }
            }
        }
        assert_eq!(
            evaluate(
                Some((Admin, MembershipStatus::Active)),
                LifecycleStatus::Suspended,
                OrganizationRead
            ),
            DecisionReason::OrganizationSuspended
        );
        assert_eq!(
            evaluate(
                Some((Owner, MembershipStatus::Active)),
                LifecycleStatus::Archived,
                ProfileWrite
            ),
            DecisionReason::OrganizationArchived
        );
    }

    #[test]
    fn non_active_memberships_grant_nothing() {
        for role in Role::ALL {
            assert!(effective_capabilities(
                *role,
                MembershipStatus::Suspended,
                LifecycleStatus::Active
            )
            .is_empty());
            assert!(effective_capabilities(
                *role,
                MembershipStatus::Removed,
                LifecycleStatus::Active
            )
            .is_empty());
        }
        assert_eq!(
            evaluate(
                Some((Owner, MembershipStatus::Removed)),
                LifecycleStatus::Active,
                OrganizationRead
            ),
            DecisionReason::NotAMember
        );
        assert_eq!(
            evaluate(None, LifecycleStatus::Active, OrganizationRead),
            DecisionReason::NotAMember
        );
        assert_eq!(
            evaluate(
                Some((Owner, MembershipStatus::Suspended)),
                LifecycleStatus::Active,
                OrganizationRead
            ),
            DecisionReason::MembershipSuspended
        );
        assert_eq!(
            evaluate(
                Some((Guest, MembershipStatus::Active)),
                LifecycleStatus::Active,
                MembersRead
            ),
            DecisionReason::CapabilityNotGranted
        );
    }

    #[test]
    fn admins_never_manage_or_create_owners_or_admins() {
        assert!(can_manage_target(Owner, Owner));
        assert!(can_manage_target(Admin, Manager));
        assert!(!can_manage_target(Admin, Admin));
        assert!(!can_manage_target(Admin, Owner));
        assert!(!can_manage_target(Manager, Member));
        assert!(can_assign(Owner, Owner));
        assert!(!can_assign(Admin, Admin));
        assert!(!can_assign(Admin, Owner));
        assert!(can_assign(Admin, Guest));
        assert!(can_invite(Owner, Admin));
        assert!(!can_invite(Owner, Owner));
        assert!(!can_invite(Admin, Admin));
        assert!(can_invite(Manager, Member));
        assert!(!can_invite(Manager, Manager));
        assert!(!can_invite(Member, Guest));
    }

    #[test]
    fn names_round_trip() {
        for c in Capability::ALL {
            assert_eq!(Capability::parse(c.as_str()), Some(*c));
        }
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(*r));
        }
        assert_eq!(Capability::parse("organization.*"), None);
    }
}
