//! The canonical Call state machine (Part C §17).
//!
//! Exactly seventeen business states. They describe the logical Call, independent of how
//! it is executed: provider, SIP, carrier or RTC states are never canonical and must be
//! mapped onto these by the adapters that observe them.
//!
//! ```text
//! REQUESTED ─► AUTHORIZED ─► RESOLVING ─► ROUTING ─► DIALING ─┬─► RINGING ─┐
//!                                                             └────────────┴─► ANSWERED ─► CONNECTED ⇄ HELD
//!                                                                                  │          ⇅   ⇅
//!                                                                                  │       TRANSFERRING
//!                                                                                  └────────► ENDING ─► ENDED
//! terminal: ENDED · DECLINED · BUSY · NO_ANSWER · FAILED · CANCELLED
//! ```
//!
//! The directed graph below is the conservative implementation decision for §§15–19
//! (the specification defines the states and rules but not every edge). It is mirrored in
//! the database by `calling_call_transition_allowed` as defence in depth; a test proves
//! the two agree on every pair of states.

text_enum!(
    /// Canonical business state of a Call.
    CallState {
        Requested = "REQUESTED",
        Authorized = "AUTHORIZED",
        Resolving = "RESOLVING",
        Routing = "ROUTING",
        Dialing = "DIALING",
        Ringing = "RINGING",
        Answered = "ANSWERED",
        Connected = "CONNECTED",
        Held = "HELD",
        Transferring = "TRANSFERRING",
        Ending = "ENDING",
        Ended = "ENDED",
        Declined = "DECLINED",
        Busy = "BUSY",
        NoAnswer = "NO_ANSWER",
        Failed = "FAILED",
        Cancelled = "CANCELLED",
    }
);

use CallState::*;

impl CallState {
    /// States a Call can never leave.
    pub const TERMINAL: &'static [CallState] =
        &[Ended, Declined, Busy, NoAnswer, Failed, Cancelled];

    pub fn is_terminal(&self) -> bool {
        Self::TERMINAL.contains(self)
    }

    /// Every state this state may move to. Terminal states have none.
    pub fn allowed_targets(&self) -> &'static [CallState] {
        match self {
            Requested => &[Authorized, Failed, Cancelled],
            Authorized => &[Resolving, Failed, Cancelled],
            Resolving => &[Routing, Failed, Cancelled],
            Routing => &[Dialing, Failed, Cancelled],
            Dialing => &[
                Ringing, Answered, Declined, Busy, NoAnswer, Failed, Cancelled,
            ],
            Ringing => &[Answered, Declined, Busy, NoAnswer, Failed, Cancelled],
            Answered => &[Connected, Ending, Failed, Cancelled],
            Connected => &[Held, Transferring, Ending, Failed],
            Held => &[Connected, Transferring, Ending, Failed],
            Transferring => &[Connected, Held, Ending, Failed],
            Ending => &[Ended, Failed],
            Ended | Declined | Busy | NoAnswer | Failed | Cancelled => &[],
        }
    }

    pub fn can_transition_to(&self, target: CallState) -> bool {
        self.allowed_targets().contains(&target)
    }

    /// States that count against the concurrent-call limit.
    pub fn is_active(&self) -> bool {
        !self.is_terminal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_the_seventeen_canonical_states_exist() {
        let names: Vec<&str> = CallState::ALL.iter().map(CallState::as_str).collect();
        assert_eq!(
            names,
            [
                "REQUESTED",
                "AUTHORIZED",
                "RESOLVING",
                "ROUTING",
                "DIALING",
                "RINGING",
                "ANSWERED",
                "CONNECTED",
                "HELD",
                "TRANSFERRING",
                "ENDING",
                "ENDED",
                "DECLINED",
                "BUSY",
                "NO_ANSWER",
                "FAILED",
                "CANCELLED",
            ]
        );
    }

    #[test]
    fn terminal_states_have_no_exit_and_non_terminal_states_can_fail() {
        for state in CallState::ALL {
            if state.is_terminal() {
                assert!(state.allowed_targets().is_empty(), "{state}");
            } else {
                assert!(state.can_transition_to(Failed), "{state} cannot fail");
                assert!(
                    !state.allowed_targets().contains(state),
                    "{state} self-loop"
                );
            }
        }
        assert_eq!(CallState::TERMINAL.len(), 6);
    }

    #[test]
    fn key_rules_of_the_graph() {
        assert!(!Requested.can_transition_to(Connected));
        assert!(!Requested.can_transition_to(Ringing));
        assert!(!Ringing.can_transition_to(Routing));
        assert!(!Connected.can_transition_to(Cancelled));
        assert!(!Answered.can_transition_to(Ringing));
        assert!(Answered.can_transition_to(Failed));
        assert!(Answered.can_transition_to(Connected));
        assert!(!Ending.can_transition_to(Connected));
        assert!(Ending.can_transition_to(Ended));
    }

    #[test]
    fn every_non_terminal_state_is_reachable_from_requested() {
        let mut seen = vec![Requested];
        let mut frontier = vec![Requested];
        while let Some(s) = frontier.pop() {
            for t in s.allowed_targets() {
                if !seen.contains(t) {
                    seen.push(*t);
                    frontier.push(*t);
                }
            }
        }
        assert_eq!(seen.len(), CallState::ALL.len());
    }
}
