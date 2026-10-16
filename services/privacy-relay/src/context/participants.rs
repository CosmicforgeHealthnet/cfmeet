//! Participant subjects.
//!
//! A subject is EXACTLY ONE opaque reference: a CF Meet principal id
//! (`{"principalId": …}`) or an opaque ContactReference id (`{"contactReferenceId": …}`).
//! Both are UUIDs, so a telephone number, e-mail address or SIP URI can never stand in
//! for a ContactReference. Privacy Relay never resolves, compares or verifies a
//! ContactReference (Contact Vault is a later section).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::{RelayError, RelayResult},
    models::{ParticipantRow, SessionParticipantRow},
    vocabulary::ParticipantSubjectKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SubjectRef {
    Principal(Uuid),
    Contact(Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubjectBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_reference_id: Option<Uuid>,
}

impl SubjectBody {
    pub fn subject(&self) -> RelayResult<SubjectRef> {
        match (self.principal_id, self.contact_reference_id) {
            (Some(p), None) => Ok(SubjectRef::Principal(p)),
            (None, Some(c)) => Ok(SubjectRef::Contact(c)),
            _ => Err(RelayError::Validation(
                "A participant must have exactly one of principalId or contactReferenceId.",
            )),
        }
    }
}

impl From<SubjectRef> for SubjectBody {
    fn from(s: SubjectRef) -> Self {
        match s {
            SubjectRef::Principal(p) => Self {
                principal_id: Some(p),
                contact_reference_id: None,
            },
            SubjectRef::Contact(c) => Self {
                principal_id: None,
                contact_reference_id: Some(c),
            },
        }
    }
}

impl SubjectRef {
    pub fn kind(&self) -> ParticipantSubjectKind {
        match self {
            SubjectRef::Principal(_) => ParticipantSubjectKind::Principal,
            SubjectRef::Contact(_) => ParticipantSubjectKind::ContactReference,
        }
    }

    pub fn principal(&self) -> Option<Uuid> {
        match self {
            SubjectRef::Principal(p) => Some(*p),
            SubjectRef::Contact(_) => None,
        }
    }

    pub fn contact(&self) -> Option<Uuid> {
        match self {
            SubjectRef::Contact(c) => Some(*c),
            SubjectRef::Principal(_) => None,
        }
    }

    pub fn id(&self) -> Uuid {
        match self {
            SubjectRef::Principal(id) | SubjectRef::Contact(id) => *id,
        }
    }

    pub fn of_row(row: &ParticipantRow) -> RelayResult<Self> {
        subject_of(row.principal_ref, row.contact_ref)
    }

    pub fn of_session_row(row: &SessionParticipantRow) -> RelayResult<Self> {
        subject_of(row.principal_ref, row.contact_ref)
    }
}

fn subject_of(principal: Option<Uuid>, contact: Option<Uuid>) -> RelayResult<SubjectRef> {
    match (principal, contact) {
        (Some(p), None) => Ok(SubjectRef::Principal(p)),
        (None, Some(c)) => Ok(SubjectRef::Contact(c)),
        _ => Err(RelayError::Refused(crate::error::ErrorCode::InternalError)),
    }
}

/// Maximum participants of one context.
pub const MAX_PARTICIPANTS: usize = 64;

/// Parses a participant set: bounded, non-duplicated, at least `min` subjects.
pub fn parse_participant_set(bodies: &[SubjectBody], min: usize) -> RelayResult<Vec<SubjectRef>> {
    if bodies.len() < min {
        return Err(RelayError::Validation(if min >= 2 {
            "participants must contain at least two subjects."
        } else {
            "participants must not be empty."
        }));
    }
    if bodies.len() > MAX_PARTICIPANTS {
        return Err(RelayError::Validation(
            "participants contains too many subjects.",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(bodies.len());
    for body in bodies {
        let subject = body.subject()?;
        if !seen.insert(subject) {
            return Err(RelayError::Validation(
                "participants must not contain duplicates.",
            ));
        }
        out.push(subject);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_opaque_reference() {
        let id = Uuid::new_v4();
        let ok: SubjectBody =
            serde_json::from_value(serde_json::json!({ "principalId": id })).unwrap();
        assert_eq!(ok.subject().unwrap(), SubjectRef::Principal(id));
        let both: SubjectBody = serde_json::from_value(
            serde_json::json!({ "principalId": id, "contactReferenceId": id }),
        )
        .unwrap();
        assert!(both.subject().is_err());
        for bad in [
            serde_json::json!({ "phoneNumber": "+2348012345678" }),
            serde_json::json!({ "contactReferenceId": "+2348012345678" }),
            serde_json::json!({ "contactReferenceId": "alice@example.com" }),
            serde_json::json!({ "contactReferenceId": "sip:alice@example.com" }),
            serde_json::json!({ "e164": "+2348012345678" }),
        ] {
            assert!(
                serde_json::from_value::<SubjectBody>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn participant_sets_are_bounded_and_distinct() {
        let a = SubjectBody::from(SubjectRef::Principal(Uuid::new_v4()));
        let b = SubjectBody::from(SubjectRef::Contact(Uuid::new_v4()));
        assert!(parse_participant_set(&[], 1).is_err());
        assert!(parse_participant_set(&[a], 2).is_err());
        assert!(parse_participant_set(&[a, a], 1).is_err());
        assert_eq!(parse_participant_set(&[a, b], 2).unwrap().len(), 2);
    }
}
