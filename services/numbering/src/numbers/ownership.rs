//! The ownership policy (Part IV Workstream J §144.1).
//!
//! | Class | Legal owner | Who it routes to |
//! |---|---|---|
//! | PERSONAL | the individual principal | the owner (endpoint, voicemail, approved forwarding) |
//! | PROFESSIONAL | the individual principal | the owner's professional identity / endpoint |
//! | ORGANIZATION | the organization (the trusted tenant) | any enterprise construct |
//! | EMPLOYEE_DIRECT | the organization | one employee (principal) — never the employee's property |
//! | DEPARTMENT | the organization | a department (queue, ring group, IVR, its inbox) |
//! | CAMPAIGN | the organization | a campaign target (queue, IVR, application) |
//! | SHARED_TEAM | the organization | a shared team / queue / shared inbox |
//!
//! * The legal owner comes from TRUSTED context: an organizational number belongs to the
//!   authenticated tenant (a request never names an organization), a personal /
//!   professional number to a principal Platform Access resolves in the caller's
//!   environment.
//! * Targets are opaque CF Meet references; their kinds are bounded per class (a personal
//!   number routes only to its owner; an employee number never routes to an organization
//!   queue while it is the employee's direct line, …).
//! * A transfer keeps the legal owner (same owner kind and reference): a target change of
//!   the same class is a routing revision; a class change between classes of the same
//!   owner kind is a successor assignment. A personal number never becomes employer
//!   property, and an organization's number never becomes a person's: a different legal
//!   owner is refused (release / quarantine / re-acquisition are Workstream Q).

use uuid::Uuid;

use super::{
    routing::RoutingTargets, DomainError, DomainResult, OwnerKind, OwnershipClass, SmsTargetKind,
    VoiceTargetKind,
};

use OwnershipClass as O;
use SmsTargetKind as Sms;
use VoiceTargetKind as V;

/// The voice target kinds a class may route to.
pub fn voice_targets(class: OwnershipClass) -> &'static [VoiceTargetKind] {
    match class {
        O::Personal => &[
            V::None,
            V::Principal,
            V::CallingEndpoint,
            V::Voicemail,
            V::ApprovedForwarding,
        ],
        O::Professional => &[
            V::None,
            V::Principal,
            V::ProfessionalIdentity,
            V::CallingEndpoint,
            V::Voicemail,
            V::ApprovedForwarding,
        ],
        O::EmployeeDirect => &[
            V::None,
            V::Principal,
            V::CallingEndpoint,
            V::EnterpriseExtension,
            V::Voicemail,
        ],
        O::Department => &[
            V::None,
            V::EnterpriseDepartment,
            V::EnterpriseQueue,
            V::EnterpriseRingGroup,
            V::EnterpriseIvr,
            V::Voicemail,
        ],
        O::SharedTeam => &[
            V::None,
            V::SharedTeam,
            V::EnterpriseQueue,
            V::EnterpriseRingGroup,
            V::Voicemail,
        ],
        O::Campaign => &[
            V::None,
            V::EnterpriseQueue,
            V::EnterpriseRingGroup,
            V::EnterpriseIvr,
            V::Voicemail,
            V::ApprovedForwarding,
        ],
        O::Organization => &[
            V::None,
            V::CallingEndpoint,
            V::EnterpriseExtension,
            V::EnterpriseRingGroup,
            V::EnterpriseQueue,
            V::EnterpriseIvr,
            V::EnterpriseDepartment,
            V::SharedTeam,
            V::Voicemail,
            V::ApprovedForwarding,
        ],
    }
}

/// The SMS target kinds a class may route to.
pub fn sms_targets(class: OwnershipClass) -> &'static [SmsTargetKind] {
    match class {
        O::Personal | O::Professional => &[Sms::None, Sms::Principal, Sms::Application],
        O::EmployeeDirect => &[Sms::None, Sms::Principal],
        O::Department => &[
            Sms::None,
            Sms::Department,
            Sms::SharedTeam,
            Sms::Application,
        ],
        O::SharedTeam => &[Sms::None, Sms::SharedTeam, Sms::Application],
        O::Campaign => &[Sms::None, Sms::Application, Sms::SharedTeam],
        O::Organization => &[
            Sms::None,
            Sms::Application,
            Sms::Department,
            Sms::SharedTeam,
        ],
    }
}

/// Validates `targets` for an assignment of `class` owned by `owner_ref`. A personal /
/// professional number's PRINCIPAL targets are its owner; an employee number's voice and
/// SMS principal targets are the same employee.
pub fn validate_targets(
    class: OwnershipClass,
    owner_ref: Uuid,
    targets: &RoutingTargets,
) -> DomainResult<()> {
    targets.validate()?;
    let (voice, voice_ref) = targets.voice;
    let (sms, sms_ref) = targets.sms;
    if !voice_targets(class).contains(&voice) || !sms_targets(class).contains(&sms) {
        return Err(DomainError::precondition(
            "TARGET_NOT_PERMITTED",
            format!("{class} numbers do not route to this target kind"),
        ));
    }
    if class.owner_kind() == OwnerKind::Principal
        && (voice == V::Principal && voice_ref != Some(owner_ref)
            || sms == Sms::Principal && sms_ref != Some(owner_ref))
    {
        return Err(DomainError::precondition(
            "TARGET_NOT_PERMITTED",
            "a personal or professional number routes to its owner only",
        ));
    }
    if class == O::EmployeeDirect
        && voice == V::Principal
        && sms == Sms::Principal
        && voice_ref != sms_ref
    {
        return Err(DomainError::precondition(
            "TARGET_NOT_PERMITTED",
            "an employee's direct number routes to one employee",
        ));
    }
    Ok(())
}

