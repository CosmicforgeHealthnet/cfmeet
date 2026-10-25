//! Part VI workspace roles and the Section 50 permission matrix.
//!
//! | Role | Authority (Section 50 §6.1) |
//! |---|---|
//! | OWNER | tenant ownership, membership / admin changes, plan / billing reference, production-access requests, all developer configuration |
//! | ADMIN | projects, environments, service accounts, credentials, scopes, webhooks, usage configuration; never ownership or membership |
//! | DEVELOPER | project / environment integration, allowed service accounts / credentials, webhooks, SDK / docs, usage reads |
//! | ANALYST | read-only usage, limits, delivery history and activity / audit |
//! | BILLING | billing-account reference, commercial plan visibility, usage export; no communications authority |
//!
//! The matrix is the single authorization table of the human control plane: every
//! handler asks [`allows`] for exactly one [`Action`]. Roles are never inferred from a
//! Platform Access scope, a Meeting Service organization role or a product name.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Owner,
    Admin,
    Developer,
    Analyst,
    Billing,
}

impl Role {
    pub const ALL: &'static [Role] = &[
        Role::Owner,
        Role::Admin,
        Role::Developer,
        Role::Analyst,
        Role::Billing,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Owner => "OWNER",
            Role::Admin => "ADMIN",
            Role::Developer => "DEVELOPER",
            Role::Analyst => "ANALYST",
            Role::Billing => "BILLING",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Role::ALL.iter().copied().find(|r| r.as_str() == value)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Every human control-plane operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    OrganizationRead,
    OrganizationUpdate,
    OrganizationClose,
    MembersRead,
    MembersManage,
    OwnershipTransfer,
    ProjectsRead,
    ProjectsManage,
    EnvironmentsManage,
    WebhooksRead,
    WebhooksManage,
    UsageRead,
    UsageExport,
    LimitsRead,
    ActivityRead,
    CommercialRead,
    BillingAccountManage,
    ProductionAccessRequest,
    CredentialsManage,
    /// Read the service accounts of an environment (OWNER, ADMIN, DEVELOPER).
    ServiceAccountsRead,
    /// Create / resume a service account (OWNER, ADMIN; delegated to Platform Access).
    ServiceAccountsManage,
    /// Suspend a service account (OWNER, ADMIN): withdrawing machine access stays
    /// possible in a RESTRICTED organization.
    ServiceAccountsSuspend,
    /// §54: read API credential metadata (never a secret) — OWNER, ADMIN, DEVELOPER.
    CredentialsRead,
    /// §54: revoke / suspend a credential (withdrawing machine access stays possible in
    /// a RESTRICTED organization) — OWNER, ADMIN, DEVELOPER.
    CredentialsRevoke,
    /// §55: read production-access requests and decisions — every member.
    ProductionAccessRead,
    /// §57: update the support profile's contact references — OWNER, ADMIN.
    SupportProfileManage,
}

impl Action {
    pub const ALL: &'static [Action] = &[
        Action::OrganizationRead,
        Action::OrganizationUpdate,
        Action::OrganizationClose,
        Action::MembersRead,
        Action::MembersManage,
        Action::OwnershipTransfer,
        Action::ProjectsRead,
        Action::ProjectsManage,
        Action::EnvironmentsManage,
        Action::WebhooksRead,
        Action::WebhooksManage,
        Action::UsageRead,
        Action::UsageExport,
        Action::LimitsRead,
        Action::ActivityRead,
        Action::CommercialRead,
        Action::BillingAccountManage,
        Action::ProductionAccessRequest,
        Action::CredentialsManage,
        Action::ServiceAccountsRead,
        Action::ServiceAccountsManage,
        Action::ServiceAccountsSuspend,
        Action::CredentialsRead,
        Action::CredentialsRevoke,
        Action::ProductionAccessRead,
        Action::SupportProfileManage,
    ];

    /// Whether the action changes state (refused for RESTRICTED organizations unless
    /// [`allowed_while_restricted`](Self::allowed_while_restricted)).
    pub fn is_mutation(&self) -> bool {
        !matches!(
            self,
            Action::OrganizationRead
                | Action::MembersRead
                | Action::ProjectsRead
                | Action::WebhooksRead
                | Action::UsageRead
                | Action::UsageExport
                | Action::LimitsRead
                | Action::ActivityRead
                | Action::CommercialRead
                | Action::ServiceAccountsRead
                | Action::CredentialsRead
                | Action::ProductionAccessRead
        )
    }

    /// Mutations that stay available in a RESTRICTED organization: membership hygiene
    /// (removing access is always safe), billing reference updates (to resolve the
    /// restriction), suspending service accounts (withdrawing machine access) and pausing
    /// integrations. Creating projects / environments / service accounts, resuming
    /// service accounts, production requests and new webhooks are disabled.
    pub fn allowed_while_restricted(&self) -> bool {
        !self.is_mutation()
            || matches!(
                self,
                Action::MembersManage
                    | Action::BillingAccountManage
                    | Action::OrganizationClose
                    | Action::ServiceAccountsSuspend
                    | Action::CredentialsRevoke
            )
    }
}

