//! Blueprint Section 12 follow-up (closes the Section 8 gap) — organization policy on the
//! Meeting write paths that act on an organization.
//!
//! In `ORGANIZATIONS_AUTHORITY_MODE=authoritative`, `GET /api/v2/me/workspaces/{id}/
//! capabilities` reports what the Organizations Service decides (role matrix under the
//! current policy, `features.*` from the effective policy). The write paths below enforce
//! the SAME decision — they call [`workspaces::require_organization_capability`], which
//! runs the exact evaluation the read returns. There is no second policy evaluator.
//!
//! | Action | Requirement |
//! |---|---|
//! | create a meeting space for an organization (`POST /api/v2/meeting-spaces` with `organizationId`), duplicate one that keeps its organization | organization ACTIVE, `features.meetings`, `meetings.organization.create` |
//! | host admits waiting participants into an organization's meeting | organization ACTIVE, `features.meetings` (evaluated for the acting host; a host who is not a member of the organization is governed by the meeting roles alone) |
//! | join admission that relies on organization membership (`ORGANIZATION_ONLY`, internal waiting-room bypass) | organization ACTIVE, `features.meetings` for the joining member; otherwise the membership grants nothing (other grants — invitation, passcode, public — still apply) |
//!
//! Failure behaviour: a refusal is `403 ORGANIZATION_POLICY_RESTRICTED` (policy or feature),
//! `403 PERMISSION_DENIED` (the role does not hold the capability) or `409
//! ORGANIZATION_SUSPENDED`; Organizations unreachable or an incomplete answer is `503
//! SERVICE_UNAVAILABLE` (never an allow). In `legacy` / `shadow` mode nothing here runs and
//! behaviour is exactly as before.

use std::borrow::Cow;

use uuid::Uuid;

use crate::{
    error::{ApiResult, ErrorCode},
    services::{
        actor::ActorContext,
        meeting_core::{permission_denied, DomainCore},
        organizations_authority::{ActingUser, OrganizationsAuthority},
        workspaces::{self, OrganizationFeature},
    },
};

/// The organization capability that creating an organization meeting needs.
pub const MEETINGS_ORGANIZATION_CREATE: &str = "meetings.organization.create";

/// The Organizations Service, only when it is authoritative.
pub fn authoritative(core: &DomainCore) -> Option<&OrganizationsAuthority> {
    core.organization_authority
        .as_deref()
        .filter(|a| a.is_authoritative())
}

/// Creating (or duplicating into) an organization meeting space. The caller has already
/// been verified as a member of `organization` by the request's organization directory.
pub async fn require_meeting_creation(
    core: &DomainCore,
    actor: &ActorContext,
    organization: Uuid,
) -> ApiResult<()> {
    let Some(authority) = authoritative(core) else {
        return Ok(());
    };
    let Some(user) = actor.registered() else {
        return Err(permission_denied(
            "You are not a verified member of that organization",
        ));
    };
    workspaces::require_organization_capability(
        authority,
        &ActingUser::of(user),
        organization,
        OrganizationFeature::Meetings,
        Some(MEETINGS_ORGANIZATION_CREATE),
    )
    .await
    .map(|_| ())
}

/// A host admitting participants into a meeting space of `organization`. Evaluated for
/// the acting host (Organizations answers policy reads only for members): a host who is
/// not a member of the organization (`ORGANIZATION_NOT_FOUND` from the authority) is
/// governed by the meeting roles alone. The membership is read from Organizations here
/// — not from the request's directory, which is empty while Organizations is down — so an
/// outage is `503`, never an allow.
pub async fn require_meetings_enabled(
    core: &DomainCore,
    actor: &ActorContext,
    organization: Option<Uuid>,
) -> ApiResult<()> {
    let (Some(authority), Some(organization), Some(user)) =
        (authoritative(core), organization, actor.registered())
    else {
        return Ok(());
    };
    match workspaces::require_organization_capability(
        authority,
        &ActingUser::of(user),
        organization,
        OrganizationFeature::Meetings,
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(e) if e.code() == Some(ErrorCode::OrganizationNotFound) => Ok(()),
        Err(e) => Err(e),
    }
}

/// The actor a join is evaluated with: when the joining member's organization membership
/// would count for this space (its organization is among the actor's verified
/// organizations) but the organization does not allow meetings for them, that membership
/// is removed from the actor so it grants nothing. Organizations unreachable → 503.
pub async fn admission_actor<'a>(
    core: &DomainCore,
    actor: &'a ActorContext,
    organization: Option<Uuid>,
) -> ApiResult<Cow<'a, ActorContext>> {
    let (Some(authority), Some(organization), Some(user)) =
        (authoritative(core), organization, actor.registered())
    else {
        return Ok(Cow::Borrowed(actor));
    };
    if !actor.verified_organizations.contains(&organization) {
        return Ok(Cow::Borrowed(actor));
    }
    match workspaces::require_organization_capability(
        authority,
        &ActingUser::of(user),
        organization,
        OrganizationFeature::Meetings,
        None,
    )
    .await
    {
        Ok(_) => Ok(Cow::Borrowed(actor)),
        Err(e)
            if matches!(
                e.code(),
                Some(ErrorCode::ServiceUnavailable | ErrorCode::RateLimited)
            ) =>
        {
            Err(e)
        }
        // Refused (policy, suspended, membership ended between the reads): the
        // organization membership grants nothing for this join.
        Err(_) => {
            let mut narrowed = actor.clone();
            narrowed
                .verified_organizations
                .retain(|o| *o != organization);
            Ok(Cow::Owned(narrowed))
        }
    }
}
