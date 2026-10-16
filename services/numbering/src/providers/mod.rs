//! The provider-neutral numbering provider contract (Part IV Workstream G §11).
//!
//! [`NumberingProvider`] is the ONLY way Numbering talks to a carrier / number provider:
//! inventory search, number inspection (read-back), reservation holds, provisioning,
//! voice / SMS route configuration, suspension, restoration, release, optional porting,
//! callback verification / normalization and health / capability reporting. Every
//! provider-affecting call answers a [`ProviderOutcome`] that distinguishes definite
//! success, definite failure (with whether anything was transmitted), unsupported
//! operations and UNKNOWN outcomes (a timeout after the request may have been acted on).
//!
//! Requests and responses are canonical: CF Meet identifiers, canonical number types and
//! capabilities, and the provider's opaque `external_ref`. Provider-specific bodies,
//! credentials and secrets stay inside each adapter. Every request carries `request_id`
//! (the durable command id), which adapters must forward as the provider-side idempotency
//! key, so a retry after a definite not-transmitted failure cannot duplicate an effect.
//!
//! * [`registry`] — registered adapters, discovery, environment restrictions.
//! * [`deterministic`] — the deterministic synthetic provider with scripted faults.
//! * [`commands`] — durable command execution (intent → dispatch → evidence → transition).
//! * [`events`] — authenticated callback ingestion.
//! * [`reconcile`] — read-back reconciliation of uncertain commands.
//! * [`certification`] — the reusable adapter certification suite.

pub mod certification;
pub mod commands;
pub mod deterministic;
pub mod events;
pub mod incidents;
pub mod reconcile;
pub mod registry;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::numbers::{Capability, FailureCategory, NumberType};

macro_rules! simple_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $s)] $variant,)+
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
pub(crate) use simple_enum;

simple_enum!(
    /// Every operation of the contract (capability discovery).
    ProviderOperation {
        Search = "SEARCH",
        Inspect = "INSPECT",
        Reserve = "RESERVE",
        Provision = "PROVISION",
        ConfigureVoice = "CONFIGURE_VOICE",
        ConfigureSms = "CONFIGURE_SMS",
        Suspend = "SUSPEND",
        Restore = "RESTORE",
        Release = "RELEASE",
        PortIn = "PORT_IN",
        Events = "EVENTS",
        Health = "HEALTH",
    }
);

simple_enum!(
    /// The provider-affecting operations executed as durable commands.
    CommandOperation {
        Reserve = "RESERVE",
        /// Gives a reservation's provider hold back (cancellation / expiry).
        ReleaseHold = "RELEASE_HOLD",
        Provision = "PROVISION",
        ConfigureVoice = "CONFIGURE_VOICE",
        ConfigureSms = "CONFIGURE_SMS",
        Suspend = "SUSPEND",
        Restore = "RESTORE",
        Release = "RELEASE",
        PortIn = "PORT_IN",
    }
);

impl CommandOperation {
    pub fn provider_operation(&self) -> ProviderOperation {
        match self {
            CommandOperation::Reserve => ProviderOperation::Reserve,
            CommandOperation::ReleaseHold => ProviderOperation::Release,
            CommandOperation::Provision => ProviderOperation::Provision,
            CommandOperation::ConfigureVoice => ProviderOperation::ConfigureVoice,
            CommandOperation::ConfigureSms => ProviderOperation::ConfigureSms,
            CommandOperation::Suspend => ProviderOperation::Suspend,
            CommandOperation::Restore => ProviderOperation::Restore,
            CommandOperation::Release => ProviderOperation::Release,
            CommandOperation::PortIn => ProviderOperation::PortIn,
        }
    }
}

simple_enum!(
    /// A number's state as the provider reports it (read-back).
    ProviderNumberStatus {
        Available = "AVAILABLE",
        Held = "HELD",
        Provisioned = "PROVISIONED",
        Suspended = "SUSPENDED",
        Released = "RELEASED",
        /// Held or owned by someone else / withdrawn from sale.
        Unavailable = "UNAVAILABLE",
        NotFound = "NOT_FOUND",
    }
);

simple_enum!(
    /// Canonical provider event kinds (mirrored by `ck_number_provider_events_kind`).
    ProviderEventKind {
        HoldConfirmed = "HOLD_CONFIRMED",
        HoldExpired = "HOLD_EXPIRED",
        Provisioned = "PROVISIONED",
        RouteConfigured = "ROUTE_CONFIGURED",
        Suspended = "SUSPENDED",
        Restored = "RESTORED",
        Released = "RELEASED",
        CapabilityChanged = "CAPABILITY_CHANGED",
        NumberRevoked = "NUMBER_REVOKED",
    }
);

