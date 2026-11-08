//! Blueprint Section 8 — workspace context (ADR 0056 §5).
//!
//! A workspace is a per-request context: "Personal" or one organization in which the
//! caller holds an ACTIVE membership. Nothing here is stored: there is no "active
//! organization", no switch endpoint and no context token. Every answer is computed from
//! the organization authority on each request, and every organization-scoped operation
//! elsewhere still names its organization and is re-verified by its owner service.
//!
//! Which store answers follows `ORGANIZATIONS_AUTHORITY_MODE`:
//! * `authoritative` — the Organizations Service: the caller's memberships with their
//!   policy-aware effective capabilities (`GET /internal/v1/users/{id}/organizations`),
//!   and for one organization the role matrix under the current policy
//!   (`…/roles/capabilities`) plus the effective constraints (`…/policies/effective`).
//!   A capability the role holds but the policy removes is `POLICY_RESTRICTED`. Any
//!   failure to obtain a complete answer is `SERVICE_UNAVAILABLE`: never a wider answer.
//! * `legacy` / `shadow` — the meeting-service organization directory, with the rule of
//!   `GET /api/v2/me/organizations` (directory-verified ACTIVE memberships of ACTIVE
//!   organizations; shadow mode still serves the legacy answer). Only what the legacy
//!   routes actually implement is granted; everything that needs the Organizations
//!   Service is `FEATURE_DISABLED`. There is no policy and no provisioning there.
//!
//! In both modes a SUSPENDED organization is listed with its status and grants nothing
//! (except what the authority itself still grants its OWNERs: `organization.read`), and an
//! unknown organization, a non-member and a suspended / removed membership all get the
//! same `ORGANIZATION_NOT_FOUND`.

use base64::Engine;
use chrono::Utc;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domain::{OrganizationMemberStatus, OrganizationStatus},
    dto::v2::{
        Workspace, WorkspaceAuthority, WorkspaceCapabilities, WorkspaceCapability,
        WorkspaceCapabilityReason as Reason, WorkspaceCapabilitySummary, WorkspaceFeatures,
        WorkspaceKind, WorkspaceListQuery, WorkspacePage,
    },
    error::{ApiError, ApiResult, ErrorCode},
    models::{organization_memberships, organizations},
    services::{
        actor::ActorContext,
        meeting_core::validation,
        organizations_authority::{
            relayed_error, unavailable, ActingUser, AuthorityError, OrganizationRoleMatrix,
            OrganizationsAuthority,
        },
    },
};

/// The personal workspace id.
pub const PERSONAL: &str = "personal";
/// Display name of the personal workspace.
pub const PERSONAL_DISPLAY_NAME: &str = "Personal";
/// The Communications Hub read that owns personal communications capabilities.
pub const PERSONAL_COMMUNICATIONS_CAPABILITIES: &str = "/api/v2/communications/me/capabilities";

/// Personal capabilities (meeting-service owned).
pub const MEETINGS_PERSONAL_CREATE: &str = "meetings.personal.create";
pub const ORGANIZATIONS_CREATE: &str = "organizations.create";

/// The organization capability vocabulary (`docs/api/organizations-internal-v1.md` §3),
/// used when the legacy directory answers and for suspended organizations. When the
/// Organizations Service answers, its own vocabulary is used.
pub const ORGANIZATION_CAPABILITIES: &[&str] = &[
    "organization.read",
    "organization.members.read",
    "organization.profile.write",
    "organization.members.invite",
    "organization.members.manage",
    "organization.ownership.transfer",
    "organization.lifecycle.manage",
    "organization.audit.read",
    "organization.policies.manage",
    "organization.teams.manage",
    "meetings.organization.create",
    "meetings.organization.manage",
    "calling.enterprise.manage",
    "messaging.organization.moderate",
];

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

/// One organization workspace of the caller, whichever store answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationContext {
    pub organization_id: Uuid,
    pub name: String,
    pub slug: String,
    /// OWNER, ADMIN, MANAGER, MEMBER or GUEST.
    pub role: String,
    /// The organization is ACTIVE (otherwise SUSPENDED).
    pub active: bool,
    pub provisioning_state: Option<String>,
    /// Capabilities granted now (default deny).
    pub granted: Vec<String>,
}

impl OrganizationContext {
    fn status(&self) -> &'static str {
        if self.active {
            "ACTIVE"
        } else {
            "SUSPENDED"
        }
    }
}

