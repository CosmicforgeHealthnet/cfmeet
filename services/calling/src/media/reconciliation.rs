//! Call-media reconciliation (Part C §48): the pure comparison between Calling's
//! authoritative sessions and the provider's participants, with the safe repairs it
//! allows. Executing the repairs (and recording their evidence) is the orchestration's
//! job; nothing here creates business authority from provider state.
//!
//! Detected drift → repair:
//!
//! * provider participant with no session of the Call → evict (quarantine);
//! * removed / ended session (or Call without media) still connected → evict;
//! * superseded credential generation → evict;
//! * permission mismatch → re-apply the server-derived permissions;
//! * authorized session connected at the provider but not in Calling → record the join
//!   (opens a connection interval);
//! * session connected in Calling but absent at the provider → record the disconnect.
//!
//! Resource-level drift (resource present for a terminal Call, resource missing for a
//! Call that needs media) is decided by the orchestration from the snapshot's `exists`.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::{
    events::{admission, Admission, EvictionReason, SessionMediaFacts},
    permissions::MediaPermissions,
    provider::ProviderParticipant,
};

/// One session of the Call as Calling's database describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedSession {
    pub session_id: Uuid,
    pub identity: String,
    pub facts: SessionMediaFacts,
    /// Calling has an open connection interval for the session.
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairAction {
    Evict {
        identity: String,
        session_id: Option<Uuid>,
        reason: EvictionReason,
    },
    ApplyPermissions {
        session_id: Uuid,
        identity: String,
        permissions: MediaPermissions,
    },
    RecordJoined {
        session_id: Uuid,
    },
    RecordAbsent {
        session_id: Uuid,
    },
}

/// Plans the repairs that bring the provider back in line with Calling.
pub fn plan(
    expected: &[ExpectedSession],
    participants: &[ProviderParticipant],
) -> Vec<RepairAction> {
    let by_identity: HashMap<&str, &ExpectedSession> =
        expected.iter().map(|e| (e.identity.as_str(), e)).collect();
    let mut present: HashSet<Uuid> = HashSet::new();
    let mut actions = Vec::new();
    for p in participants {
        let session = by_identity.get(p.identity.as_str()).copied();
        match admission(session.map(|s| s.facts), p.generation) {
            Admission::Evict(reason) => actions.push(RepairAction::Evict {
                identity: p.identity.clone(),
                session_id: session.map(|s| s.session_id),
                reason,
            }),
            Admission::Admit(permissions) => {
                let session = session.expect("admitted sessions exist");
                if p.active {
                    present.insert(session.session_id);
                }
                if p.permissions != permissions {
                    actions.push(RepairAction::ApplyPermissions {
                        session_id: session.session_id,
                        identity: p.identity.clone(),
                        permissions,
                    });
                }
                if p.active && !session.connected {
                    actions.push(RepairAction::RecordJoined {
                        session_id: session.session_id,
                    });
                }
            }
        }
    }
    let at_provider: HashSet<&str> = participants.iter().map(|p| p.identity.as_str()).collect();
    for session in expected {
        if session.connected && !at_provider.contains(session.identity.as_str()) {
            actions.push(RepairAction::RecordAbsent {
                session_id: session.session_id,
            });
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls::kinds::CallKind;

    fn expected(id: u128, generation: i32, authorized: bool, connected: bool) -> ExpectedSession {
        ExpectedSession {
            session_id: Uuid::from_u128(id),
            identity: format!("cfmid_{id}"),
            facts: SessionMediaFacts {
                authorized,
                generation,
                permissions: MediaPermissions::default_for(CallKind::Voice),
            },
            connected,
        }
    }

    fn participant(
        id: u128,
        generation: i32,
        permissions: MediaPermissions,
    ) -> ProviderParticipant {
        ProviderParticipant {
            identity: format!("cfmid_{id}"),
            generation: Some(generation),
            permissions,
            active: true,
            published_audio: false,
            published_video: false,
            joined_at: None,
        }
    }

    #[test]
    fn drift_is_detected_and_repaired_safely() {
        let voice = MediaPermissions::default_for(CallKind::Voice);
        let sessions = vec![
            expected(1, 1, true, true),   // in line
            expected(2, 1, false, false), // removed but still connected
            expected(3, 2, true, false),  // admitted at provider with wrong permissions
            expected(4, 1, true, true),   // connected in Calling, gone at provider
            expected(5, 3, true, false),  // connected with a superseded generation
        ];
        let participants = vec![
            participant(1, 1, voice),
            participant(2, 1, voice),
            participant(3, 2, MediaPermissions::NONE),
            participant(5, 2, voice),
            participant(9, 1, voice), // unknown identity
        ];
        let actions = plan(&sessions, &participants);
        assert!(actions.contains(&RepairAction::Evict {
            identity: "cfmid_2".into(),
            session_id: Some(Uuid::from_u128(2)),
            reason: EvictionReason::SessionRevoked
        }));
        assert!(actions.contains(&RepairAction::ApplyPermissions {
            session_id: Uuid::from_u128(3),
            identity: "cfmid_3".into(),
            permissions: voice
        }));
        assert!(actions.contains(&RepairAction::RecordJoined {
            session_id: Uuid::from_u128(3)
        }));
        assert!(actions.contains(&RepairAction::RecordAbsent {
            session_id: Uuid::from_u128(4)
        }));
        assert!(actions.contains(&RepairAction::Evict {
            identity: "cfmid_5".into(),
            session_id: Some(Uuid::from_u128(5)),
            reason: EvictionReason::StaleGeneration
        }));
        assert!(actions.contains(&RepairAction::Evict {
            identity: "cfmid_9".into(),
            session_id: None,
            reason: EvictionReason::UnknownIdentity
        }));
        // Nothing for the session that is in line.
        assert!(!actions.iter().any(|a| matches!(a,
            RepairAction::ApplyPermissions { session_id, .. }
            | RepairAction::RecordJoined { session_id }
            | RepairAction::RecordAbsent { session_id } if *session_id == Uuid::from_u128(1))));
    }
}
