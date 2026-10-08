//! Canonical Privacy Relay vocabulary (contract `cfmeet.privacy-relay.common`).
//!
//! Every value is generic platform vocabulary — no consumer, product or provider term —
//! and is persisted as TEXT guarded by a CHECK constraint with exactly these values.

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        pub enum $name {
            $(
                #[serde(rename = $s)]
                $variant,
            )+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value { $($s => Some($name::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(
    /// Lifecycle of a Communication Context. REVOKED and EXPIRED are terminal; COMPLETED
    /// never returns to ACTIVE / SUSPENDED.
    CommunicationContextStatus {
        Active = "ACTIVE",
        Suspended = "SUSPENDED",
        Revoked = "REVOKED",
        Completed = "COMPLETED",
        Expired = "EXPIRED",
    }
);

text_enum!(
    /// Lifecycle of a Privacy Relay Session (same graph and terminal rules as contexts).
    PrivacyRelaySessionStatus {
        Active = "ACTIVE",
        Suspended = "SUSPENDED",
        Revoked = "REVOKED",
        Completed = "COMPLETED",
        Expired = "EXPIRED",
    }
);

text_enum!(
    /// Communication channel. VOICE is the implemented channel; MESSAGING may be declared
    /// for the future but is never executed (no chat / SMS / provider messaging exists).
    CommunicationChannel {
        Voice = "VOICE",
        Messaging = "MESSAGING",
    }
);

text_enum!(
    /// BIDIRECTIONAL: any two active participants, both ways. EXPLICIT: only directed edges.
    DirectionPolicy {
        Bidirectional = "BIDIRECTIONAL",
        Explicit = "EXPLICIT",
    }
);

text_enum!(
    /// Kind of an authorization subject: a CF Meet principal id or an opaque
    /// ContactReference id (never a telephone number, e-mail address or SIP URI).
    ParticipantSubjectKind {
        Principal = "PRINCIPAL",
        ContactReference = "CONTACT_REFERENCE",
    }
);

text_enum!(
    /// Relay strategy: POLICY, not proof a route exists. Every strategy permits the
    /// application route; the public-network and callback strategies only become
    /// executable when relay-number / telephony infrastructure exists.
    RelayStrategy {
        AppOnly = "APP_ONLY",
        AppPreferred = "APP_PREFERRED",
        PublicRelayAllowed = "PUBLIC_RELAY_ALLOWED",
        CallbackRelayAllowed = "CALLBACK_RELAY_ALLOWED",
    }
);

text_enum!(
    /// Route an authorization request asks for.
    CommunicationRoute {
        Application = "APPLICATION",
        PublicRelay = "PUBLIC_RELAY",
        CallbackRelay = "CALLBACK_RELAY",
    }
);

text_enum!(
    /// Why the requester asks (safe call intent metadata).
    AuthorizationPurpose {
        CallCreate = "CALL_CREATE",
        CallTransfer = "CALL_TRANSFER",
        Check = "CHECK",
    }
);

text_enum!(
    AuthorizationDecision {
        Authorized = "AUTHORIZED",
        Denied = "DENIED",
    }
);

text_enum!(
    /// Normalized platform reasons of an authorization decision.
    AuthorizationReason {
        Authorized = "AUTHORIZED",
        FeatureDisabled = "FEATURE_DISABLED",
        ContextNotActive = "CONTEXT_NOT_ACTIVE",
        ContextNotStarted = "CONTEXT_NOT_STARTED",
        ContextExpired = "CONTEXT_EXPIRED",
        ContextRevoked = "CONTEXT_REVOKED",
        ContextSuspended = "CONTEXT_SUSPENDED",
        SessionNotActive = "SESSION_NOT_ACTIVE",
        SessionNotStarted = "SESSION_NOT_STARTED",
        SessionExpired = "SESSION_EXPIRED",
        ParticipantNotAuthorized = "PARTICIPANT_NOT_AUTHORIZED",
        DirectionNotAllowed = "DIRECTION_NOT_ALLOWED",
        ChannelNotAllowed = "CHANNEL_NOT_ALLOWED",
        StrategyNotAllowed = "STRATEGY_NOT_ALLOWED",
        GracePolicyDenied = "GRACE_POLICY_DENIED",
        CapabilityUnavailable = "CAPABILITY_UNAVAILABLE",
        ContactNotVerified = "CONTACT_NOT_VERIFIED",
        ContactVerificationExpired = "CONTACT_VERIFICATION_EXPIRED",
        ContactBlocked = "CONTACT_BLOCKED",
    }
);

text_enum!(
    /// Active-call enforcement state reported by a mutation.
    EnforcementState {
        NotRequired = "NOT_REQUIRED",
        Pending = "PENDING",
        Applied = "APPLIED",
    }
);

text_enum!(
    /// Why active protected calls are enforced against.
    EnforcementReason {
        ContextRevoked = "CONTEXT_REVOKED",
        ContextSuspended = "CONTEXT_SUSPENDED",
        ContextCompleted = "CONTEXT_COMPLETED",
        ContextExpired = "CONTEXT_EXPIRED",
        ParticipantRemoved = "PARTICIPANT_REMOVED",
        SessionRevoked = "SESSION_REVOKED",
        SessionSuspended = "SESSION_SUSPENDED",
        SessionExpired = "SESSION_EXPIRED",
    }
);

text_enum!(
    /// Whether a session follows its context (INHERITED) or was explicitly narrowed.
    SnapshotScope {
        Inherited = "INHERITED",
        Narrowed = "NARROWED",
    }
);

text_enum!(
    /// Audit event types (immutable evidence).
    AuditEventType {
        ContextCreated = "CONTEXT_CREATED",
        ContextUpdated = "CONTEXT_UPDATED",
        ContextParticipantsChanged = "CONTEXT_PARTICIPANTS_CHANGED",
        ContextSuspended = "CONTEXT_SUSPENDED",
        ContextResumed = "CONTEXT_RESUMED",
        ContextRevoked = "CONTEXT_REVOKED",
        ContextCompleted = "CONTEXT_COMPLETED",
        ContextExpired = "CONTEXT_EXPIRED",
        RelaySessionCreated = "RELAY_SESSION_CREATED",
        RelaySessionUpdated = "RELAY_SESSION_UPDATED",
        RelaySessionSuspended = "RELAY_SESSION_SUSPENDED",
        RelaySessionResumed = "RELAY_SESSION_RESUMED",
        RelaySessionRevoked = "RELAY_SESSION_REVOKED",
        RelaySessionCompleted = "RELAY_SESSION_COMPLETED",
        RelaySessionExpired = "RELAY_SESSION_EXPIRED",
        EnforcementRequested = "ENFORCEMENT_REQUESTED",
        EnforcementApplied = "ENFORCEMENT_APPLIED",
        EnforcementFailed = "ENFORCEMENT_FAILED",
    }
);

impl AuditEventType {
    /// Outbox event type (`privacy-relay.<aggregate>.<fact>`), when the event is published.
    pub fn envelope_type(&self) -> &'static str {
        match self {
            AuditEventType::ContextCreated => "privacy-relay.context.created",
            AuditEventType::ContextUpdated => "privacy-relay.context.updated",
            AuditEventType::ContextParticipantsChanged => {
                "privacy-relay.context.participants-changed"
            }
            AuditEventType::ContextSuspended => "privacy-relay.context.suspended",
            AuditEventType::ContextResumed => "privacy-relay.context.resumed",
            AuditEventType::ContextRevoked => "privacy-relay.context.revoked",
            AuditEventType::ContextCompleted => "privacy-relay.context.completed",
            AuditEventType::ContextExpired => "privacy-relay.context.expired",
            AuditEventType::RelaySessionCreated => "privacy-relay.session.created",
            AuditEventType::RelaySessionUpdated => "privacy-relay.session.updated",
            AuditEventType::RelaySessionSuspended => "privacy-relay.session.suspended",
            AuditEventType::RelaySessionResumed => "privacy-relay.session.resumed",
            AuditEventType::RelaySessionRevoked => "privacy-relay.session.revoked",
            AuditEventType::RelaySessionCompleted => "privacy-relay.session.completed",
            AuditEventType::RelaySessionExpired => "privacy-relay.session.expired",
            AuditEventType::EnforcementRequested => "privacy-relay.enforcement.requested",
            AuditEventType::EnforcementApplied => "privacy-relay.enforcement.applied",
            AuditEventType::EnforcementFailed => "privacy-relay.enforcement.failed",
        }
    }

    /// Whether the event describes a relay session (else the context).
    pub fn is_session_event(&self) -> bool {
        self.as_str().starts_with("RELAY_SESSION_")
    }
}

text_enum!(
    /// Idempotent command operations.
    CommandOperation {
        CreateContext = "CREATE_CONTEXT",
        UpdateContext = "UPDATE_CONTEXT",
        SuspendContext = "SUSPEND_CONTEXT",
        ResumeContext = "RESUME_CONTEXT",
        CompleteContext = "COMPLETE_CONTEXT",
        RevokeContext = "REVOKE_CONTEXT",
        CreateSession = "CREATE_SESSION",
        SuspendSession = "SUSPEND_SESSION",
        ResumeSession = "RESUME_SESSION",
        RevokeSession = "REVOKE_SESSION",
    }
);

impl CommunicationContextStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Revoked | Self::Expired)
    }
}