fn not_found() -> ApiError {
    ApiError::domain(ErrorCode::OrganizationNotFound, "Organization not found")
}

fn authority_error(e: AuthorityError) -> ApiError {
    match e {
        AuthorityError::Refused(reply) => relayed_error(&reply),
        AuthorityError::Unavailable(_) => unavailable(),
    }
}

// ---------------------------------------------------------------------------------------
// Decisions (pure)
// ---------------------------------------------------------------------------------------

/// The legacy directory's decision: only what the legacy routes implement is granted
/// (`/me/organizations` read, OWNER / ADMIN member list and member changes, organization
/// meetings for verified members); a suspended organization grants nothing.
pub fn legacy_decision(role: &str, active: bool, capability: &str) -> Reason {
    if !active {
        return Reason::OrganizationSuspended;
    }
    match capability {
        "organization.read" | "meetings.organization.create" => Reason::Allowed,
        "organization.members.read" | "organization.members.manage" => {
            if matches!(role, "OWNER" | "ADMIN") {
                Reason::Allowed
            } else {
                Reason::CapabilityNotGranted
            }
        }
        _ => Reason::FeatureDisabled,
    }
}

fn decision(capability: &str, reason: Reason) -> WorkspaceCapability {
    WorkspaceCapability {
        capability: capability.to_string(),
        granted: reason == Reason::Allowed,
        reason,
    }
}

/// Legacy decisions for every capability of the vocabulary.
pub fn legacy_decisions(role: &str, active: bool) -> Vec<WorkspaceCapability> {
    ORGANIZATION_CAPABILITIES
        .iter()
        .map(|c| decision(c, legacy_decision(role, active, c)))
        .collect()
}

/// Decisions for an ACTIVE organization from the Organizations Service: granted only when
/// both the membership's effective capabilities and the role's effective capabilities
/// under the current policy hold it (stricter wins if the two reads disagree); a
/// capability the role matrix grants but the policy removes is `POLICY_RESTRICTED`.
/// `None` when the matrix does not know the caller's role (malformed answer).
pub fn organization_decisions(
    context: &OrganizationContext,
    matrix: &OrganizationRoleMatrix,
) -> Option<Vec<WorkspaceCapability>> {
    let role = matrix.role(&context.role)?;
    Some(
        matrix
            .capabilities
            .iter()
            .map(|c| {
                let reason =
                    if context.granted.contains(c) && role.effective_capabilities.contains(c) {
                        Reason::Allowed
                    } else if role.capabilities.contains(c) {
                        Reason::PolicyRestricted
                    } else {
                        Reason::CapabilityNotGranted
                    };
                decision(c, reason)
            })
            .collect(),
    )
}

/// Decisions for a suspended organization: only what the authority still grants (an
/// OWNER's `organization.read`), everything else `ORGANIZATION_SUSPENDED`.
pub fn suspended_decisions(granted: &[String]) -> Vec<WorkspaceCapability> {
    let mut vocabulary: Vec<String> = ORGANIZATION_CAPABILITIES
        .iter()
        .map(|c| c.to_string())
        .collect();
    for g in granted {
        if !vocabulary.contains(g) {
            vocabulary.push(g.clone());
        }
    }
    vocabulary
        .iter()
        .map(|c| {
            decision(
                c,
                if granted.contains(c) {
                    Reason::Allowed
                } else {
                    Reason::OrganizationSuspended
                },
            )
        })
        .collect()
}

/// Personal decisions: personal meetings for every registered account; self-service
/// organization creation only once the Organizations Service is authoritative.
pub fn personal_decisions(authoritative: bool) -> Vec<WorkspaceCapability> {
    vec![
        decision(MEETINGS_PERSONAL_CREATE, Reason::Allowed),
        decision(
            ORGANIZATIONS_CREATE,
            if authoritative {
                Reason::Allowed
            } else {
                Reason::FeatureDisabled
            },
        ),
    ]
}

fn granted_of(decisions: &[WorkspaceCapability]) -> Vec<String> {
    decisions
        .iter()
        .filter(|d| d.granted)
        .map(|d| d.capability.clone())
        .collect()
}

/// The capability summary of a workspace list item.
pub fn summary(granted: Vec<String>) -> WorkspaceCapabilitySummary {
    let has = |c: &str| granted.iter().any(|g| g == c);
    WorkspaceCapabilitySummary {
        can_create_meetings: has("meetings.organization.create") || has(MEETINGS_PERSONAL_CREATE),
        can_invite_members: has("organization.members.invite"),
        can_administer: has("organization.profile.write")
            || has("organization.members.manage")
            || has("organization.policies.manage"),
        granted,
    }
}