/// The Section 50 permission matrix.
pub fn allows(role: Role, action: Action) -> bool {
    use Action::*;
    use Role::*;
    match action {
        OrganizationRead | MembersRead | ProjectsRead => true,
        OrganizationUpdate => matches!(role, Owner | Admin),
        OrganizationClose | MembersManage | OwnershipTransfer | ProductionAccessRequest => {
            role == Owner
        }
        ProjectsManage | EnvironmentsManage => matches!(role, Owner | Admin),
        // Section 50 §6.1: ADMIN manages service accounts, DEVELOPER sees the allowed ones.
        ServiceAccountsManage | ServiceAccountsSuspend => matches!(role, Owner | Admin),
        ServiceAccountsRead => matches!(role, Owner | Admin | Developer),
        CredentialsManage | WebhooksManage => matches!(role, Owner | Admin | Developer),
        CredentialsRead | CredentialsRevoke => matches!(role, Owner | Admin | Developer),
        ProductionAccessRead => true,
        SupportProfileManage => matches!(role, Owner | Admin),
        WebhooksRead | LimitsRead => matches!(role, Owner | Admin | Developer | Analyst),
        UsageRead | UsageExport => true,
        ActivityRead => matches!(role, Owner | Admin | Analyst),
        CommercialRead => matches!(role, Owner | Admin | Billing),
        BillingAccountManage => matches!(role, Owner | Billing),
    }
}

/// Organization lifecycle (Section 50 §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrganizationState {
    Invited,
    Active,
    Restricted,
    Suspended,
    Closed,
}