impl PrivacyRelaySessionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Revoked | Self::Expired)
    }
}

impl RelayStrategy {
    /// Whether the strategy permits `route` as a matter of POLICY (execution is a
    /// separate question: only APPLICATION exists today).
    pub fn permits(&self, route: CommunicationRoute) -> bool {
        match route {
            CommunicationRoute::Application => true,
            CommunicationRoute::PublicRelay => {
                matches!(self, Self::AppPreferred | Self::PublicRelayAllowed)
            }
            CommunicationRoute::CallbackRelay => matches!(self, Self::CallbackRelayAllowed),
        }
    }
}

impl CommunicationRoute {
    /// Whether CF Meet can execute this route today (application RTC only; public-network
    /// and callback relay need later relay-number / telephony infrastructure).
    pub fn is_executable(&self) -> bool {
        matches!(self, Self::Application)
    }
}

impl CommunicationChannel {
    /// Whether CF Meet executes this channel today (VOICE only; MESSAGING is declarative).
    pub fn is_executable(&self) -> bool {
        matches!(self, Self::Voice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabularies_round_trip_and_are_consumer_neutral() {
        fn check<T: Copy + serde::Serialize>(all: &[T], name: impl Fn(T) -> &'static str) {
            for v in all {
                assert_eq!(serde_json::to_value(v).unwrap(), name(*v));
                let lower = name(*v).to_lowercase();
                for term in [
                    "karryam",
                    "healthnet",
                    "rider",
                    "driver",
                    "doctor",
                    "patient",
                    "trip",
                ] {
                    assert!(!lower.contains(term), "{lower}");
                }
            }
        }
        check(CommunicationContextStatus::ALL, |v| v.as_str());
        check(PrivacyRelaySessionStatus::ALL, |v| v.as_str());
        check(CommunicationChannel::ALL, |v| v.as_str());
        check(AuthorizationReason::ALL, |v| v.as_str());
        check(AuditEventType::ALL, |v| v.as_str());
        for v in AuditEventType::ALL {
            assert!(v.envelope_type().starts_with("privacy-relay."));
        }
    }

    #[test]
    fn strategies_are_policy_not_execution() {
        for s in RelayStrategy::ALL {
            assert!(s.permits(CommunicationRoute::Application));
        }
        assert!(!RelayStrategy::AppOnly.permits(CommunicationRoute::PublicRelay));
        assert!(RelayStrategy::AppPreferred.permits(CommunicationRoute::PublicRelay));
        assert!(RelayStrategy::PublicRelayAllowed.permits(CommunicationRoute::PublicRelay));
        assert!(!RelayStrategy::PublicRelayAllowed.permits(CommunicationRoute::CallbackRelay));
        assert!(RelayStrategy::CallbackRelayAllowed.permits(CommunicationRoute::CallbackRelay));
        assert!(!CommunicationRoute::PublicRelay.is_executable());
        assert!(!CommunicationRoute::CallbackRelay.is_executable());
        assert!(!CommunicationChannel::Messaging.is_executable());
    }
}
