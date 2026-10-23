//! MeetingAccessService — server-side access-policy evaluation (spec §C20–§C22).
//!
//! A meeting link/code only *identifies* a meeting. Access is granted by an explicit
//! source (ownership, membership, invitation) or by the space's access policy, always
//! evaluated against the server-resolved [`ActorContext`]. A valid passcode proves
//! possession of a secret only — it never confers membership, role or ownership.

use sea_orm::EntityTrait;
use serde_json::json;
use uuid::Uuid;

use crate::services::appointment_authorization::AppointmentDecision;
use crate::{
    domain::{AccessPolicy, AdmissionState, Role},
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_participants, meeting_invitations, meeting_members, meeting_spaces},
    services::{
        actor::ActorContext,
        meeting_core::{active_membership, AccessAdapterRequest, AdapterDecision, Core},
        meeting_events::{action, audit, AuditActor, AuditRecord},
        meeting_space::access_config,
        secrets,
    },
};

/// Why access was granted.
#[derive(Debug, Clone, PartialEq)]
pub enum AccessGrant {
    Owner,
    Membership,
    Invitation,
    /// Credential holder already known to this conference (reconnect).
    ExistingParticipant,
    Public,
    Passcode,
    Authenticated,
    Organization,
    Domain,
    Adapter,
}

#[derive(Debug, Clone)]
pub struct AccessDecision {
    pub grant: AccessGrant,
    pub is_owner: bool,
    pub membership: Option<meeting_members::Model>,
    pub invitation: Option<meeting_invitations::Model>,
    pub adapter_role: Option<Role>,
    /// Part H: external (e.g. clinical) authorization context — references only.
    pub external: Option<crate::services::appointment_authorization::ExternalAuthorization>,
}

pub struct AccessRequest<'a> {
    pub space: &'a meeting_spaces::Model,
    pub conference_id: Uuid,
    pub actor: &'a ActorContext,
    pub existing_participant: Option<&'a conference_participants::Model>,
    /// Already validated for this space/conference by the invitation service.
    pub invitation: Option<meeting_invitations::Model>,
    pub passcode: Option<&'a str>,
    /// Opaque context forwarded to external authorities (never proof by itself).
    pub external_context: Option<&'a str>,
}

fn denied(message: &str) -> ApiError {
    ApiError::domain(ErrorCode::MeetingAccessDenied, message)
}

pub struct MeetingAccessService {
    core: Core,
}

