//! Persisted, provider-independent call-domain vocabularies. Values match the `CHECK`
//! constraints in `migration/src/m20261007_00000{5..9}_*` and, where they cross the service
//! boundary, the shared contract `packages/contracts/calling/common/v1.schema.json`.
//!
//! No value names a vendor, carrier, protocol status or consumer product.

text_enum!(
    /// Neutral media kind of a Call. CF Meet attaches no consumer business meaning to it.
    CallKind {
        Voice = "VOICE",
        Video = "VIDEO",
    }
);

text_enum!(
    /// Normalized, provider-independent failure category. Provider error codes never
    /// become categories; they stay inside the execution boundary that observed them.
    FailureCategory {
        Policy = "POLICY",
        TargetUnavailable = "TARGET_UNAVAILABLE",
        Routing = "ROUTING",
        Timeout = "TIMEOUT",
        Media = "MEDIA",
        Network = "NETWORK",
        Execution = "EXECUTION",
        Internal = "INTERNAL",
        Unknown = "UNKNOWN",
    }
);

text_enum!(
    /// How a Call reached its terminal state. ENDED records its completion evidence;
    /// every other terminal state has the reason of the same name.
    TerminalReason {
        CleanupConfirmed = "CLEANUP_CONFIRMED",
        ParticipationEnded = "PARTICIPATION_ENDED",
        ReconciliationTimeout = "RECONCILIATION_TIMEOUT",
        Declined = "DECLINED",
        Busy = "BUSY",
        NoAnswer = "NO_ANSWER",
        Failed = "FAILED",
        Cancelled = "CANCELLED",
    }
);

text_enum!(
    /// Role of a logical participant in a Call.
    ParticipantRole {
        Initiator = "INITIATOR",
        Recipient = "RECIPIENT",
    }
);

text_enum!(
    /// Connection state of one call session (device / network participation).
    SessionState {
        Created = "CREATED",
        Connected = "CONNECTED",
        Disconnected = "DISCONNECTED",
        Ended = "ENDED",
    }
);

text_enum!(
    /// Neutral category of the endpoint a session runs on.
    EndpointCategory {
        Mobile = "MOBILE",
        Web = "WEB",
        Desktop = "DESKTOP",
        Server = "SERVER",
        Network = "NETWORK",
    }
);

text_enum!(
    /// Why a connection interval closed.
    DisconnectReason {
        Left = "LEFT",
        ConnectionLost = "CONNECTION_LOST",
        Timeout = "TIMEOUT",
        SessionReplaced = "SESSION_REPLACED",
        SessionEnded = "SESSION_ENDED",
        CallTerminated = "CALL_TERMINATED",
    }
);

text_enum!(
    /// Kind of network the media path of a route uses. A later routing engine maps its
    /// route types onto these neutral classes.
    RouteClass {
        Application = "APPLICATION",
        PublicNetwork = "PUBLIC_NETWORK",
        Mixed = "MIXED",
        PrivateNetwork = "PRIVATE_NETWORK",
    }
);

text_enum!(RouteState {
    Selected = "SELECTED",
    Active = "ACTIVE",
    Ended = "ENDED",
    Exhausted = "EXHAUSTED",
    Failed = "FAILED",
    Cancelled = "CANCELLED",
    Superseded = "SUPERSEDED",
});

text_enum!(
    /// Direction of a leg relative to CF Meet.
    LegDirection {
        Outbound = "OUTBOUND",
        Inbound = "INBOUND",
    }
);

text_enum!(
    /// Neutral kind of network a leg runs over.
    LegKind {
        Application = "APPLICATION",
        PublicNetwork = "PUBLIC_NETWORK",
        PrivateNetwork = "PRIVATE_NETWORK",
    }
);

text_enum!(
    /// Normalized leg state.
    LegState {
        Created = "CREATED",
        Dialing = "DIALING",
        Ringing = "RINGING",
        Answered = "ANSWERED",
        Connected = "CONNECTED",
        Ended = "ENDED",
        Declined = "DECLINED",
        Busy = "BUSY",
        NoAnswer = "NO_ANSWER",
        Failed = "FAILED",
        Cancelled = "CANCELLED",
    }
);

