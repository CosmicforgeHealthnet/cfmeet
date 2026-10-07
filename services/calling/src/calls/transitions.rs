//! State transition rules (Part C §18): actions, evidence and the pure transition plan.
//!
//! Clients and services never set a state. They request an [`CallAction`]; the target
//! state is derived here. A transition is planned only if
//!
//! 1. the command was built against the persisted version (`expected_version`);
//! 2. the Call is not terminal;
//! 3. the current state is a valid source of the action and the canonical graph allows
//!    `current → target`;
//! 4. the action's evidence is well formed (its facts are then verified against the
//!    database by the service, under the call row lock).
//!
//! Evidence rules:
//!
//! * AUTHORIZED requires the Call's effective APPROVED authorization; a DENIED decision can
//!   only fail the Call (`DenyByPolicy` → FAILED / POLICY). POLICY is reserved for that
//!   path, so every POLICY failure is backed by a DENIED decision.
//! * Every entry into CONNECTED — answer, resume, completed transfer — requires positive
//!   evidence of an established bidirectional media path between sessions / legs of the
//!   Call. ANSWERED (the target accepted) never implies CONNECTED, and "a provider accepted
//!   the request" is not a kind of evidence that exists.
//! * ENDING → ENDED requires completion evidence: confirmed cleanup, verified end of all
//!   participation, or the documented reconciliation-timeout disposition.

use chrono::{DateTime, Utc};

use super::{
    aggregate::Call,
    errors::{CallError, CallResult},
    ids::{CallAuthorizationId, CallLegId, CallSessionId},
    kinds::{FailureCategory, TerminalReason},
    state::CallState::{self, *},
};

/// Positive, provider-independent evidence that a bidirectional media path exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectedEvidence {
    /// Two or more connected application sessions of at least two participants.
    RtcMediaPathEstablished { session_ids: Vec<CallSessionId> },
    /// Two or more connected network legs bridged together.
    NetworkLegEstablished { leg_ids: Vec<CallLegId> },
    /// At least one connected application session bridged to at least one connected leg.
    MixedBridgeEstablished {
        session_ids: Vec<CallSessionId>,
        leg_ids: Vec<CallLegId>,
    },
}

impl ConnectedEvidence {
    /// Upper bound on referenced sessions / legs (keeps evidence and events bounded).
    pub const MAX_REFERENCES: usize = 16;

    pub fn name(&self) -> &'static str {
        match self {
            ConnectedEvidence::RtcMediaPathEstablished { .. } => "RTC_MEDIA_PATH_ESTABLISHED",
            ConnectedEvidence::NetworkLegEstablished { .. } => "NETWORK_LEG_ESTABLISHED",
            ConnectedEvidence::MixedBridgeEstablished { .. } => "MIXED_BRIDGE_ESTABLISHED",
        }
    }

    pub fn session_ids(&self) -> &[CallSessionId] {
        match self {
            ConnectedEvidence::RtcMediaPathEstablished { session_ids }
            | ConnectedEvidence::MixedBridgeEstablished { session_ids, .. } => session_ids,
            ConnectedEvidence::NetworkLegEstablished { .. } => &[],
        }
    }

    pub fn leg_ids(&self) -> &[CallLegId] {
        match self {
            ConnectedEvidence::NetworkLegEstablished { leg_ids }
            | ConnectedEvidence::MixedBridgeEstablished { leg_ids, .. } => leg_ids,
            ConnectedEvidence::RtcMediaPathEstablished { .. } => &[],
        }
    }

    /// Shape rules (counts, duplicates). Facts are verified by the service.
    pub fn check_shape(&self) -> CallResult<()> {
        let (sessions, legs) = (self.session_ids(), self.leg_ids());
        let (min_sessions, min_legs) = match self {
            ConnectedEvidence::RtcMediaPathEstablished { .. } => (2, 0),
            ConnectedEvidence::NetworkLegEstablished { .. } => (0, 2),
            ConnectedEvidence::MixedBridgeEstablished { .. } => (1, 1),
        };
        if sessions.len() < min_sessions || legs.len() < min_legs {
            return Err(CallError::InsufficientEvidence(
                "a bidirectional media path needs two connected endpoints",
            ));
        }
        if sessions.len() > Self::MAX_REFERENCES || legs.len() > Self::MAX_REFERENCES {
            return Err(CallError::Validation("too many evidence references"));
        }
        let mut s = sessions.to_vec();
        s.sort();
        s.dedup();
        let mut l = legs.to_vec();
        l.sort();
        l.dedup();
        if s.len() != sessions.len() || l.len() != legs.len() {
            return Err(CallError::Validation("duplicate evidence references"));
        }
        Ok(())
    }
}