fn authority_kind(authoritative: bool) -> WorkspaceAuthority {
    if authoritative {
        WorkspaceAuthority::OrganizationsService
    } else {
        WorkspaceAuthority::LegacyDirectory
    }
}

pub fn personal_workspace(authoritative: bool) -> Workspace {
    Workspace {
        id: PERSONAL.to_string(),
        kind: WorkspaceKind::Personal,
        display_name: PERSONAL_DISPLAY_NAME.to_string(),
        organization_id: None,
        slug: None,
        role: None,
        organization_status: None,
        provisioning_state: None,
        capabilities: summary(granted_of(&personal_decisions(authoritative))),
    }
}

pub fn organization_workspace(c: &OrganizationContext) -> Workspace {
    Workspace {
        id: c.organization_id.to_string(),
        kind: WorkspaceKind::Organization,
        display_name: c.name.clone(),
        organization_id: Some(c.organization_id),
        slug: Some(c.slug.clone()),
        role: Some(c.role.clone()),
        organization_status: Some(c.status().to_string()),
        provisioning_state: c.provisioning_state.clone(),
        capabilities: summary(if c.active {
            c.granted.clone()
        } else {
            granted_of(&suspended_decisions(&c.granted))
        }),
    }
}

// ---------------------------------------------------------------------------------------
// Paging
// ---------------------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceCursor {
    /// The last organization id returned (nil after the personal workspace alone).
    after: Uuid,
}

fn encode_cursor(after: Uuid) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&WorkspaceCursor { after }).unwrap_or_default())
}

fn decode_cursor(cursor: &str) -> ApiResult<Uuid> {
    if cursor.is_empty() || cursor.len() > 512 {
        return Err(validation("Invalid cursor"));
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|b| serde_json::from_slice::<WorkspaceCursor>(&b).ok())
        .map(|c| c.after)
        .ok_or_else(|| validation("Invalid cursor"))
}

/// One page: PERSONAL first (first page only), then organizations ordered by id.
pub fn paginate(
    personal: Workspace,
    mut organizations: Vec<OrganizationContext>,
    query: &WorkspaceListQuery,
) -> ApiResult<(Vec<Workspace>, Option<String>)> {
    let limit = match query.limit {
        None => DEFAULT_LIMIT,
        Some(l) if (1..=MAX_LIMIT as u32).contains(&l) => l as usize,
        Some(_) => return Err(validation("limit must be between 1 and 200")),
    };
    let after = query.cursor.as_deref().map(decode_cursor).transpose()?;
    organizations.sort_by_key(|c| c.organization_id);
    organizations.dedup_by_key(|c| c.organization_id);
    let mut entries: Vec<(Uuid, Workspace)> = Vec::new();
    if after.is_none() {
        entries.push((Uuid::nil(), personal));
    }
    entries.extend(
        organizations
            .iter()
            .filter(|c| after.is_none_or(|a| c.organization_id > a))
            .map(|c| (c.organization_id, organization_workspace(c))),
    );
    let has_more = entries.len() > limit;
    entries.truncate(limit);
    let next = has_more
        .then(|| entries.last().map(|(key, _)| encode_cursor(*key)))
        .flatten();
    Ok((entries.into_iter().map(|(_, w)| w).collect(), next))
}

// ---------------------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------------------

/// Legacy / shadow: the caller's ACTIVE memberships from the meeting-service directory.
/// An ACTIVE organization counts only when the request's organization directory verified
/// it (the `/me/organizations` rule, failing closed); a SUSPENDED one is listed and grants
/// nothing.
pub async fn legacy_contexts(
    db: &DatabaseConnection,
    actor: &ActorContext,
) -> ApiResult<Vec<OrganizationContext>> {
    let user = actor.require_registered()?;
    let memberships = organization_memberships::Entity::find()
        .filter(organization_memberships::Column::UserId.eq(user.user_id))
        .filter(organization_memberships::Column::Status.eq(OrganizationMemberStatus::Active))
        .all(db)
        .await?;
    if memberships.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<Uuid> = memberships.iter().map(|m| m.organization_id).collect();
    let orgs = organizations::Entity::find()
        .filter(organizations::Column::Id.is_in(ids))
        .all(db)
        .await?;
    let mut out = Vec::with_capacity(memberships.len());
    for m in &memberships {
        let Some(o) = orgs.iter().find(|o| o.id == m.organization_id) else {
            continue;
        };
        let active = match o.status {
            OrganizationStatus::Active => {
                if !actor.verified_organizations.contains(&o.id) {
                    continue;
                }
                true
            }
            OrganizationStatus::Suspended => false,
        };
        let role = m.role.as_str().to_string();
        let granted = granted_of(&legacy_decisions(&role, active));
        out.push(OrganizationContext {
            organization_id: o.id,
            name: o.name.clone(),
            slug: o.slug.clone(),
            role,
            active,
            provisioning_state: None,
            granted,
        });
    }
    Ok(out)
}