text_enum!(
    /// Outcome of a call attempt. UNKNOWN is explicit and is not a failure: it may later
    /// be resolved to SUCCEEDED or FAILED.
    AttemptOutcome {
        Pending = "PENDING",
        Succeeded = "SUCCEEDED",
        Failed = "FAILED",
        Unknown = "UNKNOWN",
        Abandoned = "ABANDONED",
    }
);

text_enum!(AuthorizationDecision {
    Approved = "APPROVED",
    Denied = "DENIED",
});

text_enum!(
    /// Reason code of an authorization decision. APPROVED decisions are always
    /// POLICY_SATISFIED; every other code is a denial.
    AuthorizationReason {
        PolicySatisfied = "POLICY_SATISFIED",
        PolicyDenied = "POLICY_DENIED",
        LimitExceeded = "LIMIT_EXCEEDED",
        EnvironmentRestricted = "ENVIRONMENT_RESTRICTED",
        PrincipalIneligible = "PRINCIPAL_INELIGIBLE",
        Revoked = "REVOKED",
    }
);

text_enum!(
    /// Kind of actor a call mutation is attributed to.
    ActorKind {
        CustomerApplication = "CUSTOMER_APPLICATION",
        ApplicationEndpoint = "APPLICATION_ENDPOINT",
        InternalService = "INTERNAL_SERVICE",
    }
);

text_enum!(
    /// Why a ring offer exists.
    InvitationPurpose {
        Call = "CALL",
        Transfer = "TRANSFER",
    }
);

text_enum!(
    /// State of a ring offer to one endpoint (Part C §20). Terminal states are final.
    InvitationStatus {
        Pending = "PENDING",
        Ringing = "RINGING",
        Accepted = "ACCEPTED",
        Declined = "DECLINED",
        AnsweredElsewhere = "ANSWERED_ELSEWHERE",
        Cancelled = "CANCELLED",
        Expired = "EXPIRED",
    }
);

text_enum!(
    /// Why a ring offer ended.
    InvitationTerminalReason {
        Accepted = "ACCEPTED",
        Declined = "DECLINED",
        AnsweredElsewhere = "ANSWERED_ELSEWHERE",
        Expired = "EXPIRED",
        CallCancelled = "CALL_CANCELLED",
        CallTerminated = "CALL_TERMINATED",
        EndpointRevoked = "ENDPOINT_REVOKED",
        ParticipantRemoved = "PARTICIPANT_REMOVED",
        TransferEnded = "TRANSFER_ENDED",
    }
);

text_enum!(
    /// A session's media authority: NONE → AUTHORIZED → REVOKED, never back.
    MediaAccess {
        None = "NONE",
        Authorized = "AUTHORIZED",
        Revoked = "REVOKED",
    }
);

text_enum!(
    /// Lifecycle of the Calling-private media binding.
    MediaSessionStatus {
        Ensuring = "ENSURING",
        Active = "ACTIVE",
        Terminating = "TERMINATING",
        Terminated = "TERMINATED",
    }
);

text_enum!(
    /// Neutral classification of a failed media-provider operation.
    MediaFailure {
        Timeout = "TIMEOUT",
        Unavailable = "UNAVAILABLE",
        Rejected = "REJECTED",
        NotFound = "NOT_FOUND",
        Unknown = "UNKNOWN",
    }
);

text_enum!(
    /// Idempotent Calling commands.
    CommandOperation {
        CreateCall = "CREATE_CALL",
        Accept = "ACCEPT",
        Decline = "DECLINE",
        Cancel = "CANCEL",
        End = "END",
        Hold = "HOLD",
        Resume = "RESUME",
        Transfer = "TRANSFER",
        RemoveSession = "REMOVE_SESSION",
    }
);

