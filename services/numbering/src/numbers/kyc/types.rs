//! The KYC and eligibility vocabulary (equal to the database CHECK constraints and the
//! `cfmeet.numbering.kyc` contract).

text_enum!(
    /// What a requirement asks the subject to prove.
    RequirementType {
        IndividualIdentity = "INDIVIDUAL_IDENTITY",
        BusinessRegistration = "BUSINESS_REGISTRATION",
        Address = "ADDRESS",
        LocalPresence = "LOCAL_PRESENCE",
        PurposeOfUse = "PURPOSE_OF_USE",
        BeneficialOwnership = "BENEFICIAL_OWNERSHIP",
    }
);

text_enum!(EvidenceKind {
    IdentityDocument = "IDENTITY_DOCUMENT",
    Passport = "PASSPORT",
    BusinessRegistrationCertificate = "BUSINESS_REGISTRATION_CERTIFICATE",
    ProofOfAddress = "PROOF_OF_ADDRESS",
    LocalPresenceAttestation = "LOCAL_PRESENCE_ATTESTATION",
    PurposeStatement = "PURPOSE_STATEMENT",
    BeneficialOwnershipDeclaration = "BENEFICIAL_OWNERSHIP_DECLARATION",
});

impl EvidenceKind {
    /// The requirement an evidence kind can satisfy by default.
    pub fn default_requirement(&self) -> RequirementType {
        match self {
            EvidenceKind::IdentityDocument | EvidenceKind::Passport => {
                RequirementType::IndividualIdentity
            }
            EvidenceKind::BusinessRegistrationCertificate => RequirementType::BusinessRegistration,
            EvidenceKind::ProofOfAddress => RequirementType::Address,
            EvidenceKind::LocalPresenceAttestation => RequirementType::LocalPresence,
            EvidenceKind::PurposeStatement => RequirementType::PurposeOfUse,
            EvidenceKind::BeneficialOwnershipDeclaration => RequirementType::BeneficialOwnership,
        }
    }
}

text_enum!(
    /// The canonical KYC case lifecycle (see `kyc_allowed`).
    KycStatus {
        Draft = "DRAFT",
        EvidenceRequired = "EVIDENCE_REQUIRED",
        Submitted = "SUBMITTED",
        UnderReview = "UNDER_REVIEW",
        MoreEvidenceRequired = "MORE_EVIDENCE_REQUIRED",
        Approved = "APPROVED",
        Rejected = "REJECTED",
        Expired = "EXPIRED",
        Revoked = "REVOKED",
    }
);

text_enum!(SubjectKind {
    Individual = "INDIVIDUAL",
    Organization = "ORGANIZATION",
});

text_enum!(RequirementScope {
    Base = "BASE",
    ProviderExtension = "PROVIDER_EXTENSION",
});

text_enum!(TenantClassification {
    FirstParty = "FIRST_PARTY",
    External = "EXTERNAL",
});

text_enum!(VerifierKind {
    Deterministic = "DETERMINISTIC",
    Manual = "MANUAL",
    System = "SYSTEM",
});

text_enum!(DecisionKind {
    Created = "CREATED",
    EvidenceCollection = "EVIDENCE_COLLECTION",
    Submission = "SUBMISSION",
    ReviewStarted = "REVIEW_STARTED",
    Verification = "VERIFICATION",
    ManualReview = "MANUAL_REVIEW",
    Revalidation = "REVALIDATION",
    Expiry = "EXPIRY",
    Revocation = "REVOCATION",
    AppealOpened = "APPEAL_OPENED",
    AppealResolved = "APPEAL_RESOLVED",
});

text_enum!(AppealStatus {
    Open = "OPEN",
    Upheld = "UPHELD",
    Overturned = "OVERTURNED",
});

text_enum!(
    /// A manual reviewer's explicit decision.
    ReviewDecision {
        Approve = "APPROVE",
        Reject = "REJECT",
        RequestEvidence = "REQUEST_EVIDENCE",
    }
);

text_enum!(
    /// Eligibility outcomes (explainable, with reasons).
    EligibilityOutcome {
        Eligible = "ELIGIBLE",
        KycRequired = "KYC_REQUIRED",
        AdditionalEvidenceRequired = "ADDITIONAL_EVIDENCE_REQUIRED",
        PendingReview = "PENDING_REVIEW",
        Denied = "DENIED",
        UnsupportedCapability = "UNSUPPORTED_CAPABILITY",
        LimitExceeded = "LIMIT_EXCEEDED",
        RiskRestricted = "RISK_RESTRICTED",
    }
);

use KycStatus as K;

/// The KYC edges (mirrored by `number_kyc_transition_allowed`). `APPROVED → APPROVED` is
/// a revalidation renewal.
pub fn kyc_allowed(from: KycStatus, to: KycStatus) -> bool {
    matches!(
        (from, to),
        (K::Draft, K::EvidenceRequired | K::Expired)
            | (K::EvidenceRequired, K::Submitted | K::Expired)
            | (K::Submitted, K::UnderReview)
            | (
                K::UnderReview,
                K::Approved | K::Rejected | K::MoreEvidenceRequired
            )
            | (K::MoreEvidenceRequired, K::Submitted | K::Expired)
            | (
                K::Approved,
                K::Expired | K::Revoked | K::MoreEvidenceRequired | K::Approved
            )
            | (K::Rejected, K::UnderReview)
            | (K::Revoked, K::UnderReview)
    )
}