impl MeetingAccessService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    fn limiter_key(space_id: Uuid, actor: &ActorContext) -> (String, i64) {
        match actor.user_id() {
            Some(u) => (format!("{space_id}:user:{u}"), 1),
            // Anonymous callers share a per-space bucket with a higher ceiling.
            None => (format!("{space_id}:anon"), 5),
        }
    }

    /// Verify a passcode with rate limiting and failure auditing.
    pub async fn verify_passcode(
        &self,
        space: &meeting_spaces::Model,
        actor: &ActorContext,
        passcode: Option<&str>,
    ) -> ApiResult<()> {
        let Some(hash) = space.passcode_hash.as_deref() else {
            return Err(denied("This meeting is not configured for passcode access"));
        };
        let Some(passcode) = passcode.filter(|p| !p.is_empty()) else {
            return Err(ApiError::domain(
                ErrorCode::MeetingPasscodeRequired,
                "A passcode is required to join this meeting",
            ));
        };
        let (key, multiplier) = Self::limiter_key(space.id, actor);
        let limiter = &self.core.passcode_limiter;
        if limiter.is_blocked(&key, multiplier).await {
            return Err(ApiError::domain(
                ErrorCode::RateLimited,
                "Too many incorrect passcode attempts. Try again later.",
            ));
        }
        if secrets::verify_passcode(passcode, hash) {
            return Ok(());
        }
        limiter.record_failure(&key).await;
        // Auditable, value-free failure record (own transaction: the join rolls back).
        let _ = audit(
            &self.core.db,
            &AuditActor::from(actor),
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: None,
                action: action::PASSCODE_FAILED,
                target_type: Some("MEETING_SPACE"),
                target_ref: Some(space.id.to_string()),
                metadata: json!({ "anonymous": actor.user_id().is_none() }),
            },
        )
        .await;
        Err(ApiError::domain(
            ErrorCode::MeetingPasscodeInvalid,
            "The passcode is incorrect",
        ))
    }

    async fn adapter(&self, req: &AccessRequest<'_>) -> ApiResult<Option<Role>> {
        let cfg = access_config(req.space);
        let adapter_ref = cfg.adapter_ref.unwrap_or_default();
        let adapter = self
            .core
            .adapters
            .get(&adapter_ref)
            .ok_or_else(|| denied("This meeting's access adapter is not available"))?;
        let registered = req.actor.registered();
        let decision = adapter
            .evaluate(&AccessAdapterRequest {
                meeting_space_id: req.space.id,
                conference_id: Some(req.conference_id),
                adapter_ref,
                user_id: registered.map(|r| r.user_id),
                verified_email: registered.and_then(|r| r.verified_email.clone()),
                organizations: req.actor.verified_organizations.clone(),
            })
            .await;
        match decision {
            // Adapters can never mint ownership.
            AdapterDecision::Allow { role } => {
                Ok(role.map(|r| if r == Role::Owner { Role::Host } else { r }))
            }
            AdapterDecision::Deny => Err(denied("You are not authorized to join this meeting")),
        }
    }

    /// Evaluate the access policy. Runs before lock/ban/waiting-room checks: the waiting
    /// room never turns an unauthorized actor into an authorized one.
    pub async fn evaluate(&self, req: AccessRequest<'_>) -> ApiResult<AccessDecision> {
        let space = req.space;
        let registered = req.actor.registered();
        let base = |grant| AccessDecision {
            grant,
            is_owner: false,
            membership: None,
            invitation: None,
            adapter_role: None,
            external: None,
        };

        // Explicit grants.
        if let Some(r) = registered {
            crate::services::meeting_core::enforce_revocation_policy(space, r)?;
            if space.owner_user_id == Some(r.user_id) {
                return Ok(AccessDecision {
                    is_owner: true,
                    ..base(AccessGrant::Owner)
                });
            }
            if let Some(m) = active_membership(&self.core.db, space.id, r).await? {
                return Ok(AccessDecision {
                    membership: Some(m),
                    ..base(AccessGrant::Membership)
                });
            }
        }
        if let Some(inv) = req.invitation {
            return Ok(AccessDecision {
                invitation: Some(inv),
                ..base(AccessGrant::Invitation)
            });
        }

        // Reconnect of a participant already known to this conference. Removal/ban and
        // invitation revocation are re-checked; credential holders (guests) may only
        // reconnect under policies such actors can satisfy.
        if let Some(p) = req.existing_participant {
            if p.admission_state == AdmissionState::Banned {
                return Err(ApiError::domain(
                    ErrorCode::ParticipantBanned,
                    "You are banned from this meeting",
                ));
            }
            if p.admission_state == AdmissionState::Removed {
                return Err(denied("You have been removed from this meeting"));
            }
            if let Some(inv_id) = p.invitation_id {
                let revoked = crate::models::meeting_invitations::Entity::find_by_id(inv_id)
                    .one(&self.core.db)
                    .await?
                    .map(|i| i.status == crate::domain::InvitationStatus::Revoked)
                    .unwrap_or(true);
                if revoked {
                    return Err(ApiError::domain(
                        ErrorCode::InvitationInvalid,
                        "The invitation used to join was revoked",
                    ));
                }
                return Ok(base(AccessGrant::ExistingParticipant));
            }
            if registered.is_none()
                && matches!(
                    space.access_policy,
                    AccessPolicy::Public | AccessPolicy::PublicPasscode
                )
            {
                return Ok(base(AccessGrant::ExistingParticipant));
            }
        }

        let verified_domain = registered.and_then(|r| r.verified_domain());
        let allowed_domains = access_config(space).allowed_domains;
        match policy_step(&PolicyInput {
            policy: space.access_policy,
            registered: registered.is_some(),
            organization_id: space.organization_id,
            verified_organizations: &req.actor.verified_organizations,
            verified_domain: verified_domain.as_deref(),
            allowed_domains: &allowed_domains,
        }) {
            PolicyStep::Grant(grant) => Ok(base(grant)),
            PolicyStep::Deny(e) => Err(e),
            PolicyStep::VerifyPasscode => {
                self.verify_passcode(space, req.actor, req.passcode).await?;
                Ok(base(AccessGrant::Passcode))
            }
            PolicyStep::AppointmentAuthority => {
                // Active appointment authorization is verified server-side by the
                // appointment authority on every join/reconnect (fail closed).
                let decision = self
                    .core
                    .appointments
                    .authorize(
                        req.actor,
                        space,
                        Some(req.conference_id),
                        req.external_context,
                    )
                    .await;
                match decision {
                    AppointmentDecision::Authorized { role, context } => Ok(AccessDecision {
                        adapter_role: role,
                        external: context,
                        ..base(AccessGrant::Adapter)
                    }),
                    AppointmentDecision::Denied { reason } => {
                        tracing::info!(meeting_space_id = %space.id, %reason, "appointment authorization denied");
                        Err(denied(
                            "No active appointment authorizes you to join this meeting",
                        ))
                    }
                }
            }
            PolicyStep::CustomAdapter => {
                let role = self.adapter(&req).await?;
                Ok(AccessDecision {
                    adapter_role: role,
                    ..base(AccessGrant::Adapter)
                })
            }
        }
    }
}