text_enum!(CommandStatus {
    InProgress = "IN_PROGRESS",
    Completed = "COMPLETED",
});

text_enum!(
    /// Application-to-application transfer lifecycle (kept out of the Call state).
    TransferState {
        Requested = "REQUESTED",
        RingingTarget = "RINGING_TARGET",
        Accepted = "ACCEPTED",
        Completed = "COMPLETED",
        Failed = "FAILED",
        Cancelled = "CANCELLED",
    }
);

text_enum!(
    /// Why a transfer did not complete.
    TransferFailureReason {
        TargetDeclined = "TARGET_DECLINED",
        TargetNoAnswer = "TARGET_NO_ANSWER",
        TargetUnavailable = "TARGET_UNAVAILABLE",
        MediaPathLost = "MEDIA_PATH_LOST",
        CallEnded = "CALL_ENDED",
        Cancelled = "CANCELLED",
    }
);

text_enum!(
    /// Normalized kind of an authenticated media-provider callback.
    MediaEventKind {
        ResourceStarted = "RESOURCE_STARTED",
        ResourceFinished = "RESOURCE_FINISHED",
        ParticipantJoined = "PARTICIPANT_JOINED",
        ParticipantLeft = "PARTICIPANT_LEFT",
        ParticipantConnectionAborted = "PARTICIPANT_CONNECTION_ABORTED",
        TrackPublished = "TRACK_PUBLISHED",
        TrackUnpublished = "TRACK_UNPUBLISHED",
        Other = "OTHER",
    }
);

text_enum!(
    /// Whether a callback's media identity resolved to a session of the Call.
    MediaIdentityStatus {
        None = "NONE",
        Known = "KNOWN",
        Unknown = "UNKNOWN",
    }
);

text_enum!(
    /// What processing a callback did.
    MediaEventOutcome {
        Applied = "APPLIED",
        NoOp = "NO_OP",
        IgnoredTerminal = "IGNORED_TERMINAL",
        Quarantined = "QUARANTINED",
        Evicted = "EVICTED",
    }
);

text_enum!(
    /// Provider-independent consumption fact types (not billing).
    UsageType {
        CallConnectedTime = "CALL_CONNECTED_TIME",
        ParticipantConnectedTime = "PARTICIPANT_CONNECTED_TIME",
        SessionConnectedTime = "SESSION_CONNECTED_TIME",
        LegConnectedTime = "LEG_CONNECTED_TIME",
        VoicemailArtifact = "VOICEMAIL_ARTIFACT",
        RecordingArtifact = "RECORDING_ARTIFACT",
    }
);

text_enum!(UsageUnit {
    Milliseconds = "MILLISECONDS",
});

text_enum!(OutboxStatus {
    Pending = "PENDING",
    Published = "PUBLISHED",
    DeadLetter = "DEAD_LETTER",
});

impl FailureCategory {
    /// Categories a generic `Fail` action may use. POLICY is reserved for the denial
    /// path, which requires an effective DENIED authorization as evidence.
    pub fn is_policy(&self) -> bool {
        matches!(self, FailureCategory::Policy)
    }
}

impl SessionState {
    pub fn can_transition_to(&self, target: SessionState) -> bool {
        use SessionState::*;
        matches!(
            (self, target),
            (Created, Connected)
                | (Created, Ended)
                | (Connected, Disconnected)
                | (Connected, Ended)
                | (Disconnected, Connected)
                | (Disconnected, Ended)
        )
    }
}

impl RouteState {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, RouteState::Selected | RouteState::Active)
    }

    /// SELECTED → ACTIVE → ENDED | EXHAUSTED | FAILED | CANCELLED | SUPERSEDED (a selected
    /// route may also end directly).
    pub fn can_transition_to(&self, target: RouteState) -> bool {
        use RouteState::*;
        match self {
            Selected => target != Selected,
            Active => target.is_terminal(),
            _ => false,
        }
    }
}