/// Evidence that an ENDING Call has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndedEvidence {
    /// The executing boundary confirmed media / network cleanup. Remaining open session
    /// intervals are closed with the transition.
    CleanupConfirmed,
    /// Verified by Calling: no session has an open connection interval and no leg is
    /// non-terminal.
    AllParticipationEnded,
    /// Documented disposition: the Call has been ENDING for at least the configured
    /// reconciliation timeout without completion evidence.
    ReconciliationTimeout,
}

impl EndedEvidence {
    pub fn name(&self) -> &'static str {
        match self {
            EndedEvidence::CleanupConfirmed => "CLEANUP_CONFIRMED",
            EndedEvidence::AllParticipationEnded => "PARTICIPATION_ENDED",
            EndedEvidence::ReconciliationTimeout => "RECONCILIATION_TIMEOUT",
        }
    }

    pub fn terminal_reason(&self) -> TerminalReason {
        match self {
            EndedEvidence::CleanupConfirmed => TerminalReason::CleanupConfirmed,
            EndedEvidence::AllParticipationEnded => TerminalReason::ParticipationEnded,
            EndedEvidence::ReconciliationTimeout => TerminalReason::ReconciliationTimeout,
        }
    }
}

/// A requested action. The target state is always derived by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallAction {
    /// REQUESTED → AUTHORIZED, with the Call's effective APPROVED authorization.
    Authorize {
        authorization_id: CallAuthorizationId,
    },
    /// Any non-terminal → FAILED (POLICY), with the Call's effective DENIED authorization.
    DenyByPolicy {
        authorization_id: CallAuthorizationId,
    },
    BeginResolution,
    BeginRouting,
    BeginDialing,
    MarkRinging,
    /// The target accepted: DIALING / RINGING → ANSWERED. Not CONNECTED.
    Accept,
    ConfirmConnected {
        evidence: ConnectedEvidence,
    },
    Hold,
    Resume {
        evidence: ConnectedEvidence,
    },
    BeginTransfer,
    CompleteTransfer {
        evidence: ConnectedEvidence,
    },
    RequestEnd,
    ConfirmEnded {
        evidence: EndedEvidence,
    },
    Decline,
    MarkBusy,
    MarkNoAnswer,
    Fail {
        category: FailureCategory,
    },
    Cancel,
}

const NOT_TERMINAL: &[CallState] = &[
    Requested,
    Authorized,
    Resolving,
    Routing,
    Dialing,
    Ringing,
    Answered,
    Connected,
    Held,
    Transferring,
    Ending,
];

