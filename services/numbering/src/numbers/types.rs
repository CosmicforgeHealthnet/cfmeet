//! The number domain's vocabulary. Values equal the database CHECK constraints and the
//! `cfmeet.numbering.common` contract (a conformance test compares them).

use serde::Serialize;
use uuid::Uuid;

text_enum!(
    /// The canonical Number Resource lifecycle (Part IV §D2).
    LifecycleState {
        Discovered = "DISCOVERED",
        Available = "AVAILABLE",
        Reserving = "RESERVING",
        Reserved = "RESERVED",
        Provisioning = "PROVISIONING",
        Provisioned = "PROVISIONED",
        Active = "ACTIVE",
        Suspended = "SUSPENDED",
        Releasing = "RELEASING",
        Quarantined = "QUARANTINED",
        Reassignable = "REASSIGNABLE",
        Released = "RELEASED",
        Failed = "FAILED",
        UnknownProviderState = "UNKNOWN_PROVIDER_STATE",
    }
);

text_enum!(
    /// Where a resource came from. SYNTHETIC_TEST is test inventory and is never a real
    /// public number; it is usable only by SANDBOX / TEST environments.
    InventorySource {
        LiveProvider = "LIVE_PROVIDER",
        SyntheticTest = "SYNTHETIC_TEST",
        ManualImport = "MANUAL_IMPORT",
    }
);

text_enum!(
    /// Number classification. Set only from evidence (never inferred from the E.164 format
    /// or from SMS support).
    NumberType {
        Unclassified = "UNCLASSIFIED",
        Geographic = "GEOGRAPHIC",
        Mobile = "MOBILE",
        TollFree = "TOLL_FREE",
        National = "NATIONAL",
    }
);

text_enum!(ClassificationSource {
    None = "NONE",
    ProviderInventory = "PROVIDER_INVENTORY",
    ManualImport = "MANUAL_IMPORT",
    Synthetic = "SYNTHETIC",
});

text_enum!(
    /// Canonical capability types (Part IV §D4).
    Capability {
        VoiceInbound = "VOICE_INBOUND",
        VoiceOutbound = "VOICE_OUTBOUND",
        SmsInbound = "SMS_INBOUND",
        SmsOutbound = "SMS_OUTBOUND",
        MmsRich = "MMS_RICH",
        MobileNumberClass = "MOBILE_NUMBER_CLASS",
        GeographicLocal = "GEOGRAPHIC_LOCAL",
        TollFreeBusiness = "TOLL_FREE_BUSINESS",
    }
);

impl Capability {
    /// Capabilities a provider binding routes traffic for (the others are number-class
    /// attributes).
    pub fn is_routable(&self) -> bool {
        matches!(
            self,
            Capability::VoiceInbound
                | Capability::VoiceOutbound
                | Capability::SmsInbound
                | Capability::SmsOutbound
                | Capability::MmsRich
        )
    }
}

text_enum!(
    /// VERIFIED only with evidence; RESTRICTED / WITHDRAWN are downgrades.
    CapabilityStatus {
        Verified = "VERIFIED",
        Declared = "DECLARED",
        Restricted = "RESTRICTED",
        Withdrawn = "WITHDRAWN",
        Expired = "EXPIRED",
        Revoked = "REVOKED",
    }
);

impl CapabilityStatus {
    /// Whether traffic may rely on the capability.
    pub fn is_usable(&self) -> bool {
        matches!(
            self,
            CapabilityStatus::Verified | CapabilityStatus::Declared
        )
    }
}

text_enum!(
    /// Which catalog a resource is governed by: synthetic test inventory has its own
    /// catalog, never the real one.
    InventoryClass {
        Real = "REAL",
        Synthetic = "SYNTHETIC",
    }
);

impl InventoryClass {
    pub fn of(source: InventorySource) -> Self {
        match source {
            InventorySource::SyntheticTest => InventoryClass::Synthetic,
            _ => InventoryClass::Real,
        }
    }
}

text_enum!(CountrySupport {
    Supported = "SUPPORTED",
    Restricted = "RESTRICTED",
    Unsupported = "UNSUPPORTED",
});

text_enum!(
    /// Where a catalog fact comes from. SYNTHETIC_TEST is test data, never real.
    CatalogSource {
        RegulatorPublication = "REGULATOR_PUBLICATION",
        ProviderDocumentation = "PROVIDER_DOCUMENTATION",
        OperatorAttestation = "OPERATOR_ATTESTATION",
        SyntheticTest = "SYNTHETIC_TEST",
    }
);