impl InvitationStatus {
    pub fn is_terminal(&self) -> bool {
        !self.is_live()
    }

    /// PENDING / RINGING: the offer can still be answered or end.
    pub fn is_live(&self) -> bool {
        matches!(self, InvitationStatus::Pending | InvitationStatus::Ringing)
    }

    pub fn can_transition_to(&self, target: InvitationStatus) -> bool {
        use InvitationStatus::*;
        matches!(
            (self, target),
            (Pending, Ringing)
                | (Pending, AnsweredElsewhere)
                | (Pending, Cancelled)
                | (Pending, Expired)
                | (Ringing, Accepted)
                | (Ringing, Declined)
                | (Ringing, AnsweredElsewhere)
                | (Ringing, Cancelled)
                | (Ringing, Expired)
        )
    }
}

impl TransferState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            TransferState::Requested | TransferState::RingingTarget | TransferState::Accepted
        )
    }
}

impl LegState {
    pub fn is_terminal(&self) -> bool {
        use LegState::*;
        matches!(
            self,
            Ended | Declined | Busy | NoAnswer | Failed | Cancelled
        )
    }

    pub fn allowed_targets(&self) -> &'static [LegState] {
        use LegState::*;
        match self {
            Created => &[Dialing, Ringing, Failed, Cancelled],
            Dialing => &[
                Ringing, Answered, Declined, Busy, NoAnswer, Failed, Cancelled,
            ],
            Ringing => &[Answered, Declined, Busy, NoAnswer, Failed, Cancelled],
            Answered => &[Connected, Ended, Failed],
            Connected => &[Ended, Failed],
            Ended | Declined | Busy | NoAnswer | Failed | Cancelled => &[],
        }
    }

    pub fn can_transition_to(&self, target: LegState) -> bool {
        self.allowed_targets().contains(&target)
    }
}

impl AttemptOutcome {
    pub fn can_transition_to(&self, target: AttemptOutcome) -> bool {
        use AttemptOutcome::*;
        matches!(
            (self, target),
            (Pending, Succeeded)
                | (Pending, Failed)
                | (Pending, Unknown)
                | (Pending, Abandoned)
                | (Unknown, Succeeded)
                | (Unknown, Failed)
        )
    }
}