impl CallAction {
    pub fn name(&self) -> &'static str {
        match self {
            CallAction::Authorize { .. } => "AUTHORIZE",
            CallAction::DenyByPolicy { .. } => "DENY_BY_POLICY",
            CallAction::BeginResolution => "BEGIN_RESOLUTION",
            CallAction::BeginRouting => "BEGIN_ROUTING",
            CallAction::BeginDialing => "BEGIN_DIALING",
            CallAction::MarkRinging => "MARK_RINGING",
            CallAction::Accept => "ACCEPT",
            CallAction::ConfirmConnected { .. } => "CONFIRM_CONNECTED",
            CallAction::Hold => "HOLD",
            CallAction::Resume { .. } => "RESUME",
            CallAction::BeginTransfer => "BEGIN_TRANSFER",
            CallAction::CompleteTransfer { .. } => "COMPLETE_TRANSFER",
            CallAction::RequestEnd => "REQUEST_END",
            CallAction::ConfirmEnded { .. } => "CONFIRM_ENDED",
            CallAction::Decline => "DECLINE",
            CallAction::MarkBusy => "MARK_BUSY",
            CallAction::MarkNoAnswer => "MARK_NO_ANSWER",
            CallAction::Fail { .. } => "FAIL",
            CallAction::Cancel => "CANCEL",
        }
    }

    /// The state this action leads to.
    pub fn target(&self) -> CallState {
        match self {
            CallAction::Authorize { .. } => Authorized,
            CallAction::DenyByPolicy { .. } | CallAction::Fail { .. } => Failed,
            CallAction::BeginResolution => Resolving,
            CallAction::BeginRouting => Routing,
            CallAction::BeginDialing => Dialing,
            CallAction::MarkRinging => Ringing,
            CallAction::Accept => Answered,
            CallAction::ConfirmConnected { .. }
            | CallAction::Resume { .. }
            | CallAction::CompleteTransfer { .. } => Connected,
            CallAction::Hold => Held,
            CallAction::BeginTransfer => Transferring,
            CallAction::RequestEnd => Ending,
            CallAction::ConfirmEnded { .. } => Ended,
            CallAction::Decline => Declined,
            CallAction::MarkBusy => Busy,
            CallAction::MarkNoAnswer => NoAnswer,
            CallAction::Cancel => Cancelled,
        }
    }

    /// States the action may be applied in (always a subset of the graph's sources of
    /// [`target`](Self::target)).
    pub fn sources(&self) -> &'static [CallState] {
        match self {
            CallAction::Authorize { .. } => &[Requested],
            CallAction::DenyByPolicy { .. } | CallAction::Fail { .. } => NOT_TERMINAL,
            CallAction::BeginResolution => &[Authorized],
            CallAction::BeginRouting => &[Resolving],
            CallAction::BeginDialing => &[Routing],
            CallAction::MarkRinging => &[Dialing],
            CallAction::Accept => &[Dialing, Ringing],
            CallAction::ConfirmConnected { .. } => &[Answered],
            CallAction::Hold => &[Connected, Transferring],
            CallAction::Resume { .. } => &[Held],
            CallAction::BeginTransfer => &[Connected, Held],
            CallAction::CompleteTransfer { .. } => &[Transferring],
            CallAction::RequestEnd => &[Answered, Connected, Held, Transferring],
            CallAction::ConfirmEnded { .. } => &[Ending],
            CallAction::Decline | CallAction::MarkBusy | CallAction::MarkNoAnswer => {
                &[Dialing, Ringing]
            }
            CallAction::Cancel => &[
                Requested, Authorized, Resolving, Routing, Dialing, Ringing, Answered,
            ],
        }
    }

    pub fn connected_evidence(&self) -> Option<&ConnectedEvidence> {
        match self {
            CallAction::ConfirmConnected { evidence }
            | CallAction::Resume { evidence }
            | CallAction::CompleteTransfer { evidence } => Some(evidence),
            _ => None,
        }
    }

    /// Every action, with placeholder evidence (tests and exhaustive checks).
    pub fn representatives() -> Vec<CallAction> {
        let auth = CallAuthorizationId::from_uuid(uuid::Uuid::nil());
        let evidence = ConnectedEvidence::RtcMediaPathEstablished {
            session_ids: vec![],
        };
        vec![
            CallAction::Authorize {
                authorization_id: auth,
            },
            CallAction::DenyByPolicy {
                authorization_id: auth,
            },
            CallAction::BeginResolution,
            CallAction::BeginRouting,
            CallAction::BeginDialing,
            CallAction::MarkRinging,
            CallAction::Accept,
            CallAction::ConfirmConnected {
                evidence: evidence.clone(),
            },
            CallAction::Hold,
            CallAction::Resume {
                evidence: evidence.clone(),
            },
            CallAction::BeginTransfer,
            CallAction::CompleteTransfer { evidence },
            CallAction::RequestEnd,
            CallAction::ConfirmEnded {
                evidence: EndedEvidence::CleanupConfirmed,
            },
            CallAction::Decline,
            CallAction::MarkBusy,
            CallAction::MarkNoAnswer,
            CallAction::Fail {
                category: FailureCategory::Media,
            },
            CallAction::Cancel,
        ]
    }
}