/// Authoritative: the caller's ACTIVE memberships of ACTIVE or SUSPENDED organizations
/// (archived organizations and suspended / removed memberships are not workspaces).
pub async fn authoritative_contexts(
    authority: &OrganizationsAuthority,
    acting: &ActingUser,
) -> ApiResult<Vec<OrganizationContext>> {
    let items = authority
        .user_organizations(acting)
        .await
        .map_err(authority_error)?;
    Ok(items
        .into_iter()
        .filter(|i| {
            i.membership_status == "ACTIVE"
                && matches!(i.lifecycle_status.as_str(), "ACTIVE" | "SUSPENDED")
        })
        .map(|i| OrganizationContext {
            organization_id: i.organization_id,
            name: i.name,
            slug: i.slug,
            role: i.role,
            active: i.lifecycle_status == "ACTIVE",
            provisioning_state: i.provisioning_state,
            granted: i.capabilities,
        })
        .collect())
}

// ---------------------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------------------

/// `GET /api/v2/me/workspaces`. `authority` is `Some` only in authoritative mode.
pub async fn list(
    db: &DatabaseConnection,
    authority: Option<&OrganizationsAuthority>,
    actor: &ActorContext,
    query: &WorkspaceListQuery,
) -> ApiResult<WorkspacePage> {
    let user = actor.require_registered()?;
    let organizations = match authority {
        Some(a) => authoritative_contexts(a, &ActingUser::of(user)).await?,
        None => legacy_contexts(db, actor).await?,
    };
    let authoritative = authority.is_some();
    let (items, next_cursor) = paginate(personal_workspace(authoritative), organizations, query)?;
    Ok(WorkspacePage {
        items,
        next_cursor,
        authority: authority_kind(authoritative),
    })
}

/// `GET /api/v2/me/workspaces/{workspaceId}/capabilities`.
pub async fn capabilities(
    db: &DatabaseConnection,
    authority: Option<&OrganizationsAuthority>,
    actor: &ActorContext,
    workspace_id: &str,
) -> ApiResult<WorkspaceCapabilities> {
    let user = actor.require_registered()?;
    let authoritative = authority.is_some();
    let mut answer = WorkspaceCapabilities {
        workspace_id: workspace_id.to_string(),
        kind: WorkspaceKind::Personal,
        authority: authority_kind(authoritative),
        organization_id: None,
        role: None,
        organization_status: None,
        provisioning_state: None,
        policy_revision: None,
        features: WorkspaceFeatures {
            meetings: false,
            messaging: false,
            calling: false,
        },
        capabilities: vec![],
        personal_communications_capabilities: None,
        evaluated_at: Utc::now(),
    };
    if workspace_id == PERSONAL {
        // No organization constraint applies in the personal context; the owner services
        // (Communications Hub for personal communications) decide the rest.
        answer.features = WorkspaceFeatures {
            meetings: true,
            messaging: true,
            calling: true,
        };
        answer.capabilities = personal_decisions(authoritative);
        answer.personal_communications_capabilities =
            Some(PERSONAL_COMMUNICATIONS_CAPABILITIES.to_string());
        return Ok(answer);
    }
    // Malformed, unknown and unauthorized ids all get the same answer.
    let organization = Uuid::try_parse(workspace_id).map_err(|_| not_found())?;
    answer.workspace_id = organization.to_string();
    answer.kind = WorkspaceKind::Organization;
    answer.organization_id = Some(organization);
    let Some(authority) = authority else {
        let context = legacy_contexts(db, actor)
            .await?
            .into_iter()
            .find(|c| c.organization_id == organization)
            .ok_or_else(not_found)?;
        answer.role = Some(context.role.clone());
        answer.organization_status = Some(context.status().to_string());
        answer.features = WorkspaceFeatures {
            meetings: context.active,
            messaging: context.active,
            calling: context.active,
        };
        answer.capabilities = legacy_decisions(&context.role, context.active);
        return Ok(answer);
    };
    authoritative_organization_answer(authority, &ActingUser::of(user), organization, answer).await
}

