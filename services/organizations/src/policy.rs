//! Organization policies (Blueprint Section 6): organization-level constraints only.
//!
//! The document is typed and validated (unknown keys refused). Its defaults are exactly
//! the Section 3 role matrix, so an organization without a stored policy (revision 0)
//! behaves as before. A policy can only REMOVE capabilities a role would otherwise hold
//! ([`PolicyDocument::restricts`]); it never grants one the matrix denies, never touches
//! OWNER administration (an OWNER cannot lock themselves out) and never weakens a stricter
//! owner-service policy: Meeting, Calling and Messaging apply their own policy and the
//! organization's [`PolicyDocument::constraints_json`] together, stricter wins.
//!
//! The legacy Meeting Service `profile_policy` JSON is carried verbatim
//! (`legacyMeetingProfilePolicy`): the Meeting Service still enforces it.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::domain::{Capability, Role};

/// The minimum organization role a tier admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Tier {
    /// MEMBER and above (GUESTs excluded).
    Members,
    /// MANAGER and above.
    Managers,
    /// ADMIN and above.
    Admins,
}

impl Tier {
    pub fn admits(&self, role: Role) -> bool {
        let min = match self {
            Tier::Members => Role::Member,
            Tier::Managers => Role::Manager,
            Tier::Admins => Role::Admin,
        };
        role.rank() >= min.rank()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct MemberDirectory {
    /// Who may list the organization's members (`organization.members.read`).
    pub visibility: Tier,
    /// Whether colleague discovery / search may surface members (a hint for the
    /// Communications Hub, which keeps its own privacy rules; stricter wins).
    pub searchable: bool,
}

impl Default for MemberDirectory {
    fn default() -> Self {
        Self {
            visibility: Tier::Members,
            searchable: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Guests {
    /// Whether GUEST invitations may be created or accepted (external guests).
    pub invitations_allowed: bool,
}

impl Default for Guests {
    fn default() -> Self {
        Self {
            invitations_allowed: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Meetings {
    /// Who may host (create) organization meetings (`meetings.organization.create`).
    pub host_creation: Tier,
}

impl Default for Meetings {
    fn default() -> Self {
        Self {
            host_creation: Tier::Members,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Features {
    pub meetings: bool,
    pub messaging: bool,
    pub calling: bool,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            meetings: true,
            messaging: true,
            calling: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Retention {
    /// A retention HINT in days for organization-scoped records (owner services apply
    /// their own retention; the shorter, legally permitted one wins). `null`: no hint.
    pub hint_days: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct DelegatedAdministration {
    /// MANAGERs may invite (MEMBER / GUEST). `false` removes their
    /// `organization.members.invite`.
    pub managers_may_invite: bool,
    /// MANAGERs may create teams (they become the new team's TEAM_MANAGER).
    pub managers_may_create_teams: bool,
    /// ADMINs may change policies. `false` leaves policies to OWNERs.
    pub admins_may_manage_policies: bool,
}

impl Default for DelegatedAdministration {
    fn default() -> Self {
        Self {
            managers_may_invite: true,
            managers_may_create_teams: false,
            admins_may_manage_policies: true,
        }
    }
}

/// Who may install an app for the whole organization (ADR 0064 workspace installs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InstallTier {
    /// ADMIN and above (the default).
    Admins,
    /// MEMBER and above (GUESTs excluded).
    Members,
}

impl InstallTier {
    pub fn admits(&self, role: Role) -> bool {
        match self {
            InstallTier::Admins => Tier::Admins.admits(role),
            InstallTier::Members => Tier::Members.admits(role),
        }
    }
}

/// Upper bound of `apps.allowList`.
pub const MAX_APP_ALLOW_LIST: usize = 200;

/// ADR 0064 `apps` section (`ORGANIZATIONS_APP_INSTALLS_ENABLED`): who may install apps
/// for the organization and which apps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Apps {
    pub install_tier: InstallTier,
    /// `null`: any app the caller may otherwise install; a list: only these app ids.
    pub allow_list: Option<Vec<uuid::Uuid>>,
    /// Whether members may connect apps to their own accounts (a hint the account
    /// authority shows; personal installs stay the member's own decision).
    pub user_installs_allowed: bool,
}

impl Default for Apps {
    fn default() -> Self {
        Self {
            install_tier: InstallTier::Admins,
            allow_list: None,
            user_installs_allowed: true,
        }
    }
}

/// Why an organization app install is refused (`app-install-check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppInstallRefusal {
    NotAMember,
    RoleRequired,
    NotAllowlisted,
    OrganizationNotReady,
}

impl AppInstallRefusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            AppInstallRefusal::NotAMember => "NOT_A_MEMBER",
            AppInstallRefusal::RoleRequired => "ROLE_REQUIRED",
            AppInstallRefusal::NotAllowlisted => "NOT_ALLOWLISTED",
            AppInstallRefusal::OrganizationNotReady => "ORGANIZATION_NOT_READY",
        }
    }
}

impl Apps {
    /// May a member with `role` install `app` for the organization? (Membership, the
    /// organization's readiness and the membership status are checked by the caller.)
    pub fn install_decision(&self, role: Role, app: uuid::Uuid) -> Result<(), AppInstallRefusal> {
        if !self.install_tier.admits(role) {
            return Err(AppInstallRefusal::RoleRequired);
        }
        if self
            .allow_list
            .as_ref()
            .is_some_and(|list| !list.contains(&app))
        {
            return Err(AppInstallRefusal::NotAllowlisted);
        }
        Ok(())
    }
}

/// The organization policy document (`schemaVersion` 1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct PolicyDocument {
    pub member_directory: MemberDirectory,
    pub guests: Guests,
    pub meetings: Meetings,
    pub features: Features,
    pub retention: Retention,
    pub delegated_administration: DelegatedAdministration,
    /// ADR 0064 (`ORGANIZATIONS_APP_INSTALLS_ENABLED`). Absent (`None`, never serialized)
    /// means the defaults of [`Apps`]; documents without it are exactly as before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apps: Option<Apps>,
}

/// Upper bound of `retention.hintDays` (10 years).
pub const MAX_RETENTION_HINT_DAYS: u32 = 3650;

/// Policy sections only OWNERs may change (they shape who administers).
pub const OWNER_ONLY_SECTIONS: &[&str] = &["delegatedAdministration"];

impl PolicyDocument {
    /// Parses a stored or merged document strictly (unknown keys refused, bounds checked).
    pub fn parse(value: &Value) -> Result<Self, String> {
        let doc: PolicyDocument = serde_json::from_value(value.clone())
            .map_err(|_| "the policy has a missing, unknown or mistyped field".to_string())?;
        if doc
            .retention
            .hint_days
            .is_some_and(|d| d == 0 || d > MAX_RETENTION_HINT_DAYS)
        {
            return Err(format!(
                "retention.hintDays must be between 1 and {MAX_RETENTION_HINT_DAYS}"
            ));
        }
        if let Some(apps) = &doc.apps {
            if let Some(list) = &apps.allow_list {
                if list.len() > MAX_APP_ALLOW_LIST {
                    return Err(format!(
                        "apps.allowList is limited to {MAX_APP_ALLOW_LIST} apps"
                    ));
                }
                let unique: std::collections::BTreeSet<_> = list.iter().collect();
                if unique.len() != list.len() {
                    return Err("apps.allowList must not repeat an app".into());
                }
            }
        }
        Ok(doc)
    }

    /// The effective `apps` section (defaults when absent).
    pub fn apps(&self) -> Apps {
        self.apps.clone().unwrap_or_default()
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("policy serializes")
    }

    /// Whether this policy removes `capability` from `role` (default deny: the role matrix
    /// is evaluated first; this is only ever an additional restriction).
    pub fn restricts(&self, role: Role, capability: Capability) -> bool {
        use Capability::*;
        if role == Role::Owner
            && matches!(
                capability,
                OrganizationRead
                    | MembersRead
                    | ProfileWrite
                    | MembersManage
                    | OwnershipTransfer
                    | LifecycleManage
                    | AuditRead
                    | PoliciesManage
                    | TeamsManage
                    | MembersInvite
            )
        {
            // OWNER administration is never policy-restricted.
            return false;
        }
        match capability {
            MembersRead => !self.member_directory.visibility.admits(role),
            MembersInvite => {
                role == Role::Manager && !self.delegated_administration.managers_may_invite
            }
            PoliciesManage => {
                role == Role::Admin && !self.delegated_administration.admins_may_manage_policies
            }
            MeetingsCreate => !self.features.meetings || !self.meetings.host_creation.admits(role),
            MeetingsManage => !self.features.meetings,
            CallingEnterpriseManage => !self.features.calling,
            MessagingModerate => !self.features.messaging,
            _ => false,
        }
    }

    /// The organization-level constraints owner services combine with their own policy
    /// (stricter wins).
    pub fn constraints_json(&self) -> Value {
        json!({
            "memberDirectory": {
                "visibility": self.member_directory.visibility,
                "searchable": self.member_directory.searchable,
            },
            "externalGuestsAllowed": self.guests.invitations_allowed,
            "meetingHostCreation": self.meetings.host_creation,
            "enabledFeatures": {
                "meetings": self.features.meetings,
                "messaging": self.features.messaging,
                "calling": self.features.calling,
            },
            "retentionHintDays": self.retention.hint_days,
            "combination": "STRICTER_WINS",
        })
    }
}

/// Deep-merges `patch` (a partial document) into `base`: objects merge key by key,
/// anything else replaces. Unknown keys survive the merge and are refused by
/// [`PolicyDocument::parse`].
pub fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// The dotted leaf keys whose values differ between two documents (sorted).
pub fn changed_keys(before: &Value, after: &Value) -> Vec<String> {
    fn leaves(prefix: &str, v: &Value, out: &mut Map<String, Value>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    leaves(&key, v, out);
                }
            }
            other => {
                out.insert(prefix.to_string(), other.clone());
            }
        }
    }
    let (mut a, mut b) = (Map::new(), Map::new());
    leaves("", before, &mut a);
    leaves("", after, &mut b);
    let mut keys: Vec<String> = a
        .keys()
        .chain(b.keys())
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use Capability::*;
    use Role::*;

    #[test]
    fn defaults_restrict_nothing_the_matrix_grants() {
        let p = PolicyDocument::default();
        for role in Role::ALL {
            for cap in Capability::ALL {
                if crate::domain::role_grants(*role, *cap) {
                    assert!(!p.restricts(*role, *cap), "{role} {cap}");
                }
            }
        }
        // The default document round-trips and is complete.
        let v = p.to_json();
        assert_eq!(PolicyDocument::parse(&v).unwrap(), p);
        assert_eq!(PolicyDocument::parse(&json!({})).unwrap(), p);
    }

    #[test]
    fn a_policy_only_removes_capabilities_and_never_from_owner_administration() {
        let strict = PolicyDocument::parse(&json!({
            "memberDirectory": { "visibility": "ADMINS", "searchable": false },
            "guests": { "invitationsAllowed": false },
            "meetings": { "hostCreation": "ADMINS" },
            "features": { "meetings": false, "messaging": false, "calling": false },
            "retention": { "hintDays": 30 },
            "delegatedAdministration": { "managersMayInvite": false, "managersMayCreateTeams": true, "adminsMayManagePolicies": false },
        }))
        .unwrap();
        for role in Role::ALL {
            for cap in Capability::ALL {
                let granted = crate::domain::role_grants(*role, *cap);
                let effective = granted && !strict.restricts(*role, *cap);
                assert!(!effective || granted, "a policy never grants");
            }
        }
        assert!(strict.restricts(Member, MembersRead));
        assert!(strict.restricts(Manager, MembersRead));
        assert!(!strict.restricts(Admin, MembersRead));
        assert!(strict.restricts(Manager, MembersInvite));
        assert!(strict.restricts(Admin, PoliciesManage));
        assert!(!strict.restricts(Owner, PoliciesManage));
        assert!(!strict.restricts(Owner, MembersRead));
        assert!(
            strict.restricts(Owner, MeetingsCreate),
            "feature off for everyone"
        );
        assert!(strict.restricts(Admin, CallingEnterpriseManage));
        let hosts =
            PolicyDocument::parse(&json!({ "meetings": { "hostCreation": "MANAGERS" } })).unwrap();
        assert!(hosts.restricts(Member, MeetingsCreate));
        assert!(!hosts.restricts(Manager, MeetingsCreate));
    }

    #[test]
    fn documents_are_strict_and_bounded() {
        for bad in [
            json!({ "unknown": true }),
            json!({ "features": { "video": true } }),
            json!({ "meetings": { "hostCreation": "EVERYONE" } }),
            json!({ "retention": { "hintDays": 0 } }),
            json!({ "retention": { "hintDays": 4000 } }),
            json!({ "guests": { "invitationsAllowed": "yes" } }),
        ] {
            assert!(PolicyDocument::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_apps_section_defaults_to_admins_and_is_strict() {
        let p = PolicyDocument::default();
        assert!(
            !p.to_json().as_object().unwrap().contains_key("apps"),
            "absent unless stored"
        );
        let apps = p.apps();
        assert_eq!(apps.install_tier, InstallTier::Admins);
        assert_eq!(apps.allow_list, None);
        assert!(apps.user_installs_allowed);
        let app = uuid::Uuid::new_v4();
        assert_eq!(apps.install_decision(Role::Owner, app), Ok(()));
        assert_eq!(apps.install_decision(Role::Admin, app), Ok(()));
        for role in [Role::Manager, Role::Member, Role::Guest] {
            assert_eq!(
                apps.install_decision(role, app),
                Err(AppInstallRefusal::RoleRequired)
            );
        }
        let members = PolicyDocument::parse(&json!({
            "apps": { "installTier": "MEMBERS", "allowList": [app], "userInstallsAllowed": false }
        }))
        .unwrap();
        let a = members.apps();
        assert_eq!(a.install_decision(Role::Member, app), Ok(()));
        assert_eq!(
            a.install_decision(Role::Guest, app),
            Err(AppInstallRefusal::RoleRequired)
        );
        assert_eq!(
            a.install_decision(Role::Owner, uuid::Uuid::new_v4()),
            Err(AppInstallRefusal::NotAllowlisted)
        );
        assert_eq!(
            PolicyDocument::parse(&members.to_json()).unwrap(),
            members,
            "round-trips"
        );
        // A partial section takes the remaining defaults.
        let partial = PolicyDocument::parse(&json!({ "apps": { "allowList": [] } })).unwrap();
        assert_eq!(partial.apps().install_tier, InstallTier::Admins);
        assert_eq!(
            partial.apps().install_decision(Role::Owner, app),
            Err(AppInstallRefusal::NotAllowlisted)
        );
        for bad in [
            json!({ "apps": { "installTier": "EVERYONE" } }),
            json!({ "apps": { "installTier": "MANAGERS" } }),
            json!({ "apps": { "allowList": ["not-a-uuid"] } }),
            json!({ "apps": { "allowList": [app, app] } }),
            json!({ "apps": { "unknown": true } }),
            json!({ "apps": { "userInstallsAllowed": "yes" } }),
        ] {
            assert!(PolicyDocument::parse(&bad).is_err(), "{bad}");
        }
        let too_many: Vec<_> = (0..=MAX_APP_ALLOW_LIST)
            .map(|_| uuid::Uuid::new_v4())
            .collect();
        assert!(PolicyDocument::parse(&json!({ "apps": { "allowList": too_many } })).is_err());
    }

    #[test]
    fn merge_and_changed_keys() {
        let mut base = PolicyDocument::default().to_json();
        let before = base.clone();
        merge(
            &mut base,
            &json!({ "features": { "calling": false }, "retention": { "hintDays": 90 } }),
        );
        let doc = PolicyDocument::parse(&base).unwrap();
        assert!(!doc.features.calling);
        assert!(doc.features.meetings);
        assert_eq!(doc.retention.hint_days, Some(90));
        assert_eq!(
            changed_keys(&before, &base),
            vec![
                "features.calling".to_string(),
                "retention.hintDays".to_string()
            ]
        );
    }
}
