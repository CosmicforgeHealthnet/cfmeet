//! The provider-independent KYC verifier contract (Part IV Workstream F §7) and its two
//! adapters.
//!
//! * [`KycVerifier`] receives a [`VerificationRequest`] — the case, its requirement items
//!   and evidence SUMMARIES (kinds and requirement types, never content) — and answers a
//!   [`VerifierOutcome`]. An outcome may be `Unknown` (a timeout after the request may have
//!   been received): the caller then asks [`KycVerifier::status`] for the same
//!   `request_ref` instead of re-submitting (no duplicate external check). Every definite
//!   response carries a `response_ref`; a response applied once is ignored when repeated.
//! * [`DeterministicKycVerifier`] — reproducible scripted outcomes per subject (approve,
//!   reject, missing / more evidence, short validity for expiry, unknown then resolved,
//!   duplicate responses, different revalidation outcomes). It never claims to be an
//!   external KYC provider and is refused in staging / production configuration.
//! * [`ManualKycVerifier`] — queues every case for an authenticated, privileged human
//!   reviewer (`PendingReview`); the reviewer's explicit, reason-coded, evidence-referencing
//!   decision is applied by `KycService::review_case`, under the same completeness rules
//!   (a manual approval never bypasses a mandatory requirement).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use uuid::Uuid;

use super::{
    requirements::RequirementItem,
    types::{EvidenceKind, RequirementType, SubjectKind, VerifierKind},
};

/// What the verifier needs to know (no evidence content, no number).
#[derive(Debug, Clone)]
pub struct VerificationRequest {
    pub request_ref: Uuid,
    pub case_id: Uuid,
    /// SUBMISSION or REVALIDATION.
    pub revalidation: bool,
    pub subject_kind: SubjectKind,
    pub subject_ref: Uuid,
    pub country: String,
    pub items: Vec<RequirementItem>,
    pub evidence: Vec<EvidenceSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSummary {
    pub evidence_id: Uuid,
    pub kind: EvidenceKind,
    pub requirement_type: RequirementType,
}

impl VerificationRequest {
    /// Mandatory requirement types without accepted ACTIVE evidence.
    pub fn missing(&self) -> Vec<RequirementType> {
        missing_requirements(&self.items, &self.evidence)
    }
}

/// Mandatory requirements not covered by an accepted evidence kind.
pub fn missing_requirements(
    items: &[RequirementItem],
    evidence: &[EvidenceSummary],
) -> Vec<RequirementType> {
    items
        .iter()
        .filter(|i| i.mandatory)
        .filter(|i| {
            !evidence.iter().any(|e| {
                e.requirement_type == i.requirement_type && i.accepted_evidence.contains(&e.kind)
            })
        })
        .map(|i| i.requirement_type)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierOutcome {
    Approved {
        response_ref: String,
        valid_for_days: Option<i64>,
    },
    Rejected {
        response_ref: String,
        reason: String,
    },
    /// Some evidence is insufficient: more is needed.
    MoreEvidence {
        response_ref: String,
        missing: Vec<RequirementType>,
    },
    /// Mandatory evidence was not provided at all.
    MissingEvidence {
        response_ref: String,
        missing: Vec<RequirementType>,
    },
    /// Accepted for (human) review: the decision comes later.
    PendingReview,
    /// The outcome is unknown (timeout after possible receipt): ask `status`.
    Unknown { reason: String },
}

#[async_trait::async_trait]
pub trait KycVerifier: Send + Sync {
    fn kind(&self) -> VerifierKind;
    async fn verify(&self, request: &VerificationRequest) -> VerifierOutcome;
    /// The outcome of an earlier request (never a new check).
    async fn status(&self, request_ref: Uuid) -> VerifierOutcome;
}

/// A scripted outcome for one subject (deterministic verifier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scenario {
    /// Approve when mandatory evidence is complete, else report it missing (default).
    Standard,
    Approve {
        valid_for_days: i64,
    },
    Reject {
        reason: String,
    },
    MoreEvidence {
        missing: Vec<RequirementType>,
    },
    /// `Unknown` on verify; `status` then answers the inner scenario.
    UnknownThen(Box<Scenario>),
    /// Approve, and answer every status poll with the SAME response (duplicate delivery).
    DuplicateApproval,
}

#[derive(Default)]
struct DeterministicState {
    scenarios: HashMap<Uuid, Scenario>,
    revalidation: HashMap<Uuid, Scenario>,
    /// request_ref → the scenario it was received with and its request.
    received: HashMap<Uuid, (Scenario, VerificationRequest)>,
    calls: Vec<(Uuid, &'static str)>,
}

/// Reproducible, scriptable verifier (not an external KYC provider).
#[derive(Clone, Default)]
pub struct DeterministicKycVerifier {
    state: Arc<Mutex<DeterministicState>>,
}

impl DeterministicKycVerifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scripts the submission outcome of a subject.
    pub fn script(&self, subject_ref: Uuid, scenario: Scenario) {
        self.state
            .lock()
            .unwrap()
            .scenarios
            .insert(subject_ref, scenario);
    }

