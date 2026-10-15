//! Provider-neutral Telephony vocabulary (Part E §30–§34, §49). Values match the `CHECK`
//! constraints of the Telephony migrations and the contract
//! `packages/contracts/telephony/common/v1.schema.json`. No value names a vendor, a SIP
//! response code or a carrier status: provider statuses are mapped INTO these by the
//! adapters and never leave Telephony.

macro_rules! vocab {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
                 serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $( #[serde(rename = $s)] $variant, )+
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
pub(crate) use vocab;

vocab!(
    /// Normalized execution lifecycle of a Telephony leg (distinct from the Call state).
    LegStatus {
        Planned = "PLANNED",
        Originating = "ORIGINATING",
        Ringing = "RINGING",
        Answered = "ANSWERED",
        Connected = "CONNECTED",
        Held = "HELD",
        Ending = "ENDING",
        Ended = "ENDED",
        Failed = "FAILED",
        Unknown = "UNKNOWN",
    }
);

impl LegStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, LegStatus::Ended | LegStatus::Failed)
    }

    /// Mirrors `trg_telephony_legs_transition` (a test proves they agree).
    pub fn allowed_targets(&self) -> &'static [LegStatus] {
        use LegStatus::*;
        match self {
            Planned => &[Originating, Failed, Ended],
            Originating => &[Ringing, Answered, Connected, Failed, Unknown, Ending, Ended],
            Ringing => &[Answered, Connected, Failed, Unknown, Ending, Ended],
            Answered => &[Connected, Held, Ending, Ended, Failed],
            Connected => &[Held, Ending, Ended, Failed],
            Held => &[Connected, Ending, Ended, Failed],
            Ending => &[Ended, Failed, Unknown],
            Unknown => &[Ringing, Answered, Connected, Ending, Ended, Failed],
            Ended | Failed => &[],
        }
    }

    pub fn can_transition_to(&self, target: LegStatus) -> bool {
        self.allowed_targets().contains(&target)
    }

    /// Progress rank used to refuse regressions from out-of-order provider events.
    pub fn progress(&self) -> u8 {
        use LegStatus::*;
        match self {
            Planned => 0,
            Originating => 1,
            Unknown => 1,
            Ringing => 2,
            Answered => 3,
            Connected | Held => 4,
            Ending => 5,
            Ended | Failed => 6,
        }
    }
}

vocab!(Direction {
    Outbound = "OUTBOUND",
    Inbound = "INBOUND",
});

vocab!(
    /// Paid route classes Telephony executes (Calling's Routing Engine decided them).
    PublicRouteClass {
        AppToPstn = "APP_TO_PSTN",
        PstnToApp = "PSTN_TO_APP",
        PstnRelay = "PSTN_RELAY",
        CallbackRelay = "CALLBACK_RELAY",
        // Part F: an internal enterprise SIP dialog on the own edge (no carrier).
        EnterpriseInternal = "ENTERPRISE_INTERNAL",
    }
);

vocab!(MediaAttachment {
    None = "NONE",
    Application = "APPLICATION",
});

vocab!(
    /// Why a leg reached ENDED / FAILED.
    EndReason {
        Completed = "COMPLETED",
        Busy = "BUSY",
        NoAnswer = "NO_ANSWER",
        Declined = "DECLINED",
        Failed = "FAILED",
        Cancelled = "CANCELLED",
        Timeout = "TIMEOUT",
        DurationLimit = "DURATION_LIMIT",
        Orphaned = "ORPHANED",
        NotSent = "NOT_SENT",
        AbsentAtProvider = "ABSENT_AT_PROVIDER",
    }
);

