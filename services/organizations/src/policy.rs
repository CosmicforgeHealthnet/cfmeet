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
        Ok(doc)
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