/// What an adapter supports and where it may run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDescriptor {
    /// Internal provider key (never exposed to customers).
    pub provider_id: String,
    /// `true` for test providers: synthetic inventory only, refused in staging / production.
    pub synthetic: bool,
    /// Whether numbers of this provider can carry real PSTN traffic.
    pub pstn_routable: bool,
    pub operations: Vec<ProviderOperation>,
    pub countries: Vec<String>,
    pub number_types: Vec<NumberType>,
}

impl ProviderDescriptor {
    pub fn supports(&self, op: ProviderOperation) -> bool {
        self.operations.contains(&op)
    }
}

/// An inventory search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryQuery {
    pub country: String,
    pub number_type: Option<NumberType>,
    /// Every listed capability must be offered.
    pub capabilities: Vec<Capability>,
    pub region: Option<String>,
    pub limit: usize,
}

/// One number offered by a provider (normalized).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryOffer {
    pub external_ref: String,
    /// Internal only: E.164 never leaves Numbering's internal API.
    pub e164: String,
    pub country: String,
    pub region: Option<String>,
    pub number_type: NumberType,
    pub capabilities: Vec<Capability>,
    pub unsupported: Vec<Capability>,
    pub kyc_required: bool,
    pub monthly_cost_minor: Option<i64>,
    pub currency: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub fresh_for: chrono::Duration,
}

/// Read-back of one number at the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNumberState {
    pub external_ref: String,
    pub status: ProviderNumberStatus,
    /// The CF Meet request (command) holding / owning it, when it is ours.
    pub held_by_request: Option<Uuid>,
    /// Whether this CF Meet account holds / owns it.
    pub owned_by_us: bool,
    pub capabilities: Vec<Capability>,
    pub voice_route: Option<Uuid>,
    pub sms_route: Option<Uuid>,
    pub hold_expires_at: Option<DateTime<Utc>>,
    /// Monotonic provider-side version (stale read-backs are detectable).
    pub sequence: i64,
    pub observed_at: DateTime<Utc>,
}

/// The operation-specific part of a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationKind {
    Reserve {
        hold_seconds: i64,
    },
    /// The provider releases a hold (provider operation `release` on a held number).
    ReleaseHold,
    Provision {
        capabilities: Vec<Capability>,
    },
    ConfigureVoice {
        route_ref: Uuid,
    },
    ConfigureSms {
        route_ref: Uuid,
    },
    Suspend {
        reason_code: String,
    },
    Restore,
    Release,
    PortIn {
        losing_carrier_ref: String,
    },
}

impl OperationKind {
    pub fn operation(&self) -> CommandOperation {
        match self {
            OperationKind::Reserve { .. } => CommandOperation::Reserve,
            OperationKind::ReleaseHold => CommandOperation::ReleaseHold,
            OperationKind::Provision { .. } => CommandOperation::Provision,
            OperationKind::ConfigureVoice { .. } => CommandOperation::ConfigureVoice,
            OperationKind::ConfigureSms { .. } => CommandOperation::ConfigureSms,
            OperationKind::Suspend { .. } => CommandOperation::Suspend,
            OperationKind::Restore => CommandOperation::Restore,
            OperationKind::Release => CommandOperation::Release,
            OperationKind::PortIn { .. } => CommandOperation::PortIn,
        }
    }
}

/// A canonical provider-affecting request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationRequest {
    /// The durable command id: the provider-side idempotency key.
    pub request_id: Uuid,
    pub external_ref: String,
    pub operation: OperationKind,
}

/// A definite success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationReceipt {
    /// The provider's reference for this request (evidence).
    pub provider_request_ref: String,
    pub external_ref: String,
    /// The number's state right after the operation, when the provider reports it.
    pub state: Option<ProviderNumberState>,
}

/// A definite failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    /// Stable UPPER_SNAKE_CASE code (e.g. `ALREADY_OWNED`, `NOT_AVAILABLE`).
    pub code: String,
    pub category: FailureCategory,
    /// `false`: the request certainly never reached the provider (safe to retry).
    pub transmitted: bool,
    pub retryable: bool,
}

impl ProviderFailure {
    pub fn rejected(code: &str, category: FailureCategory) -> Self {
        Self {
            code: code.into(),
            category,
            transmitted: true,
            retryable: false,
        }
    }