text_enum!(CatalogVerification {
    Verified = "VERIFIED",
    Unverified = "UNVERIFIED",
    Synthetic = "SYNTHETIC",
});

text_enum!(LocalityClass {
    National = "NATIONAL",
    Regional = "REGIONAL",
    Local = "LOCAL",
    NonGeographic = "NON_GEOGRAPHIC",
});

text_enum!(
    /// How a capability record changed the effective truth.
    CapabilityChangeKind {
        Initial = "INITIAL",
        Upgrade = "UPGRADE",
        Downgrade = "DOWNGRADE",
        Refresh = "REFRESH",
        Revocation = "REVOCATION",
        Expiry = "EXPIRY",
    }
);

text_enum!(CapabilitySource {
    ProviderInventory = "PROVIDER_INVENTORY",
    ProviderConfirmation = "PROVIDER_CONFIRMATION",
    ManualImport = "MANUAL_IMPORT",
    Synthetic = "SYNTHETIC",
});

text_enum!(Availability {
    Available = "AVAILABLE",
    Unavailable = "UNAVAILABLE",
    Unknown = "UNKNOWN",
});

text_enum!(ReservationStatus {
    Pending = "PENDING",
    Held = "HELD",
    Unknown = "UNKNOWN",
    Expired = "EXPIRED",
    Cancelled = "CANCELLED",
    Converted = "CONVERTED",
    Failed = "FAILED",
});

impl ReservationStatus {
    pub fn is_current(&self) -> bool {
        matches!(
            self,
            ReservationStatus::Pending | ReservationStatus::Held | ReservationStatus::Unknown
        )
    }
}

text_enum!(
    /// Whether a provider hold was given back. An expired reservation does NOT imply a
    /// successful provider release.
    ReleaseState {
        NotStarted = "NOT_STARTED",
        NotRequired = "NOT_REQUIRED",
        Pending = "PENDING",
        Confirmed = "CONFIRMED",
        Unknown = "UNKNOWN",
    }
);

text_enum!(
    /// DCN assignment states (Part IV §D6).
    AssignmentStatus {
        Requested = "REQUESTED",
        PendingEligibility = "PENDING_ELIGIBILITY",
        Eligible = "ELIGIBLE",
        Reserved = "RESERVED",
        Activating = "ACTIVATING",
        Active = "ACTIVE",
        Suspended = "SUSPENDED",
        ReleaseRequested = "RELEASE_REQUESTED",
        RoutingRemoved = "ROUTING_REMOVED",
        Quarantined = "QUARANTINED",
        Closed = "CLOSED",
    }
);

text_enum!(OwnerKind {
    Principal = "PRINCIPAL",
    Organization = "ORGANIZATION",
});

text_enum!(
    /// Provider-independent ownership classes (Part IV §D7).
    OwnershipClass {
        Personal = "PERSONAL",
        Professional = "PROFESSIONAL",
        Organization = "ORGANIZATION",
        EmployeeDirect = "EMPLOYEE_DIRECT",
        Department = "DEPARTMENT",
        Campaign = "CAMPAIGN",
        SharedTeam = "SHARED_TEAM",
    }
);

impl OwnershipClass {
    /// Personal / professional numbers belong to a principal; every organizational class —
    /// including a number routed to an employee — stays the organization's property.
    pub fn owner_kind(&self) -> OwnerKind {
        match self {
            OwnershipClass::Personal | OwnershipClass::Professional => OwnerKind::Principal,
            _ => OwnerKind::Organization,
        }
    }
}

text_enum!(
    /// Voice routing targets: opaque CF Meet ids of Calling / enterprise constructs.
    VoiceTargetKind {
        None = "NONE",
        Principal = "PRINCIPAL",
        ProfessionalIdentity = "PROFESSIONAL_IDENTITY",
        CallingEndpoint = "CALLING_ENDPOINT",
        EnterpriseExtension = "ENTERPRISE_EXTENSION",
        EnterpriseRingGroup = "ENTERPRISE_RING_GROUP",
        EnterpriseQueue = "ENTERPRISE_QUEUE",
        EnterpriseIvr = "ENTERPRISE_IVR",
        EnterpriseDepartment = "ENTERPRISE_DEPARTMENT",
        SharedTeam = "SHARED_TEAM",
        Voicemail = "VOICEMAIL",
        ApprovedForwarding = "APPROVED_FORWARDING",
    }
);

text_enum!(
    /// SMS routing targets: the owner / inbox that receives a number's messages (opaque
    /// CF Meet ids; the inbox itself is the SMS workstreams').
    SmsTargetKind {
        None = "NONE",
        Principal = "PRINCIPAL",
        Application = "APPLICATION",
        SharedTeam = "SHARED_TEAM",
        Department = "DEPARTMENT",
    }
);