/// The authoritative answer for one organization (Organizations Service): membership,
/// role matrix under the current policy and the effective constraints. This is THE single
/// evaluation of organization capabilities in meeting-service: the capabilities read
/// returns it and every organization-scoped write path enforces it through
/// [`require_organization_capability`], so the read and the action always agree.
pub async fn authoritative_organization_answer(
    authority: &OrganizationsAuthority,
    acting: &ActingUser,
    organization: Uuid,
    mut answer: WorkspaceCapabilities,
) -> ApiResult<WorkspaceCapabilities> {
    answer.workspace_id = organization.to_string();
    answer.kind = WorkspaceKind::Organization;
    answer.authority = WorkspaceAuthority::OrganizationsService;
    answer.organization_id = Some(organization);
    let context = authoritative_contexts(authority, acting)
        .await?
        .into_iter()
        .find(|c| c.organization_id == organization)
        .ok_or_else(not_found)?;
    answer.role = Some(context.role.clone());
    answer.organization_status = Some(context.status().to_string());
    answer.provisioning_state = context.provisioning_state.clone();
    if !context.active {
        answer.capabilities = suspended_decisions(&context.granted);
        return Ok(answer);
    }
    let (matrix, policy) = tokio::join!(
        authority.role_matrix(organization, acting),
        authority.effective_policy(organization, acting)
    );
    let matrix = matrix.map_err(authority_error)?;
    let policy = policy.map_err(authority_error)?;
    if matrix.lifecycle_status != "ACTIVE" || policy.lifecycle_status != "ACTIVE" {
        // The organization changed status between the reads: grant nothing.
        answer.organization_status = Some(
            [&matrix.lifecycle_status, &policy.lifecycle_status]
                .into_iter()
                .find(|s| s.as_str() != "ACTIVE")
                .cloned()
                .unwrap_or_else(|| "SUSPENDED".to_string()),
        );
        answer.capabilities = suspended_decisions(&[]);
        return Ok(answer);
    }
    answer.capabilities = organization_decisions(&context, &matrix).ok_or_else(unavailable)?;
    answer.policy_revision = Some(policy.revision);
    if policy.provisioning_state.is_some() {
        answer.provisioning_state = policy.provisioning_state.clone();
    }
    answer.features = WorkspaceFeatures {
        meetings: policy.meetings_enabled,
        messaging: policy.messaging_enabled,
        calling: policy.calling_enabled,
    };
    Ok(answer)
}

/// An organization answer that grants nothing yet (filled by the evaluation).
fn empty_answer(organization: Uuid) -> WorkspaceCapabilities {
    WorkspaceCapabilities {
        workspace_id: organization.to_string(),
        kind: WorkspaceKind::Organization,
        authority: WorkspaceAuthority::OrganizationsService,
        organization_id: Some(organization),
        role: None,
        organization_status: None,
        provisioning_state: None,
        policy_revision: None,
        features: WorkspaceFeatures {
            meetings: false,
            messaging: false,
            calling: false,
        },
        capabilities: vec![],
        personal_communications_capabilities: None,
        evaluated_at: Utc::now(),
    }
}

/// The organization feature an action belongs to (`features.*` of the capabilities read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrganizationFeature {
    Meetings,
    Calling,
    Messaging,
}

impl OrganizationFeature {
    fn enabled(&self, features: &WorkspaceFeatures) -> bool {
        match self {
            Self::Meetings => features.meetings,
            Self::Calling => features.calling,
            Self::Messaging => features.messaging,
        }
    }

    fn turned_off(&self) -> &'static str {
        match self {
            Self::Meetings => "Meetings are turned off by your organization",
            Self::Calling => "Calling is turned off by your organization",
            Self::Messaging => "Messaging is turned off by your organization",
        }
    }
}

