//! KYC and eligibility (Part IV Workstream F).
//!
//! * [`requirements`] — versioned requirement sets per country / jurisdiction, number type,
//!   ownership class, tenant classification and provider extension; unknown mandatory
//!   requirements fail closed.
//! * [`cases`] — the KYC case lifecycle, protected evidence ([`evidence`]), verification
//!   through one [`verifier::KycVerifier`] contract (deterministic and audited manual
//!   adapters), expiry, revalidation, revocation, appeals and immutable history.
//! * [`crate::numbers::eligibility`] evaluates whether a request may proceed (it never
//!   reserves or provisions).

pub mod cases;
pub mod evidence;
pub mod requirements;
pub mod types;
pub mod verifier;

pub use cases::{
    AssignmentKycStatus, KycCaseRow, KycService, KycSettings, ManualReview, NewCase,
    VerificationHandler,
};
pub use types::*;