text_enum!(ActorKind {
    Operator = "OPERATOR",
    Service = "SERVICE",
    System = "SYSTEM",
    Reconciliation = "RECONCILIATION",
    Customer = "CUSTOMER",
});

text_enum!(EnvironmentKind {
    Sandbox = "SANDBOX",
    Test = "TEST",
    Production = "PRODUCTION",
});

text_enum!(RiskTier {
    Unassessed = "UNASSESSED",
    Standard = "STANDARD",
    Elevated = "ELEVATED",
    High = "HIGH",
});

text_enum!(CostEvidence {
    Known = "KNOWN",
    Estimated = "ESTIMATED",
    Unknown = "UNKNOWN",
});

text_enum!(FailureCategory {
    ProviderRejected = "PROVIDER_REJECTED",
    ProviderUnavailable = "PROVIDER_UNAVAILABLE",
    NotObtainable = "NOT_OBTAINABLE",
    PolicyDenied = "POLICY_DENIED",
    ValidationFailed = "VALIDATION_FAILED",
    ReconciledFailed = "RECONCILED_FAILED",
});

text_enum!(
    /// The external operation whose outcome is unknown (UNKNOWN_PROVIDER_STATE).
    UncertainOperation {
        Discover = "DISCOVER",
        Reserve = "RESERVE",
        ReleaseHold = "RELEASE_HOLD",
        Provision = "PROVISION",
        Activate = "ACTIVATE",
        Release = "RELEASE",
        Port = "PORT",
    }
);

text_enum!(BindingStatus {
    Active = "ACTIVE",
    Ended = "ENDED",
});

text_enum!(BindingChangeReason {
    Initial = "INITIAL",
    ProviderReplacement = "PROVIDER_REPLACEMENT",
    PortIn = "PORT_IN",
    PortOut = "PORT_OUT",
    Correction = "CORRECTION",
    Release = "RELEASE",
});

text_enum!(RestrictionFlag {
    NoInternationalOutbound = "NO_INTERNATIONAL_OUTBOUND",
    NoPremiumDestinations = "NO_PREMIUM_DESTINATIONS",
    KycRequired = "KYC_REQUIRED",
    LocalPresenceRequired = "LOCAL_PRESENCE_REQUIRED",
    SmsRegistrationRequired = "SMS_REGISTRATION_REQUIRED",
    EmergencyServicesUnsupported = "EMERGENCY_SERVICES_UNSUPPORTED",
});

/// Who performed a change (recorded in lifecycle / assignment history).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Actor {
    pub kind: ActorKind,
    /// Bounded opaque reference: a service id, operator id, service-account id, worker name.
    pub reference: String,
}

impl Actor {
    pub fn new(kind: ActorKind, reference: impl Into<String>) -> Self {
        let reference: String = reference.into();
        let reference: String = reference
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '@' | '-'))
            .take(200)
            .collect();
        Self {
            kind,
            reference: if reference.is_empty() {
                "unknown".into()
            } else {
                reference
            },
        }
    }

    pub fn system(worker: &str) -> Self {
        Self::new(ActorKind::System, worker)
    }

    pub fn operator(name: &str) -> Self {
        Self::new(ActorKind::Operator, name)
    }

    pub fn reconciliation(reference: &str) -> Self {
        Self::new(ActorKind::Reconciliation, reference)
    }
}

/// The trusted owning scope of a customer operation (from Platform Access, never from a
/// request body).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: EnvironmentKind,
    pub service_account_id: Uuid,
}

impl OwnerScope {
    pub fn event_scope(&self) -> crate::infra::outbox::EventScope {
        crate::infra::outbox::EventScope {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
        }
    }

    /// Whether the environment may use inventory of `source` (explicit separation of
    /// sandbox / test and production).
    pub fn may_use(&self, source: InventorySource) -> bool {
        match self.environment_kind {
            EnvironmentKind::Production => source != InventorySource::SyntheticTest,
            EnvironmentKind::Sandbox | EnvironmentKind::Test => {
                source == InventorySource::SyntheticTest
            }
        }
    }
}

/// Validates a reason code (`[A-Z][A-Z0-9_]{1,63}`).
pub fn reason_code(code: &str) -> Result<&str, crate::numbers::DomainError> {
    let ok = (2..=64).contains(&code.len())
        && code.as_bytes()[0].is_ascii_uppercase()
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    ok.then_some(code).ok_or_else(|| {
        crate::numbers::DomainError::Validation("reason codes are UPPER_SNAKE_CASE".into())
    })
}