    /// Scripts the revalidation outcome of a subject.
    pub fn script_revalidation(&self, subject_ref: Uuid, scenario: Scenario) {
        self.state
            .lock()
            .unwrap()
            .revalidation
            .insert(subject_ref, scenario);
    }

    /// `(request_ref, "verify" | "status")` calls received, in order.
    pub fn calls(&self) -> Vec<(Uuid, &'static str)> {
        self.state.lock().unwrap().calls.clone()
    }

    fn decide(scenario: &Scenario, request: &VerificationRequest, polled: bool) -> VerifierOutcome {
        let response_ref = format!("det-{}", request.request_ref.simple());
        match scenario {
            Scenario::Standard => {
                let missing = request.missing();
                if missing.is_empty() {
                    VerifierOutcome::Approved {
                        response_ref,
                        valid_for_days: None,
                    }
                } else {
                    VerifierOutcome::MissingEvidence {
                        response_ref,
                        missing,
                    }
                }
            }
            Scenario::Approve { valid_for_days } => VerifierOutcome::Approved {
                response_ref,
                valid_for_days: Some(*valid_for_days),
            },
            Scenario::Reject { reason } => VerifierOutcome::Rejected {
                response_ref,
                reason: reason.clone(),
            },
            Scenario::MoreEvidence { missing } => VerifierOutcome::MoreEvidence {
                response_ref,
                missing: missing.clone(),
            },
            Scenario::UnknownThen(inner) if !polled => {
                let _ = inner;
                VerifierOutcome::Unknown {
                    reason: "verifier timeout after receipt".into(),
                }
            }
            Scenario::UnknownThen(inner) => Self::decide(inner, request, false),
            Scenario::DuplicateApproval => VerifierOutcome::Approved {
                response_ref,
                valid_for_days: None,
            },
        }
    }
}

#[async_trait::async_trait]
impl KycVerifier for DeterministicKycVerifier {
    fn kind(&self) -> VerifierKind {
        VerifierKind::Deterministic
    }

    async fn verify(&self, request: &VerificationRequest) -> VerifierOutcome {
        let mut state = self.state.lock().unwrap();
        state.calls.push((request.request_ref, "verify"));
        let table = if request.revalidation {
            &state.revalidation
        } else {
            &state.scenarios
        };
        let scenario = table
            .get(&request.subject_ref)
            .cloned()
            .unwrap_or(Scenario::Standard);
        state
            .received
            .insert(request.request_ref, (scenario.clone(), request.clone()));
        Self::decide(&scenario, request, false)
    }

    async fn status(&self, request_ref: Uuid) -> VerifierOutcome {
        let mut state = self.state.lock().unwrap();
        state.calls.push((request_ref, "status"));
        match state.received.get(&request_ref) {
            Some((scenario, request)) => Self::decide(scenario, request, true),
            None => VerifierOutcome::Unknown {
                reason: "request not received".into(),
            },
        }
    }
}

/// Every case waits for an authenticated, privileged human reviewer.
#[derive(Debug, Clone, Copy, Default)]
pub struct ManualKycVerifier;

#[async_trait::async_trait]
impl KycVerifier for ManualKycVerifier {
    fn kind(&self) -> VerifierKind {
        VerifierKind::Manual
    }

    async fn verify(&self, _request: &VerificationRequest) -> VerifierOutcome {
        VerifierOutcome::PendingReview
    }

    async fn status(&self, _request_ref: Uuid) -> VerifierOutcome {
        VerifierOutcome::PendingReview
    }
}