/// Inputs of the space-policy step (after explicit grants: owner, membership,
/// invitation, reconnect). Everything here is server-resolved.
pub struct PolicyInput<'a> {
    pub policy: AccessPolicy,
    pub registered: bool,
    pub organization_id: Option<Uuid>,
    pub verified_organizations: &'a [Uuid],
    pub verified_domain: Option<&'a str>,
    pub allowed_domains: &'a [String],
}

/// Outcome of the space-policy step. Steps that need I/O (passcode verification,
/// appointment authority, custom adapter) are returned for the caller to run.
#[derive(Debug)]
pub enum PolicyStep {
    Grant(AccessGrant),
    Deny(ApiError),
    VerifyPasscode,
    AppointmentAuthority,
    CustomAdapter,
}

/// Pure access-policy decision (deterministic; unit-tested below). Public and
/// passcode spaces never grant a role; organization / domain membership comes only from
/// the server-resolved actor, never from a request.
pub fn policy_step(i: &PolicyInput<'_>) -> PolicyStep {
    match i.policy {
        AccessPolicy::Public => PolicyStep::Grant(AccessGrant::Public),
        AccessPolicy::PublicPasscode => PolicyStep::VerifyPasscode,
        AccessPolicy::Authenticated if i.registered => {
            PolicyStep::Grant(AccessGrant::Authenticated)
        }
        AccessPolicy::Authenticated => PolicyStep::Deny(ApiError::domain(
            ErrorCode::AuthenticationRequired,
            "Sign in to join this meeting",
        )),
        AccessPolicy::InviteOnly => PolicyStep::Deny(denied("This meeting is invite-only")),
        AccessPolicy::OrganizationOnly => {
            if !i.registered {
                return PolicyStep::Deny(ApiError::domain(
                    ErrorCode::AuthenticationRequired,
                    "Sign in with your organization account to join this meeting",
                ));
            }
            match i.organization_id {
                None => PolicyStep::Deny(denied("Organization not configured")),
                Some(org) if i.verified_organizations.contains(&org) => {
                    PolicyStep::Grant(AccessGrant::Organization)
                }
                Some(_) => {
                    PolicyStep::Deny(denied("This meeting is restricted to its organization"))
                }
            }
        }
        AccessPolicy::DomainRestricted => match i.verified_domain {
            Some(d) if i.allowed_domains.iter().any(|a| a.eq_ignore_ascii_case(d)) => {
                PolicyStep::Grant(AccessGrant::Domain)
            }
            _ => PolicyStep::Deny(denied(
                "This meeting is restricted to verified accounts of specific domains",
            )),
        },
        AccessPolicy::HealthcareAppointment => PolicyStep::AppointmentAuthority,
        AccessPolicy::Custom => PolicyStep::CustomAdapter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        policy: AccessPolicy,
        registered: bool,
        orgs: &'a [Uuid],
        org: Option<Uuid>,
        domain: Option<&'a str>,
        allowed: &'a [String],
    ) -> PolicyInput<'a> {
        PolicyInput {
            policy,
            registered,
            organization_id: org,
            verified_organizations: orgs,
            verified_domain: domain,
            allowed_domains: allowed,
        }
    }

    fn code(step: PolicyStep) -> Option<ErrorCode> {
        match step {
            PolicyStep::Deny(e) => e.code(),
            _ => None,
        }
    }

    #[test]
    fn public_and_passcode_never_grant_more_than_entry() {
        assert!(matches!(
            policy_step(&input(AccessPolicy::Public, false, &[], None, None, &[])),
            PolicyStep::Grant(AccessGrant::Public)
        ));
        assert!(matches!(
            policy_step(&input(
                AccessPolicy::PublicPasscode,
                true,
                &[],
                None,
                None,
                &[]
            )),
            PolicyStep::VerifyPasscode
        ));
    }

    #[test]
    fn authenticated_and_invite_only_are_distinct() {
        assert!(matches!(
            policy_step(&input(
                AccessPolicy::Authenticated,
                true,
                &[],
                None,
                None,
                &[]
            )),
            PolicyStep::Grant(AccessGrant::Authenticated)
        ));
        assert_eq!(
            code(policy_step(&input(
                AccessPolicy::Authenticated,
                false,
                &[],
                None,
                None,
                &[]
            ))),
            Some(ErrorCode::AuthenticationRequired)
        );
        for registered in [true, false] {
            assert_eq!(
                code(policy_step(&input(
                    AccessPolicy::InviteOnly,
                    registered,
                    &[],
                    None,
                    None,
                    &[]
                ))),
                Some(ErrorCode::MeetingAccessDenied),
                "invite-only without an invitation is denied even when signed in"
            );
        }
    }

    #[test]
    fn organization_membership_is_server_resolved() {
        let org = Uuid::new_v4();
        let other = Uuid::new_v4();
        assert!(matches!(
            policy_step(&input(
                AccessPolicy::OrganizationOnly,
                true,
                &[org],
                Some(org),
                None,
                &[]
            )),
            PolicyStep::Grant(AccessGrant::Organization)
        ));
        assert_eq!(
            code(policy_step(&input(
                AccessPolicy::OrganizationOnly,
                true,
                &[other],
                Some(org),
                None,
                &[]
            ))),
            Some(ErrorCode::MeetingAccessDenied)
        );
        assert_eq!(
            code(policy_step(&input(
                AccessPolicy::OrganizationOnly,
                false,
                &[org],
                Some(org),
                None,
                &[]
            ))),
            Some(ErrorCode::AuthenticationRequired)
        );
        assert_eq!(
            code(policy_step(&input(
                AccessPolicy::OrganizationOnly,
                true,
                &[org],
                None,
                None,
                &[]
            ))),
            Some(ErrorCode::MeetingAccessDenied),
            "misconfigured organization space fails closed"
        );
    }

    #[test]
    fn domain_restriction_requires_a_verified_domain() {
        let allowed = vec!["cosmicforge.com".to_string()];
        assert!(matches!(
            policy_step(&input(
                AccessPolicy::DomainRestricted,
                true,
                &[],
                None,
                Some("CosmicForge.com"),
                &allowed
            )),
            PolicyStep::Grant(AccessGrant::Domain)
        ));
        for domain in [None, Some("evil.com")] {
            assert_eq!(
                code(policy_step(&input(
                    AccessPolicy::DomainRestricted,
                    true,
                    &[],
                    None,
                    domain,
                    &allowed
                ))),
                Some(ErrorCode::MeetingAccessDenied)
            );
        }
    }

    #[test]
    fn external_authorities_are_delegated_not_granted() {
        assert!(matches!(
            policy_step(&input(
                AccessPolicy::HealthcareAppointment,
                true,
                &[],
                None,
                None,
                &[]
            )),
            PolicyStep::AppointmentAuthority
        ));
        assert!(matches!(
            policy_step(&input(AccessPolicy::Custom, false, &[], None, None, &[])),
            PolicyStep::CustomAdapter
        ));
    }

    #[test]
    fn every_policy_has_a_decision() {
        for p in AccessPolicy::ALL {
            let _ = policy_step(&input(*p, false, &[], None, None, &[]));
            let _ = policy_step(&input(*p, true, &[], None, None, &[]));
        }
    }
}