    pub fn not_transmitted(code: &str) -> Self {
        Self {
            code: code.into(),
            category: FailureCategory::ProviderUnavailable,
            transmitted: false,
            retryable: true,
        }
    }
}

/// The outcome of a provider call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOutcome<T> {
    Succeeded(T),
    Failed(ProviderFailure),
    Unsupported,
    /// The request may have been acted on: never retried blindly; read back instead.
    Unknown {
        reason: String,
    },
}

impl<T> ProviderOutcome<T> {
    pub fn label(&self) -> &'static str {
        match self {
            ProviderOutcome::Succeeded(_) => "SUCCEEDED",
            ProviderOutcome::Failed(f) if !f.transmitted => "NOT_TRANSMITTED",
            ProviderOutcome::Failed(_) => "FAILED",
            ProviderOutcome::Unsupported => "UNSUPPORTED",
            ProviderOutcome::Unknown { .. } => "UNKNOWN",
        }
    }
}

/// An inbound provider callback, as received (raw body, headers).
#[derive(Debug, Clone)]
pub struct CallbackRequest {
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl CallbackRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        let mut values = self
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name));
        match (values.next(), values.next()) {
            (Some((_, v)), None) => Some(v.as_str()),
            _ => None,
        }
    }
}

/// A verified, normalized provider event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedProviderEvent {
    /// Stable provider event id (dedupe key, constant across redeliveries).
    pub provider_event_id: String,
    pub kind: ProviderEventKind,
    pub external_ref: String,
    /// The CF Meet request (command) it answers, when the provider echoes it.
    pub request_id: Option<Uuid>,
    /// Provider-side version of the number (out-of-order detection).
    pub sequence: Option<i64>,
    pub occurred_at: DateTime<Utc>,
    /// CAPABILITY_CHANGED: the complete current capability set.
    pub capabilities: Option<Vec<Capability>>,
}

/// Why a callback was refused (nothing is stored or applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CallbackRejection {
    #[error("the callback is not authenticated")]
    Unauthenticated,
    #[error("the callback is outside the replay window")]
    Replayed,
    #[error("the callback is malformed")]
    Malformed,
}

/// Provider health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealth {
    pub available: bool,
    pub detail: String,
}

/// The canonical provider contract.
#[async_trait::async_trait]
pub trait NumberingProvider: Send + Sync {
    fn descriptor(&self) -> ProviderDescriptor;

    async fn search(&self, query: &InventoryQuery) -> ProviderOutcome<Vec<InventoryOffer>>;
    /// Authoritative read-back (reconciliation evidence).
    async fn inspect(&self, external_ref: &str) -> ProviderOutcome<ProviderNumberState>;
    async fn reserve(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt>;
    async fn provision(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt>;
    async fn configure_voice(
        &self,
        request: &OperationRequest,
    ) -> ProviderOutcome<OperationReceipt>;
    async fn configure_sms(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt>;
    async fn suspend(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt>;
    async fn restore(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt>;
    async fn release(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt>;
    /// Optional: providers without porting answer `Unsupported`.
    async fn port_in(&self, _request: &OperationRequest) -> ProviderOutcome<OperationReceipt> {
        ProviderOutcome::Unsupported
    }
    /// Authenticates (signature, replay window) and normalizes one callback.
    fn verify_event(
        &self,
        request: &CallbackRequest,
        now: DateTime<Utc>,
    ) -> Result<NormalizedProviderEvent, CallbackRejection>;
    async fn health(&self) -> ProviderHealth;
}

/// Routes a request to the adapter method of its operation.
pub async fn dispatch(
    provider: &dyn NumberingProvider,
    request: &OperationRequest,
) -> ProviderOutcome<OperationReceipt> {
    match request.operation {
        OperationKind::Reserve { .. } => provider.reserve(request).await,
        OperationKind::ReleaseHold => provider.release(request).await,
        OperationKind::Provision { .. } => provider.provision(request).await,
        OperationKind::ConfigureVoice { .. } => provider.configure_voice(request).await,
        OperationKind::ConfigureSms { .. } => provider.configure_sms(request).await,
        OperationKind::Suspend { .. } => provider.suspend(request).await,
        OperationKind::Restore => provider.restore(request).await,
        OperationKind::Release => provider.release(request).await,
        OperationKind::PortIn { .. } => provider.port_in(request).await,
    }
}
