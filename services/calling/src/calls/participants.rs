//! Call participants: logical communication participants (Part C §19).
//!
//! A participant is a trusted principal taking part in a Call — never a connection,
//! device, WebSocket, media-provider participant or telephone leg (those are sessions and
//! legs). It owns the logical identity, role and call history; reconnects and extra
//! devices add sessions under the same participant and never change its authority.
//!
//! Uniqueness rule: a principal is represented by **exactly one** participant per Call for
//! the Call's whole lifetime (`UNIQUE (call_id, principal_id)`). Adding the same principal
//! again is idempotent and returns the existing participant; concurrent additions cannot
//! create duplicates.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{
    attributes::looks_like_contact,
    errors::CallError,
    ids::{CallId, CallParticipantId},
    kinds::ParticipantRole,
};
use crate::{
    domain::{PrincipalId, PrincipalKind},
    models::call_participants,
};

/// Presentation-only display name. Bounded (1–100 characters), whitespace-normalized,
/// free of control / bidirectional-override characters and of telephone-number-like
/// content. It never identifies, authenticates or authorizes anyone and is never used to
/// look anything up: two participants may share a display name and remain two identities.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct DisplayName(String);

impl DisplayName {
    pub const MAX_CHARS: usize = 100;

    pub fn parse(value: &str) -> Result<Self, CallError> {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let chars = normalized.chars().count();
        let bidi_override =
            |c: char| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
        if chars == 0
            || chars > Self::MAX_CHARS
            || normalized
                .chars()
                .any(|c| c.is_control() || bidi_override(c))
        {
            return Err(CallError::Validation(
                "display name must be 1-100 printable characters",
            ));
        }
        // Mirrors ck_call_participants_display_name.
        let digits = normalized.chars().filter(char::is_ascii_digit).count();
        let lower = normalized.to_lowercase();
        if digits >= 7
            || looks_like_contact(&normalized)
            || ["tel:", "sip:", "sips:"].iter().any(|p| lower.contains(p))
        {
            return Err(CallError::Validation(
                "display names must not contain telephone numbers or contact URIs",
            ));
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallParticipant {
    pub id: CallParticipantId,
    pub call_id: CallId,
    pub principal_id: PrincipalId,
    pub principal_kind: PrincipalKind,
    pub role: ParticipantRole,
    pub display_name: Option<DisplayName>,
    pub created_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl From<call_participants::Model> for CallParticipant {
    fn from(m: call_participants::Model) -> Self {
        Self {
            id: CallParticipantId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            principal_id: PrincipalId::from_uuid(m.principal_id),
            principal_kind: m.principal_kind,
            role: m.role,
            display_name: m.display_name.map(DisplayName),
            created_at: m.created_at,
            ended_at: m.ended_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_are_normalized_and_bounded() {
        assert_eq!(
            DisplayName::parse("  Ada   Lovelace ").unwrap().as_str(),
            "Ada Lovelace"
        );
        assert_eq!(
            DisplayName::parse("Zoë Ångström").unwrap().as_str(),
            "Zoë Ångström"
        );
        for bad in [
            "",
            "   ",
            "bad\u{0007}bell",
            "evil\u{202E}txt",
            &"x".repeat(101),
        ] {
            assert!(DisplayName::parse(bad).is_err(), "{bad:?}");
        }
        assert!(DisplayName::parse(&"x".repeat(100)).is_ok());
    }

    #[test]
    fn display_names_never_carry_telephone_numbers() {
        for bad in [
            "+2348012345678",
            "Call me 0801 234 5678",
            "tel:0801",
            "sip:alice@example.org",
            "x +1",
        ] {
            assert!(DisplayName::parse(bad).is_err(), "{bad}");
        }
        assert!(DisplayName::parse("Room 101").is_ok());
    }
}