/// The derived effect of a valid action on the Call row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionPlan {
    pub from: CallState,
    pub to: CallState,
    pub new_version: i64,
    pub terminal_reason: Option<TerminalReason>,
    pub failure_category: Option<FailureCategory>,
    pub at: DateTime<Utc>,
}

/// Validates `action` against the Call snapshot (pure; no database).
pub fn plan_transition(
    call: &Call,
    expected_version: i64,
    action: &CallAction,
    at: DateTime<Utc>,
) -> CallResult<TransitionPlan> {
    if expected_version != call.version {
        return Err(CallError::StaleVersion {
            expected: expected_version,
            current: call.version,
        });
    }
    if call.state.is_terminal() {
        return Err(CallError::Terminal { state: call.state });
    }
    let target = action.target();
    if !action.sources().contains(&call.state) || !call.state.can_transition_to(target) {
        return Err(CallError::InvalidTransition {
            state: call.state,
            action: action.name(),
        });
    }
    if let Some(evidence) = action.connected_evidence() {
        evidence.check_shape()?;
    }
    let (terminal_reason, failure_category) = match action {
        CallAction::DenyByPolicy { .. } => {
            (Some(TerminalReason::Failed), Some(FailureCategory::Policy))
        }
        CallAction::Fail { category } => {
            if category.is_policy() {
                return Err(CallError::FailureCategoryNotAllowed(*category));
            }
            (Some(TerminalReason::Failed), Some(*category))
        }
        CallAction::ConfirmEnded { evidence } => (Some(evidence.terminal_reason()), None),
        CallAction::Decline => (Some(TerminalReason::Declined), None),
        CallAction::MarkBusy => (Some(TerminalReason::Busy), None),
        CallAction::MarkNoAnswer => (Some(TerminalReason::NoAnswer), None),
        CallAction::Cancel => (Some(TerminalReason::Cancelled), None),
        _ => (None, None),
    };
    Ok(TransitionPlan {
        from: call.state,
        to: target,
        new_version: call.version + 1,
        terminal_reason,
        failure_category,
        // Never before the last persisted change (tolerates clock skew between replicas).
        at: at.max(call.updated_at),
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::{
        calls::{aggregate::CallTimestamps, command::CallScope, ids::CallId, kinds::CallKind},
        domain::{
            ApplicationId, CredentialId, EnvironmentId, PrincipalId, ServiceAccountId, TenantId,
        },
    };

    fn call(state: CallState, version: i64) -> Call {
        let t = Utc.with_ymd_and_hms(2031, 1, 1, 0, 0, 0).unwrap();
        Call {
            id: CallId::generate(),
            scope: CallScope::from_parts(
                TenantId::generate(),
                ApplicationId::generate(),
                EnvironmentId::generate(),
            ),
            created_by_service_account_id: ServiceAccountId::generate(),
            created_by_credential_id: CredentialId::generate(),
            initiator_principal_id: PrincipalId::generate(),
            kind: CallKind::Voice,
            state,
            version,
            communication_context_ref: None,
            privacy_relay_ref: None,
            terminal_reason: None,
            failure_category: None,
            timestamps: CallTimestamps {
                requested_at: t,
                authorized_at: None,
                resolving_at: None,
                routing_at: None,
                dialing_at: None,
                ringing_at: None,
                answered_at: None,
                connected_at: None,
                ending_at: None,
                ended_at: None,
            },
            created_at: t,
            updated_at: t,
        }
    }

    fn evidence() -> ConnectedEvidence {
        ConnectedEvidence::RtcMediaPathEstablished {
            session_ids: vec![CallSessionId::generate(), CallSessionId::generate()],
        }
    }

    #[test]
    fn action_edges_are_a_subset_of_the_graph_and_cover_it() {
        let mut covered = Vec::new();
        for action in CallAction::representatives() {
            for source in action.sources() {
                assert!(
                    source.can_transition_to(action.target()),
                    "{} uses missing edge {source} -> {}",
                    action.name(),
                    action.target()
                );
                covered.push((*source, action.target()));
            }
        }
        for from in CallState::ALL {
            for to in from.allowed_targets() {
                assert!(
                    covered.contains(&(*from, *to)),
                    "edge {from} -> {to} has no action"
                );
            }
        }
    }

    #[test]
    fn stale_versions_are_rejected_before_anything_else() {
        let c = call(Ended, 5);
        let err = plan_transition(&c, 4, &CallAction::MarkRinging, Utc::now()).unwrap_err();
        assert!(matches!(
            err,
            CallError::StaleVersion {
                expected: 4,
                current: 5
            }
        ));
    }

    #[test]
    fn terminal_calls_never_move() {
        for state in CallState::TERMINAL {
            for action in CallAction::representatives() {
                let err = plan_transition(&call(*state, 9), 9, &action, Utc::now()).unwrap_err();
                assert!(
                    matches!(err, CallError::Terminal { .. }),
                    "{state} {}",
                    action.name()
                );
            }
        }
    }

    #[test]
    fn requested_cannot_jump_ahead() {
        let c = call(Requested, 1);
        for action in [
            CallAction::MarkRinging,
            CallAction::Accept,
            CallAction::ConfirmConnected {
                evidence: evidence(),
            },
        ] {
            assert!(matches!(
                plan_transition(&c, 1, &action, Utc::now()).unwrap_err(),
                CallError::InvalidTransition { .. }
            ));
        }
    }

    #[test]
    fn connected_cannot_be_cancelled_and_answered_is_not_connected() {
        assert!(plan_transition(&call(Connected, 3), 3, &CallAction::Cancel, Utc::now()).is_err());
        let plan = plan_transition(&call(Ringing, 3), 3, &CallAction::Accept, Utc::now()).unwrap();
        assert_eq!(plan.to, Answered);
        assert_ne!(plan.to, Connected);
        let failed = plan_transition(
            &call(Answered, 4),
            4,
            &CallAction::Fail {
                category: FailureCategory::Media,
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!(failed.to, Failed);
        assert_eq!(failed.failure_category, Some(FailureCategory::Media));
    }

    #[test]
    fn connected_requires_well_formed_evidence() {
        let c = call(Answered, 4);
        let none = ConnectedEvidence::RtcMediaPathEstablished {
            session_ids: vec![],
        };
        let one = ConnectedEvidence::NetworkLegEstablished {
            leg_ids: vec![CallLegId::generate()],
        };
        let s = CallSessionId::generate();
        let dup = ConnectedEvidence::RtcMediaPathEstablished {
            session_ids: vec![s, s],
        };
        for evidence in [none, one, dup] {
            assert!(plan_transition(
                &c,
                4,
                &CallAction::ConfirmConnected { evidence },
                Utc::now()
            )
            .is_err());
        }
        assert!(plan_transition(
            &c,
            4,
            &CallAction::ConfirmConnected {
                evidence: evidence()
            },
            Utc::now()
        )
        .is_ok());
    }

    #[test]
    fn policy_failures_need_the_denial_path() {
        let c = call(Requested, 1);
        assert!(matches!(
            plan_transition(
                &c,
                1,
                &CallAction::Fail {
                    category: FailureCategory::Policy
                },
                Utc::now()
            )
            .unwrap_err(),
            CallError::FailureCategoryNotAllowed(FailureCategory::Policy)
        ));
        let plan = plan_transition(
            &c,
            1,
            &CallAction::DenyByPolicy {
                authorization_id: CallAuthorizationId::generate(),
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!(
            (plan.to, plan.failure_category),
            (Failed, Some(FailureCategory::Policy))
        );
    }

    #[test]
    fn plans_never_run_backwards_in_time() {
        let mut c = call(Requested, 1);
        c.updated_at = Utc.with_ymd_and_hms(2031, 6, 1, 0, 0, 0).unwrap();
        let early = Utc.with_ymd_and_hms(2031, 1, 2, 0, 0, 0).unwrap();
        let plan = plan_transition(&c, 1, &CallAction::Cancel, early).unwrap();
        assert_eq!(plan.at, c.updated_at);
    }
}
