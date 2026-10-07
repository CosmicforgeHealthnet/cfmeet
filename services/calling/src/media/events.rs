//! Provider callbacks are evidence, never authority (Part C §45–§47).
//!
//! A verified callback can only confirm or contradict what Calling already decided. The
//! admission decision below is the whole of what a "participant joined" callback may lead
//! to: admit a connection of a current, authorized session generation (apply its
//! server-derived permissions), or evict it. It can never create a participant, a
//! session, an authorization or a permission, and an identity that resolves to no session
//! of the Call is never attached to anyone.

use super::permissions::MediaPermissions;

/// What Calling knows about the session a connection claims to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionMediaFacts {
    /// The session holds media access (AUTHORIZED, not ended, not removed) in a Call
    /// state that permits media.
    pub authorized: bool,
    /// The session's current media generation (the latest credential issued).
    pub generation: i32,
    /// Server-derived effective permissions.
    pub permissions: MediaPermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionReason {
    /// The identity resolves to no session of this Call (quarantined).
    UnknownIdentity,
    /// The session was removed / ended or the Call no longer permits media.
    SessionRevoked,
    /// The connection uses a superseded credential generation.
    StaleGeneration,
}

impl EvictionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            EvictionReason::UnknownIdentity => "UNKNOWN_IDENTITY",
            EvictionReason::SessionRevoked => "SESSION_REVOKED",
            EvictionReason::StaleGeneration => "STALE_GENERATION",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Grant exactly these server-derived permissions to the connection.
    Admit(MediaPermissions),
    Evict(EvictionReason),
}

/// Decides what to do with a provider connection that claims `generation` of a session
/// described by `facts` (`None`: no such session in this Call).
pub fn admission(facts: Option<SessionMediaFacts>, generation: Option<i32>) -> Admission {
    let Some(facts) = facts else {
        return Admission::Evict(EvictionReason::UnknownIdentity);
    };
    if !facts.authorized {
        return Admission::Evict(EvictionReason::SessionRevoked);
    }
    if generation != Some(facts.generation) || facts.generation < 1 {
        return Admission::Evict(EvictionReason::StaleGeneration);
    }
    Admission::Admit(facts.permissions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls::kinds::CallKind;

    #[test]
    fn only_current_authorized_generations_are_admitted() {
        let perms = MediaPermissions::default_for(CallKind::Voice);
        let facts = SessionMediaFacts {
            authorized: true,
            generation: 2,
            permissions: perms,
        };
        assert_eq!(admission(Some(facts), Some(2)), Admission::Admit(perms));
        assert_eq!(
            admission(Some(facts), Some(1)),
            Admission::Evict(EvictionReason::StaleGeneration)
        );
        assert_eq!(
            admission(Some(facts), None),
            Admission::Evict(EvictionReason::StaleGeneration)
        );
        assert_eq!(
            admission(None, Some(2)),
            Admission::Evict(EvictionReason::UnknownIdentity)
        );
        let revoked = SessionMediaFacts {
            authorized: false,
            ..facts
        };
        assert_eq!(
            admission(Some(revoked), Some(2)),
            Admission::Evict(EvictionReason::SessionRevoked)
        );
    }
}