/// The write-path decision over an answer of [`authoritative_organization_answer`]
/// (pure): the organization must be ACTIVE, the feature on and — when given — the
/// capability granted. Refusals carry stable codes:
/// * `ORGANIZATION_SUSPENDED` (409) — the organization is not ACTIVE;
/// * `ORGANIZATION_POLICY_RESTRICTED` (403) — the feature is off, or the role holds the
///   capability but the organization policy removes it (`POLICY_RESTRICTED` in the read);
/// * `PERMISSION_DENIED` (403) — the role does not hold the capability.
pub fn decide(
    answer: &WorkspaceCapabilities,
    feature: OrganizationFeature,
    capability: Option<&str>,
) -> ApiResult<()> {
    let suspended = || {
        ApiError::domain(
            ErrorCode::OrganizationSuspended,
            "The organization is not active",
        )
    };
    if answer.organization_status.as_deref() != Some("ACTIVE") {
        return Err(suspended());
    }
    if !feature.enabled(&answer.features) {
        return Err(ApiError::domain(
            ErrorCode::OrganizationPolicyRestricted,
            feature.turned_off(),
        ));
    }
    let Some(capability) = capability else {
        return Ok(());
    };
    match answer
        .capabilities
        .iter()
        .find(|d| d.capability == capability)
        .map(|d| d.reason)
    {
        Some(Reason::Allowed) => Ok(()),
        Some(Reason::PolicyRestricted) => Err(ApiError::domain(
            ErrorCode::OrganizationPolicyRestricted,
            "Your organization's policy does not allow this action",
        )),
        Some(Reason::OrganizationSuspended) => Err(suspended()),
        _ => Err(ApiError::domain(
            ErrorCode::PermissionDenied,
            "Your organization role does not allow this action",
        )),
    }
}