/// The principal an assignment routes to (personal / professional: the owner; employee
/// direct: the employee target), if any.
pub fn routed_principal(
    class: OwnershipClass,
    owner_ref: Uuid,
    targets: &RoutingTargets,
) -> Option<Uuid> {
    match class.owner_kind() {
        OwnerKind::Principal => Some(owner_ref),
        OwnerKind::Organization => match (targets.voice, targets.sms) {
            ((V::Principal, r), _) | (_, (Sms::Principal, r)) if class == O::EmployeeDirect => r,
            _ => None,
        },
    }
}

/// What a transfer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// Same class, same owner: a routing revision.
    Retarget,
    /// Another class of the same legal owner: a successor assignment.
    Reclassify,
}

/// Classifies a transfer; a different legal owner is refused (fail closed).
pub fn classify_transfer(
    from: OwnershipClass,
    from_owner: Uuid,
    to: OwnershipClass,
    to_owner: Uuid,
) -> DomainResult<TransferKind> {
    if from.owner_kind() != to.owner_kind() || from_owner != to_owner {
        return Err(DomainError::precondition(
            "CROSS_OWNER_TRANSFER_UNSUPPORTED",
            "a number never changes legal owner by transfer; release and re-acquisition (Workstream Q) are required",
        ));
    }
    Ok(if from == to {
        TransferKind::Retarget
    } else {
        TransferKind::Reclassify
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(voice: (V, Option<Uuid>), sms: (Sms, Option<Uuid>)) -> RoutingTargets {
        RoutingTargets { voice, sms }
    }

    #[test]
    fn personal_numbers_route_to_their_owner_only() {
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        assert!(validate_targets(
            O::Personal,
            owner,
            &t((V::Principal, Some(owner)), (Sms::Principal, Some(owner)))
        )
        .is_ok());
        for bad in [
            t((V::Principal, Some(other)), (Sms::None, None)),
            t((V::EnterpriseQueue, Some(other)), (Sms::None, None)),
            t((V::None, None), (Sms::Department, Some(other))),
        ] {
            assert_eq!(
                validate_targets(O::Personal, owner, &bad)
                    .unwrap_err()
                    .code(),
                "TARGET_NOT_PERMITTED"
            );
        }
        assert!(validate_targets(
            O::Professional,
            owner,
            &t((V::ProfessionalIdentity, Some(other)), (Sms::None, None))
        )
        .is_ok());
    }

    #[test]
    fn organizational_classes_are_bounded_and_employee_lines_have_one_employee() {
        let org = Uuid::new_v4();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(validate_targets(
            O::EmployeeDirect,
            org,
            &t((V::Principal, Some(a)), (Sms::Principal, Some(a)))
        )
        .is_ok());
        assert!(validate_targets(
            O::EmployeeDirect,
            org,
            &t((V::Principal, Some(a)), (Sms::Principal, Some(b)))
        )
        .is_err());
        assert!(validate_targets(
            O::EmployeeDirect,
            org,
            &t((V::EnterpriseQueue, Some(a)), (Sms::None, None))
        )
        .is_err());
        assert!(validate_targets(
            O::Department,
            org,
            &t(
                (V::EnterpriseDepartment, Some(a)),
                (Sms::Department, Some(a))
            )
        )
        .is_ok());
        assert!(validate_targets(
            O::Department,
            org,
            &t((V::Principal, Some(a)), (Sms::None, None))
        )
        .is_err());
        assert!(validate_targets(
            O::SharedTeam,
            org,
            &t((V::SharedTeam, Some(a)), (Sms::SharedTeam, Some(a)))
        )
        .is_ok());
        assert!(validate_targets(
            O::Campaign,
            org,
            &t((V::EnterpriseIvr, Some(a)), (Sms::Application, Some(b)))
        )
        .is_ok());
        assert_eq!(
            routed_principal(
                O::EmployeeDirect,
                org,
                &t((V::Principal, Some(a)), (Sms::None, None))
            ),
            Some(a)
        );
        assert_eq!(
            routed_principal(
                O::Department,
                org,
                &t((V::EnterpriseDepartment, Some(a)), (Sms::None, None))
            ),
            None
        );
        // Every class allows an unrouted state (offboarding, pending reassignment).
        for class in OwnershipClass::ALL {
            assert!(validate_targets(*class, org, &t((V::None, None), (Sms::None, None))).is_ok());
        }
    }

    #[test]
    fn transfers_keep_the_legal_owner() {
        let (p, org) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            classify_transfer(O::EmployeeDirect, org, O::EmployeeDirect, org).unwrap(),
            TransferKind::Retarget
        );
        assert_eq!(
            classify_transfer(O::EmployeeDirect, org, O::Department, org).unwrap(),
            TransferKind::Reclassify
        );
        assert_eq!(
            classify_transfer(O::Personal, p, O::Professional, p).unwrap(),
            TransferKind::Reclassify
        );
        for (from, fo, to, too) in [
            (O::Personal, p, O::EmployeeDirect, org),
            (O::Organization, org, O::Personal, p),
            (O::Personal, p, O::Personal, Uuid::new_v4()),
            (O::Department, org, O::Department, Uuid::new_v4()),
        ] {
            assert_eq!(
                classify_transfer(from, fo, to, too).unwrap_err().code(),
                "CROSS_OWNER_TRANSFER_UNSUPPORTED"
            );
        }
    }
}