impl AuthorizationReason {
    /// The decision this reason code belongs to.
    pub fn decision(&self) -> AuthorizationDecision {
        match self {
            AuthorizationReason::PolicySatisfied => AuthorizationDecision::Approved,
            _ => AuthorizationDecision::Denied,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORBIDDEN_FRAGMENTS: &[&str] = &[
        "LIVEKIT",
        "TWILIO",
        "VONAGE",
        "SIP",
        "PSTN",
        "CARRIER",
        "PROVIDER",
        "KARRYAM",
        "HEALTHNET",
        "RIDER",
        "DRIVER",
        "DOCTOR",
        "PATIENT",
        "DELIVERY",
    ];

    fn all_values() -> Vec<&'static str> {
        let mut v = Vec::new();
        v.extend(CallKind::ALL.iter().map(CallKind::as_str));
        v.extend(FailureCategory::ALL.iter().map(FailureCategory::as_str));
        v.extend(TerminalReason::ALL.iter().map(TerminalReason::as_str));
        v.extend(ParticipantRole::ALL.iter().map(ParticipantRole::as_str));
        v.extend(SessionState::ALL.iter().map(SessionState::as_str));
        v.extend(EndpointCategory::ALL.iter().map(EndpointCategory::as_str));
        v.extend(DisconnectReason::ALL.iter().map(DisconnectReason::as_str));
        v.extend(RouteClass::ALL.iter().map(RouteClass::as_str));
        v.extend(RouteState::ALL.iter().map(RouteState::as_str));
        v.extend(LegDirection::ALL.iter().map(LegDirection::as_str));
        v.extend(LegKind::ALL.iter().map(LegKind::as_str));
        v.extend(LegState::ALL.iter().map(LegState::as_str));
        v.extend(AttemptOutcome::ALL.iter().map(AttemptOutcome::as_str));
        v.extend(
            AuthorizationReason::ALL
                .iter()
                .map(AuthorizationReason::as_str),
        );
        v.extend(UsageType::ALL.iter().map(UsageType::as_str));
        v.extend(InvitationStatus::ALL.iter().map(InvitationStatus::as_str));
        v.extend(
            InvitationTerminalReason::ALL
                .iter()
                .map(InvitationTerminalReason::as_str),
        );
        v.extend(MediaAccess::ALL.iter().map(MediaAccess::as_str));
        v.extend(
            MediaSessionStatus::ALL
                .iter()
                .map(MediaSessionStatus::as_str),
        );
        v.extend(MediaFailure::ALL.iter().map(MediaFailure::as_str));
        v.extend(CommandOperation::ALL.iter().map(CommandOperation::as_str));
        v.extend(TransferState::ALL.iter().map(TransferState::as_str));
        v.extend(
            TransferFailureReason::ALL
                .iter()
                .map(TransferFailureReason::as_str),
        );
        v.extend(MediaEventKind::ALL.iter().map(MediaEventKind::as_str));
        v.extend(MediaEventOutcome::ALL.iter().map(MediaEventOutcome::as_str));
        v
    }

    #[test]
    fn vocabularies_are_provider_and_product_neutral() {
        for value in all_values() {
            for fragment in FORBIDDEN_FRAGMENTS {
                assert!(!value.contains(fragment), "{value} contains {fragment}");
            }
        }
    }

    #[test]
    fn call_kinds_are_minimal_and_generic() {
        assert_eq!(CallKind::ALL, &[CallKind::Voice, CallKind::Video]);
    }

    #[test]
    fn leg_terminal_states_have_no_exit() {
        for state in LegState::ALL {
            assert_eq!(
                state.is_terminal(),
                state.allowed_targets().is_empty(),
                "{state}"
            );
        }
    }

    #[test]
    fn terminal_invitation_states_are_final() {
        for from in InvitationStatus::ALL {
            for to in InvitationStatus::ALL {
                if from.is_terminal() {
                    assert!(!from.can_transition_to(*to), "{from} -> {to}");
                }
            }
        }
        assert!(!InvitationStatus::Accepted.can_transition_to(InvitationStatus::Declined));
        assert!(!InvitationStatus::AnsweredElsewhere.can_transition_to(InvitationStatus::Accepted));
        assert!(!InvitationStatus::Expired.can_transition_to(InvitationStatus::Accepted));
        // Only a ringing offer can be accepted or declined.
        assert!(!InvitationStatus::Pending.can_transition_to(InvitationStatus::Accepted));
    }

    #[test]
    fn route_terminal_states_have_no_exit() {
        for from in RouteState::ALL {
            for to in RouteState::ALL {
                if from.is_terminal() {
                    assert!(!from.can_transition_to(*to));
                }
            }
        }
        assert!(RouteState::Active.can_transition_to(RouteState::Exhausted));
        assert!(!RouteState::Active.can_transition_to(RouteState::Selected));
    }

    #[test]
    fn unknown_attempt_outcome_is_resolvable_but_not_a_failure() {
        assert!(AttemptOutcome::Unknown.can_transition_to(AttemptOutcome::Succeeded));
        assert!(AttemptOutcome::Unknown.can_transition_to(AttemptOutcome::Failed));
        assert!(!AttemptOutcome::Failed.can_transition_to(AttemptOutcome::Succeeded));
        assert_ne!(AttemptOutcome::Unknown, AttemptOutcome::Failed);
    }

    #[test]
    fn approved_decisions_have_exactly_one_reason() {
        let approving: Vec<_> = AuthorizationReason::ALL
            .iter()
            .filter(|r| r.decision() == AuthorizationDecision::Approved)
            .collect();
        assert_eq!(approving, [&AuthorizationReason::PolicySatisfied]);
    }
}