vocab!(
    /// Normalized failure category (same vocabulary as Calling's).
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

vocab!(CommandKind {
    Originate = "ORIGINATE",
    Terminate = "TERMINATE",
    Hold = "HOLD",
    Resume = "RESUME",
    Bridge = "BRIDGE",
    Unbridge = "UNBRIDGE",
    SendDtmf = "SEND_DTMF",
    QueryStatus = "QUERY_STATUS",
    Answer = "ANSWER",
    Prompt = "PROMPT",
    Capture = "CAPTURE",
    CaptureStop = "CAPTURE_STOP",
});

vocab!(CommandStatus {
    Pending = "PENDING",
    Claimed = "CLAIMED",
    Running = "RUNNING",
    Succeeded = "SUCCEEDED",
    Failed = "FAILED",
    UnknownOutcome = "UNKNOWN_OUTCOME",
    Reconciling = "RECONCILING",
    DeadLetter = "DEAD_LETTER",
});

vocab!(
    /// Canonical normalized Telephony event vocabulary (§49).
    NormalizedEvent {
        LegAccepted = "LEG_ACCEPTED",
        LegRinging = "LEG_RINGING",
        LegAnswered = "LEG_ANSWERED",
        LegConnected = "LEG_CONNECTED",
        LegHeld = "LEG_HELD",
        LegResumed = "LEG_RESUMED",
        LegEnded = "LEG_ENDED",
        LegFailed = "LEG_FAILED",
        LegBusy = "LEG_BUSY",
        LegNoAnswer = "LEG_NO_ANSWER",
        LegDeclined = "LEG_DECLINED",
        LegStatusUnknown = "LEG_STATUS_UNKNOWN",
        BridgeEstablished = "BRIDGE_ESTABLISHED",
        BridgeEnded = "BRIDGE_ENDED",
        LegDtmf = "LEG_DTMF",
        InboundOffered = "INBOUND_OFFERED",
    }
);

impl NormalizedEvent {
    /// The leg status this event moves a leg to (None: no status change).
    pub fn target_status(&self) -> Option<LegStatus> {
        use NormalizedEvent::*;
        match self {
            LegAccepted => Some(LegStatus::Originating),
            LegRinging => Some(LegStatus::Ringing),
            LegAnswered => Some(LegStatus::Answered),
            LegConnected | LegResumed => Some(LegStatus::Connected),
            LegHeld => Some(LegStatus::Held),
            LegEnded => Some(LegStatus::Ended),
            LegFailed | LegBusy | LegNoAnswer | LegDeclined => Some(LegStatus::Failed),
            LegStatusUnknown => Some(LegStatus::Unknown),
            BridgeEstablished | BridgeEnded | LegDtmf | InboundOffered => None,
        }
    }

    /// End reason of a terminal event.
    pub fn end_reason(&self) -> Option<EndReason> {
        use NormalizedEvent::*;
        match self {
            LegEnded => Some(EndReason::Completed),
            LegFailed => Some(EndReason::Failed),
            LegBusy => Some(EndReason::Busy),
            LegNoAnswer => Some(EndReason::NoAnswer),
            LegDeclined => Some(EndReason::Declined),
            _ => None,
        }
    }
}

vocab!(
    /// Capabilities a provider ADVERTISES (never assumed from its vendor).
    ProviderCapability {
        OutboundVoice = "OUTBOUND_VOICE",
        InboundVoice = "INBOUND_VOICE",
        SipTrunk = "SIP_TRUNK",
        PublicNumberBinding = "PUBLIC_NUMBER_BINDING",
        StatusQuery = "STATUS_QUERY",
        Events = "EVENTS",
        Health = "HEALTH",
        Capacity = "CAPACITY",
        IdempotentOrigination = "IDEMPOTENT_ORIGINATION",
        CostEstimate = "COST_ESTIMATE",
        Dtmf = "DTMF",
        CallerIdSelection = "CALLER_ID_SELECTION",
    }
);

vocab!(RouteCapability {
    VoiceOutbound = "VOICE_OUTBOUND",
    VoiceInbound = "VOICE_INBOUND",
});

vocab!(Health {
    Healthy = "HEALTHY",
    Degraded = "DEGRADED",
    Unavailable = "UNAVAILABLE",
    Unknown = "UNKNOWN",
});

vocab!(CircuitState {
    Closed = "CLOSED",
    Open = "OPEN",
    HalfOpen = "HALF_OPEN",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabularies_are_provider_neutral() {
        let mut all: Vec<&str> = Vec::new();
        all.extend(LegStatus::ALL.iter().map(|v| v.as_str()));
        all.extend(NormalizedEvent::ALL.iter().map(|v| v.as_str()));
        all.extend(EndReason::ALL.iter().map(|v| v.as_str()));
        all.extend(CommandStatus::ALL.iter().map(|v| v.as_str()));
        for v in all {
            let l = v.to_lowercase();
            for vendor in [
                "asterisk",
                "freeswitch",
                "africa",
                "twilio",
                "sip_",
                "200",
                "486",
            ] {
                assert!(!l.contains(vendor), "{v}");
            }
        }
    }

    #[test]
    fn terminal_states_have_no_exit_and_unknown_is_not_terminal() {
        assert!(LegStatus::Ended.allowed_targets().is_empty());
        assert!(LegStatus::Failed.allowed_targets().is_empty());
        assert!(!LegStatus::Unknown.is_terminal());
        assert!(LegStatus::Unknown.can_transition_to(LegStatus::Ended));
        assert!(!LegStatus::Connected.can_transition_to(LegStatus::Ringing));
    }
}
