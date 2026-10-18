//! The SMS provider boundary (Part IV Workstream M).
//!
//! The gateway talks to an SMS aggregator / carrier only through [`SmsProvider`]:
//! submission with an idempotency key, a status query by that key, and authenticated
//! callbacks (inbound messages and delivery receipts) normalized into [`Callback`].
//! Provider-specific identifiers stay inside this boundary and in
//! `sms_provider_bindings` — they are never public authority.
//!
//! Submission outcomes are deliberately four-valued: ACCEPTED (the provider holds the
//! message), REJECTED (refused for good), NOT_SENT (definitely not transmitted — safe to
//! retry) and UNKNOWN (it may have been transmitted: never resubmitted automatically;
//! only provider evidence resolves it).
//!
//! The deterministic adapter ([`deterministic`]) exists for local development and tests;
//! [`guard_runtime`] refuses it in staging and production. [`contract`] is the reusable
//! adapter contract suite every adapter must pass.

pub mod contract;
pub mod deterministic;

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use axum::http::HeaderMap;
use cf_service_auth::RuntimeEnvironment;
use chrono::{DateTime, Utc};
use zeroize::Zeroizing;

use crate::sms::segments::Encoding;

/// One submission (plaintext exists only here, in memory, for the provider request).
pub struct SubmitRequest {
    pub from: Zeroizing<String>,
    pub to: Zeroizing<String>,
    pub body: Zeroizing<String>,
    pub encoding: Encoding,
    pub segments: usize,
    /// Stable per transport: a provider with native idempotency returns the same message.
    pub idempotency_key: String,
    pub validity_until: DateTime<Utc>,
}

impl fmt::Debug for SubmitRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubmitRequest")
            .field("idempotency_key", &self.idempotency_key)
            .field("segments", &self.segments)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    Accepted {
        provider_message_ref: String,
        cost_minor: Option<i64>,
    },
    /// Refused for good (invalid destination, content refused, …).
    Rejected { code: &'static str },
    /// Definitely not transmitted (throttled, connection refused before sending, …).
    NotSent { code: &'static str },
    /// Possibly transmitted (timeout after sending, malformed answer, crash).
    Unknown { reason: &'static str },
}

/// A provider's normalized message status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptStatus {
    Accepted,
    Delivered,
    Failed,
    Expired,
    Unknown,
}

impl ReceiptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReceiptStatus::Accepted => "ACCEPTED",
            ReceiptStatus::Delivered => "DELIVERED",
            ReceiptStatus::Failed => "FAILED",
            ReceiptStatus::Expired => "EXPIRED",
            ReceiptStatus::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ACCEPTED" => ReceiptStatus::Accepted,
            "DELIVERED" => ReceiptStatus::Delivered,
            "FAILED" => ReceiptStatus::Failed,
            "EXPIRED" => ReceiptStatus::Expired,
            "UNKNOWN" => ReceiptStatus::Unknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryOutcome {
    /// The provider holds a message under the idempotency key.
    Found {
        provider_message_ref: String,
        status: ReceiptStatus,
    },
    /// The provider confirms it never received the key.
    NotFound,
    /// No answer (outage, unsupported).
    Unknown,
}

/// A received message (plaintext in memory only).
pub struct InboundMessage {
    pub event_ref: String,
    pub from: Zeroizing<String>,
    pub to: Zeroizing<String>,
    pub text: Zeroizing<String>,
    pub received_at: DateTime<Utc>,
}

impl fmt::Debug for InboundMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboundMessage")
            .field("event_ref", &self.event_ref)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub event_ref: String,
    pub provider_message_ref: String,
    pub status: ReceiptStatus,
    /// Bounded upper-snake provider error class (never provider text).
    pub error_code: Option<String>,
    pub cost_minor: Option<i64>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum Callback {
    Inbound(InboundMessage),
    Receipt(DeliveryReceipt),
}

impl Callback {
    pub fn event_ref(&self) -> &str {
        match self {
            Callback::Inbound(m) => &m.event_ref,
            Callback::Receipt(r) => &r.event_ref,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CallbackError {
    /// Signature / timestamp / identity refused (401, nothing parsed).
    #[error("callback authentication failed ({0})")]
    Unauthenticated(&'static str),
    /// Authenticated but not a valid callback (400).
    #[error("callback malformed ({0})")]
    Malformed(&'static str),
}

/// An SMS provider adapter.
#[async_trait]
pub trait SmsProvider: Send + Sync {
    /// Stable adapter instance id (`[a-z0-9-]{1,64}`), the namespace of its references.
    fn id(&self) -> &str;

    /// `true` for test-only adapters (refused in staging / production).
    fn test_only(&self) -> bool {
        false
    }

    /// Whether a repeated idempotency key returns the existing message (no second effect).
    fn native_idempotency(&self) -> bool;

    async fn submit(&self, request: SubmitRequest) -> SubmitOutcome;

    /// The provider's view of the message submitted under `idempotency_key`.
    async fn query(&self, idempotency_key: &str) -> QueryOutcome;

    /// Authenticates a callback BEFORE it is parsed (signature over the raw body,
    /// timestamp window). `now_unix` is the gateway's clock.
    fn authenticate_callback(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now_unix: i64,
    ) -> Result<(), CallbackError>;

    /// Parses an authenticated callback.
    fn parse_callback(&self, body: &[u8]) -> Result<Callback, CallbackError>;
}

pub type SharedProvider = Arc<dyn SmsProvider>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the test-only SMS provider is refused in the {0} runtime environment")]
pub struct TestProviderRefused(pub &'static str);

/// Refuses a test-only adapter in a protected runtime (staging / production).
pub fn guard_runtime(
    provider: &dyn SmsProvider,
    environment: RuntimeEnvironment,
) -> Result<(), TestProviderRefused> {
    match environment {
        RuntimeEnvironment::Staging if provider.test_only() => Err(TestProviderRefused("staging")),
        RuntimeEnvironment::Production if provider.test_only() => {
            Err(TestProviderRefused("production"))
        }
        _ => Ok(()),
    }
}

/// A provider message reference / event reference: `[A-Za-z0-9._:-]{1,128}`.
pub fn valid_reference(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// A provider error class: upper snake, bounded (anything else becomes PROVIDER_ERROR).
pub fn error_class(value: Option<&str>) -> Option<String> {
    value.map(|v| {
        let ok = (2..=64).contains(&v.len())
            && v.as_bytes()[0].is_ascii_uppercase()
            && v.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if ok {
            v.to_string()
        } else {
            "PROVIDER_ERROR".to_string()
        }
    })
}