/// Enforces the organization decision for one write path in authoritative mode, using
/// exactly the evaluation of the capabilities read. Organizations unreachable (or any
/// incomplete answer) → `503 SERVICE_UNAVAILABLE`, never an allow; an organization the
/// caller cannot see → `404 ORGANIZATION_NOT_FOUND`.
pub async fn require_organization_capability(
    authority: &OrganizationsAuthority,
    acting: &ActingUser,
    organization: Uuid,
    feature: OrganizationFeature,
    capability: Option<&str>,
) -> ApiResult<WorkspaceCapabilities> {
    let answer = authoritative_organization_answer(
        authority,
        acting,
        organization,
        empty_answer(organization),
    )
    .await?;
    decide(&answer, feature, capability)?;
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::organizations_authority::RoleCapabilities;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|c| c.to_string()).collect()
    }

    fn ctx(id: Uuid, role: &str, active: bool, granted: &[&str]) -> OrganizationContext {
        OrganizationContext {
            organization_id: id,
            name: format!("org {id}"),
            slug: "org".into(),
            role: role.into(),
            active,
            provisioning_state: Some("PENDING".into()),
            granted: s(granted),
        }
    }

    fn reason(d: &[WorkspaceCapability], c: &str) -> Reason {
        d.iter().find(|x| x.capability == c).unwrap().reason
    }

    #[test]
    fn legacy_grants_only_what_the_legacy_routes_implement() {
        for role in ["OWNER", "ADMIN"] {
            let d = legacy_decisions(role, true);
            assert_eq!(d.len(), ORGANIZATION_CAPABILITIES.len());
            assert_eq!(
                granted_of(&d),
                s(&[
                    "organization.read",
                    "organization.members.read",
                    "organization.members.manage",
                    "meetings.organization.create"
                ])
            );
            assert_eq!(
                reason(&d, "organization.members.invite"),
                Reason::FeatureDisabled
            );
        }
        let member = legacy_decisions("MEMBER", true);
        assert_eq!(
            granted_of(&member),
            s(&["organization.read", "meetings.organization.create"])
        );
        assert_eq!(
            reason(&member, "organization.members.read"),
            Reason::CapabilityNotGranted
        );
        for role in ["OWNER", "ADMIN", "MEMBER"] {
            let suspended = legacy_decisions(role, false);
            assert!(granted_of(&suspended).is_empty(), "{role}");
            assert!(suspended
                .iter()
                .all(|d| d.reason == Reason::OrganizationSuspended));
        }
    }

    fn matrix(org: Uuid) -> OrganizationRoleMatrix {
        OrganizationRoleMatrix {
            organization_id: org,
            lifecycle_status: "ACTIVE".into(),
            policy_revision: 3,
            capabilities: s(ORGANIZATION_CAPABILITIES),
            roles: vec![
                RoleCapabilities {
                    role: "MANAGER".into(),
                    capabilities: s(&[
                        "organization.read",
                        "organization.members.read",
                        "organization.members.invite",
                        "organization.teams.manage",
                        "meetings.organization.create",
                    ]),
                    // Policy: managersMayInvite = false, meetings.hostCreation = ADMINS.
                    effective_capabilities: s(&[
                        "organization.read",
                        "organization.members.read",
                        "organization.teams.manage",
                    ]),
                },
                RoleCapabilities {
                    role: "GUEST".into(),
                    capabilities: s(&["organization.read"]),
                    effective_capabilities: s(&["organization.read"]),
                },
            ],
        }
    }

    #[test]
    fn policy_restricted_capabilities_are_reported_as_such() {
        let org = Uuid::new_v4();
        let m = matrix(org);
        let c = ctx(
            org,
            "MANAGER",
            true,
            &[
                "organization.read",
                "organization.members.read",
                "organization.teams.manage",
            ],
        );
        let d = organization_decisions(&c, &m).unwrap();
        assert_eq!(d.len(), ORGANIZATION_CAPABILITIES.len());
        assert_eq!(
            reason(&d, "organization.members.invite"),
            Reason::PolicyRestricted
        );
        assert_eq!(
            reason(&d, "meetings.organization.create"),
            Reason::PolicyRestricted
        );
        assert_eq!(reason(&d, "organization.teams.manage"), Reason::Allowed);
        assert_eq!(
            reason(&d, "organization.audit.read"),
            Reason::CapabilityNotGranted
        );
        // Stricter wins: a capability missing from the membership read is not granted.
        let narrower = ctx(org, "MANAGER", true, &["organization.read"]);
        let d = organization_decisions(&narrower, &m).unwrap();
        assert_eq!(granted_of(&d), s(&["organization.read"]));
        // A capability the membership read lists but the role matrix does not is refused.
        let wider = ctx(
            org,
            "GUEST",
            true,
            &["organization.read", "calling.enterprise.manage"],
        );
        let d = organization_decisions(&wider, &m).unwrap();
        assert_eq!(granted_of(&d), s(&["organization.read"]));
        // Unknown role → malformed answer.
        assert!(organization_decisions(&ctx(org, "SUPERUSER", true, &[]), &m).is_none());
    }

    fn answer_with(
        status: &str,
        features: (bool, bool),
        decisions: Vec<WorkspaceCapability>,
    ) -> WorkspaceCapabilities {
        let mut a = empty_answer(Uuid::new_v4());
        a.organization_status = Some(status.into());
        a.features = WorkspaceFeatures {
            meetings: features.0,
            messaging: true,
            calling: features.1,
        };
        a.capabilities = decisions;
        a
    }

    fn code(r: ApiResult<()>) -> Option<ErrorCode> {
        r.err().and_then(|e| e.code())
    }

    #[test]
    fn write_paths_enforce_exactly_the_capabilities_read() {
        let org = Uuid::new_v4();
        let m = matrix(org);
        // MANAGER under "managersMayInvite=false, hostCreation=ADMINS": the read reports
        // meetings.organization.create POLICY_RESTRICTED, so creation is refused.
        let restricted =
            organization_decisions(&ctx(org, "MANAGER", true, &["organization.read"]), &m).unwrap();
        let a = answer_with("ACTIVE", (true, true), restricted);
        assert_eq!(
            code(decide(
                &a,
                OrganizationFeature::Meetings,
                Some("meetings.organization.create")
            )),
            Some(ErrorCode::OrganizationPolicyRestricted)
        );
        // Allowed in the read → allowed on the write path.
        assert!(decide(&a, OrganizationFeature::Meetings, Some("organization.read")).is_ok());
        // Not held by the role at all → PERMISSION_DENIED.
        assert_eq!(
            code(decide(
                &a,
                OrganizationFeature::Calling,
                Some("calling.enterprise.manage")
            )),
            Some(ErrorCode::PermissionDenied)
        );
        // A capability the answer does not list is never granted.
        assert_eq!(
            code(decide(
                &a,
                OrganizationFeature::Meetings,
                Some("unknown.cap")
            )),
            Some(ErrorCode::PermissionDenied)
        );
        // features.meetings = false refuses every meeting action, capability or not.
        let off = answer_with(
            "ACTIVE",
            (false, true),
            vec![decision("meetings.organization.create", Reason::Allowed)],
        );
        assert_eq!(
            code(decide(&off, OrganizationFeature::Meetings, None)),
            Some(ErrorCode::OrganizationPolicyRestricted)
        );
        assert_eq!(
            code(decide(
                &off,
                OrganizationFeature::Meetings,
                Some("meetings.organization.create")
            )),
            Some(ErrorCode::OrganizationPolicyRestricted)
        );
        assert!(decide(&off, OrganizationFeature::Calling, None).is_ok());
        // Calling feature off refuses enterprise administration.
        let calling_off = answer_with(
            "ACTIVE",
            (true, false),
            vec![decision("calling.enterprise.manage", Reason::Allowed)],
        );
        assert_eq!(
            code(decide(
                &calling_off,
                OrganizationFeature::Calling,
                Some("calling.enterprise.manage")
            )),
            Some(ErrorCode::OrganizationPolicyRestricted)
        );
        // A suspended organization (or one that changed status between the reads).
        for status in ["SUSPENDED", "ARCHIVED"] {
            let s = answer_with(status, (true, true), suspended_decisions(&[]));
            assert_eq!(
                code(decide(&s, OrganizationFeature::Meetings, None)),
                Some(ErrorCode::OrganizationSuspended)
            );
        }
        let unknown = empty_answer(org);
        assert_eq!(
            code(decide(&unknown, OrganizationFeature::Meetings, None)),
            Some(ErrorCode::OrganizationSuspended)
        );
    }

    #[test]
    fn suspended_organizations_grant_only_what_the_authority_still_grants() {
        let none = suspended_decisions(&[]);
        assert!(granted_of(&none).is_empty());
        assert!(none
            .iter()
            .all(|d| d.reason == Reason::OrganizationSuspended));
        let owner = suspended_decisions(&s(&["organization.read"]));
        assert_eq!(granted_of(&owner), s(&["organization.read"]));
        let c = ctx(Uuid::new_v4(), "ADMIN", false, &[]);
        let w = organization_workspace(&c);
        assert_eq!(w.organization_status.as_deref(), Some("SUSPENDED"));
        assert!(w.capabilities.granted.is_empty());
        assert!(!w.capabilities.can_create_meetings && !w.capabilities.can_administer);
    }

    #[test]
    fn personal_workspace_never_claims_organization_creation_without_the_authority() {
        let legacy = personal_workspace(false);
        assert_eq!(legacy.id, PERSONAL);
        assert_eq!(legacy.kind, WorkspaceKind::Personal);
        assert_eq!(legacy.capabilities.granted, s(&[MEETINGS_PERSONAL_CREATE]));
        assert!(legacy.capabilities.can_create_meetings);
        let live = personal_workspace(true);
        assert_eq!(
            live.capabilities.granted,
            s(&[MEETINGS_PERSONAL_CREATE, ORGANIZATIONS_CREATE])
        );
    }

    #[test]
    fn pages_start_with_personal_and_follow_organization_ids() {
        let mut ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        let orgs: Vec<OrganizationContext> = ids
            .iter()
            .map(|id| ctx(*id, "MEMBER", true, &["organization.read"]))
            .collect();
        ids.sort();
        let q = |cursor: Option<String>, limit: Option<u32>| WorkspaceListQuery { cursor, limit };
        let (all, next) =
            paginate(personal_workspace(false), orgs.clone(), &q(None, None)).unwrap();
        assert_eq!(all.len(), 6);
        assert_eq!(next, None);
        assert_eq!(all[0].id, PERSONAL);
        assert_eq!(
            all[1..]
                .iter()
                .map(|w| w.organization_id.unwrap())
                .collect::<Vec<_>>(),
            ids
        );

        let mut seen = vec![];
        let mut cursor = None;
        loop {
            let (page, next) =
                paginate(personal_workspace(false), orgs.clone(), &q(cursor, Some(2))).unwrap();
            assert!(page.len() <= 2);
            seen.extend(page.into_iter().map(|w| w.id));
            match next {
                Some(n) => cursor = Some(n),
                None => break,
            }
        }
        assert_eq!(seen.len(), 6);
        assert_eq!(seen[0], PERSONAL);

        for bad in [Some(0), Some(201)] {
            assert!(paginate(personal_workspace(false), vec![], &q(None, bad)).is_err());
        }
        for bad in ["", "not-a-cursor", "eyJ4IjoxfQ"] {
            assert!(
                paginate(
                    personal_workspace(false),
                    vec![],
                    &q(Some(bad.into()), None)
                )
                .is_err(),
                "{bad}"
            );
        }
    }
}