impl OrganizationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            OrganizationState::Invited => "INVITED",
            OrganizationState::Active => "ACTIVE",
            OrganizationState::Restricted => "RESTRICTED",
            OrganizationState::Suspended => "SUSPENDED",
            OrganizationState::Closed => "CLOSED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            OrganizationState::Invited,
            OrganizationState::Active,
            OrganizationState::Restricted,
            OrganizationState::Suspended,
            OrganizationState::Closed,
        ]
        .into_iter()
        .find(|s| s.as_str() == value)
    }

    /// Whether Platform Access must hold the tenant SUSPENDED (machine authentication
    /// refused at the gateway and in every service).
    pub fn suspends_machine_access(&self) -> bool {
        matches!(
            self,
            OrganizationState::Suspended | OrganizationState::Closed
        )
    }

    /// Operator-controlled transitions (customers can only close their own organization).
    pub fn operator_may_transition(&self, to: OrganizationState) -> bool {
        use OrganizationState::*;
        matches!(
            (self, to),
            (Invited, Active)
                | (Active, Restricted)
                | (Active, Suspended)
                | (Restricted, Active)
                | (Restricted, Suspended)
                | (Suspended, Active)
                | (Suspended, Restricted)
                | (Invited | Active | Restricted | Suspended, Closed)
        )
    }

    /// Whether `action` may be performed by a member in this state.
    pub fn permits(&self, action: Action) -> bool {
        match self {
            OrganizationState::Active => true,
            OrganizationState::Restricted => action.allowed_while_restricted(),
            // Suspended / closed workspaces stay readable (activity, export) but no
            // mutation is possible; INVITED is not yet usable.
            OrganizationState::Suspended | OrganizationState::Closed => !action.is_mutation(),
            OrganizationState::Invited => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_matrix_is_exactly_section_50() {
        use Action::*;
        use Role::*;
        // OWNER can do everything.
        for a in Action::ALL {
            assert!(allows(Owner, *a), "{a:?}");
        }
        // Only OWNER changes membership, ownership, closure and production access.
        for a in [
            MembersManage,
            OwnershipTransfer,
            OrganizationClose,
            ProductionAccessRequest,
        ] {
            for r in [Admin, Developer, Analyst, Billing] {
                assert!(!allows(r, a), "{r:?} {a:?}");
            }
        }
        // ADMIN: projects, environments, credentials, webhooks; not billing.
        for a in [
            ProjectsManage,
            EnvironmentsManage,
            CredentialsManage,
            WebhooksManage,
        ] {
            assert!(allows(Admin, a));
        }
        assert!(!allows(Admin, BillingAccountManage));
        // Service accounts: OWNER / ADMIN manage, DEVELOPER reads, ANALYST / BILLING nothing.
        for r in [Owner, Admin] {
            for a in [
                ServiceAccountsRead,
                ServiceAccountsManage,
                ServiceAccountsSuspend,
            ] {
                assert!(allows(r, a), "{r:?} {a:?}");
            }
        }
        assert!(allows(Developer, ServiceAccountsRead));
        assert!(!allows(Developer, ServiceAccountsManage));
        assert!(!allows(Developer, ServiceAccountsSuspend));
        for r in [Analyst, Billing] {
            for a in [
                ServiceAccountsRead,
                ServiceAccountsManage,
                ServiceAccountsSuspend,
            ] {
                assert!(!allows(r, a), "{r:?} {a:?}");
            }
        }
        // DEVELOPER integrates but never creates projects.
        assert!(allows(Developer, WebhooksManage) && allows(Developer, CredentialsManage));
        assert!(!allows(Developer, ProjectsManage) && !allows(Developer, ActivityRead));
        // Credentials: OWNER / ADMIN / DEVELOPER; never ANALYST / BILLING.
        for a in [CredentialsRead, CredentialsManage, CredentialsRevoke] {
            for r in [Owner, Admin, Developer] {
                assert!(allows(r, a), "{r:?} {a:?}");
            }
            for r in [Analyst, Billing] {
                assert!(!allows(r, a), "{r:?} {a:?}");
            }
        }
        assert!(allows(Analyst, ProductionAccessRead));
        assert!(!allows(Developer, SupportProfileManage));
        // ANALYST is read-only.
        for a in Action::ALL.iter().filter(|a| a.is_mutation()) {
            assert!(!allows(Analyst, *a), "{a:?}");
        }
        assert!(allows(Analyst, ActivityRead) && allows(Analyst, WebhooksRead));
        // BILLING: billing reference and usage export, no communications authority.
        assert!(allows(Billing, BillingAccountManage) && allows(Billing, UsageExport));
        for a in [
            WebhooksRead,
            WebhooksManage,
            CredentialsManage,
            ProjectsManage,
            LimitsRead,
        ] {
            assert!(!allows(Billing, a), "{a:?}");
        }
    }

    #[test]
    fn lifecycle_states_gate_actions() {
        use OrganizationState::*;
        assert!(Active.permits(Action::ProjectsManage));
        assert!(!Restricted.permits(Action::ProjectsManage));
        assert!(!Restricted.permits(Action::WebhooksManage));
        assert!(Restricted.permits(Action::MembersManage));
        assert!(Restricted.permits(Action::ActivityRead));
        assert!(!Restricted.permits(Action::ServiceAccountsManage));
        assert!(Restricted.permits(Action::ServiceAccountsSuspend));
        assert!(Restricted.permits(Action::ServiceAccountsRead));
        assert!(!Suspended.permits(Action::ServiceAccountsSuspend));
        assert!(Closed.permits(Action::ServiceAccountsRead));
        assert!(!Suspended.permits(Action::MembersManage));
        assert!(Suspended.permits(Action::ActivityRead));
        assert!(!Closed.permits(Action::OrganizationUpdate));
        assert!(Suspended.suspends_machine_access() && Closed.suspends_machine_access());
        assert!(!Restricted.suspends_machine_access());
        assert!(!Closed.operator_may_transition(Active));
        assert!(Suspended.operator_may_transition(Active));
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(*r));
        }
    }
}
