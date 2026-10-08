//! Telecom interconnect provider boundary (Part E §42–§43).
//!
//! Business code depends ONLY on [`TelecomInterconnectProvider`]. Provider-specific
//! adapters live in sub-modules of this directory (`sip_trunk`, `deterministic`); their
//! ids, payloads, status strings and credentials never leave Telephony and never reach a
//! CF Meet contract. A provider ADVERTISES capabilities; nothing is assumed from its
//! vendor.
//!
//! Origination results are three-valued: [`OriginateOutcome::Accepted`],
//! [`OriginateOutcome::DefiniteFailure`] and [`OriginateOutcome::UnknownOutcome`]. An
//! unknown outcome is never collapsed into a failure: the request may have reached the
//! provider and produced a paid call.

pub mod deterministic;
pub mod sip_trunk;

use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::domain::{
    EndReason, FailureCategory, Health, LegStatus, NormalizedEvent, ProviderCapability,
};

/// A canonical E.164 telephone number held only inside the Telephony execution boundary:
/// redacted `Debug`, no `Display`, no `Serialize`, zeroized on drop.
#[derive(Clone)]
pub struct ExecutionNumber(Zeroizing<String>);

impl ExecutionNumber {
    /// Accepts canonical E.164 only (`+` and 7–15 digits, no leading zero).
    pub fn parse(value: &str) -> Option<Self> {
        let digits = value.strip_prefix('+')?;
        let ok = (7..=15).contains(&digits.len())
            && digits.bytes().all(|b| b.is_ascii_digit())
            && !digits.starts_with('0');
        ok.then(|| Self(Zeroizing::new(value.to_string())))
    }

    /// The plaintext, for the provider / edge request only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ExecutionNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExecutionNumber(<redacted>)")
    }
}

/// One origination request: only what the interconnect needs — never the Call domain.
#[derive(Debug, Clone)]
pub struct OriginateRequest {
    /// Telephony route reference (provider-neutral id) and its SIP peer configuration
    /// reference, if any.
    pub route_ref: String,
    pub sip_peer_config_ref: Option<String>,
    /// The authorized platform / relay identity presented as caller id (selected
    /// server-side; never from a request body).
    pub caller_identity: ExecutionNumber,
    /// The resolved destination (exists only for this execution).
    pub destination: ExecutionNumber,
    pub command_id: Uuid,
    /// Persisted before the request; lets events and read-back find the call.
    pub correlation_key: String,
    pub idempotency_key: String,
    pub ring_timeout: Duration,
    pub request_timeout: Duration,
}

/// Why an outcome is unknown (bounded, safe codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// The request was (possibly) transmitted and no answer arrived in time.
    TimeoutAfterSend,
    /// The connection broke after (part of) the request was sent.
    ConnectionResetAfterSend,
    /// The provider answered with something that is not a well-formed result.
    MalformedSuccess,
    /// The worker lost its lease after transmitting.
    WorkerInterrupted,
}

impl UnknownReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            UnknownReason::TimeoutAfterSend => "TIMEOUT_AFTER_SEND",
            UnknownReason::ConnectionResetAfterSend => "CONNECTION_RESET_AFTER_SEND",
            UnknownReason::MalformedSuccess => "MALFORMED_SUCCESS",
            UnknownReason::WorkerInterrupted => "WORKER_INTERRUPTED",
        }
    }
}

/// Normalized origination result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginateOutcome {
    /// The provider accepted the request and created the call object.
    Accepted {
        provider_call_ref: String,
        provider_idempotency_ref: Option<String>,
    },
    /// The provider definitely did NOT create a call (or the request never left this
    /// process: `transmitted = false`, so a retry is safe).
    DefiniteFailure {
        category: FailureCategory,
        reason: EndReason,
        transmitted: bool,
    },
    /// The provider may or may not have created a (paid) call.
    UnknownOutcome { reason: UnknownReason },
}

/// Result of a control action (terminate / hold / resume).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOutcome {
    Done,
    /// The provider object no longer exists (nothing to do).
    AlreadyGone,
    DefiniteFailure,
    Unknown,
}

/// Status read-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryOutcome {
    /// The provider has the call in this normalized state.
    Active {
        provider_call_ref: String,
        status: LegStatus,
    },
    /// The provider has the call and it is over.
    Ended {
        provider_call_ref: String,
        reason: EndReason,
    },
    /// The provider certainly has no call for this correlation (never created).
    Absent,
    /// The provider cannot tell (yet).
    Unknown,
}

/// How to look a call up at the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryKey {
    pub provider_call_ref: Option<String>,
    pub correlation_key: String,
    /// When the request was (possibly) transmitted. An adapter may answer `Absent` only
    /// when its evidence provably covers this instant; otherwise it answers `Unknown`.
    pub transmitted_at: Option<DateTime<Utc>>,
}

/// Provider capacity as reported / configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityReport {
    pub max_concurrent: Option<u32>,
    pub in_use: Option<u32>,
}

/// A raw provider ingress (HTTP callback) before authentication. Bounded by the caller.
#[derive(Clone)]
pub struct ProviderIngress {
    pub provider_id: String,
    /// The secret path token of the callback URL, if the provider authenticates that way.
    pub path_token: Option<String>,
    /// Selected request headers (lower-case names).
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub received_at: DateTime<Utc>,
}

impl fmt::Debug for ProviderIngress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderIngress")
            .field("provider_id", &self.provider_id)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// An inbound call offer as the interconnect observed it.
#[derive(Debug, Clone)]
pub struct InboundObservation {
    /// The platform relay number that was called (public, platform-owned).
    pub called: ExecutionNumber,
    /// The presented caller number, if any (protected immediately by Privacy Relay).
    pub caller: Option<ExecutionNumber>,
    pub provider_started_at: Option<DateTime<Utc>>,
}

/// One normalized provider event (adapters produce these; provider strings stay inside
/// `restricted_detail`, which is bounded and never forwarded).
#[derive(Debug, Clone)]
pub struct ProviderEvent {
    pub provider_event_id: String,
    pub provider_call_ref: Option<String>,
    pub correlation_key: Option<String>,
    pub event: NormalizedEvent,
    pub occurred_at: DateTime<Utc>,
    /// Provider sequence, when the provider guarantees one.
    pub sequence: Option<i64>,
    /// Restricted, bounded, redacted detail (e.g. a normalized cause code).
    pub restricted_detail: BTreeMap<String, String>,
    /// For INBOUND_OFFERED only.
    pub inbound: Option<InboundObservation>,
    /// For LEG_DTMF only: the digits (not stored).
    pub dtmf: Option<String>,
    /// Billed cost when the provider reports it on a final event.
    pub provider_cost_minor: Option<(i64, String)>,
}

/// Why an ingress was refused (bounded, safe codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventRejection {
    Unauthenticated,
    Malformed,
    UnknownProvider,
}

impl EventRejection {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventRejection::Unauthenticated => "UNAUTHENTICATED",
            EventRejection::Malformed => "MALFORMED",
            EventRejection::UnknownProvider => "UNKNOWN_PROVIDER",
        }
    }
}

/// One live call an interconnect reports in its inventory (restricted: provider refs
/// never leave Telephony).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCall {
    pub provider_call_ref: String,
    pub correlation_key: Option<String>,
    pub status: LegStatus,
}

/// The provider-independent interconnect contract.
#[async_trait]
pub trait TelecomInterconnectProvider: Send + Sync {
    /// Configured provider id (`telecom_providers.id`).
    fn id(&self) -> &str;

    /// Advertised capabilities.
    fn capabilities(&self) -> &[ProviderCapability];

    fn supports(&self, capability: ProviderCapability) -> bool {
        self.capabilities().contains(&capability)
    }

    async fn originate(&self, request: OriginateRequest) -> OriginateOutcome;

    async fn terminate(&self, provider_call_ref: &str) -> ControlOutcome;

    /// Hold primitive (media anchored at the edge for SIP-trunk interconnects).
    async fn hold(&self, _provider_call_ref: &str) -> ControlOutcome {
        ControlOutcome::DefiniteFailure
    }

    /// Resume primitive.
    async fn resume(&self, _provider_call_ref: &str) -> ControlOutcome {
        ControlOutcome::DefiniteFailure
    }

    async fn query_call(&self, key: &QueryKey) -> QueryOutcome;

    /// Inventory read-back: every call the interconnect currently holds, if it can list
    /// them (`None`: not supported / not available now). Used by reconciliation to find
    /// provider calls without a valid internal binding.
    async fn list_active(&self) -> Option<Vec<ActiveCall>> {
        None
    }

    async fn health(&self) -> Health;

    async fn capacity(&self) -> CapacityReport;

    /// Authenticates an HTTP callback (signature / token / peer). No state is touched
    /// before this returns Ok.
    fn verify_event(&self, ingress: &ProviderIngress) -> Result<(), EventRejection>;

    /// Parses an authenticated callback into normalized events (strict parser).
    fn normalize_event(
        &self,
        ingress: &ProviderIngress,
    ) -> Result<Vec<ProviderEvent>, EventRejection>;
}

/// All configured providers, by id.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Arc<dyn TelecomInterconnectProvider>>,
}

impl fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.providers.keys()).finish()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, provider: Arc<dyn TelecomInterconnectProvider>) -> Self {
        self.providers.insert(provider.id().to_string(), provider);
        self
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn TelecomInterconnectProvider>> {
        self.providers.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.providers.keys()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_numbers_are_canonical_and_redacted() {
        let n = ExecutionNumber::parse("+2348031234567").unwrap();
        assert_eq!(format!("{n:?}"), "ExecutionNumber(<redacted>)");
        assert_eq!(n.expose(), "+2348031234567");
        for bad in [
            "2348031234567",
            "+0123456789",
            "+12",
            "+234 803",
            "sip:+2348031234567",
        ] {
            assert!(ExecutionNumber::parse(bad).is_none(), "{bad}");
        }
    }
}

/// Why the interconnect configuration cannot be loaded.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("database error ({0})")]
    Database(String),
    #[error(
        "provider {0}: the DETERMINISTIC_TEST adapter is refused in the {1} runtime environment"
    )]
    TestAdapterRefused(String, String),
    #[error("provider {0}: {1}")]
    Invalid(String, String),
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct ProviderConfigRow {
    id: String,
    adapter: String,
    capabilities: Vec<String>,
    credential_ref: Option<String>,
}

/// Builds the registry from `telecom_providers` rows. Credentials come from secret FILES
/// in `TELECOM_PROVIDER_SECRETS_DIR` named by `credential_ref` (never from the database,
/// never inline). The deterministic test adapter is refused outside `local` / `test`.
pub async fn registry_from_database(
    db: &sea_orm::DatabaseConnection,
    environment: cf_service_auth::RuntimeEnvironment,
    edge: Arc<dyn crate::sip::SipMediaEdge>,
) -> Result<ProviderRegistry, RegistryError> {
    use sea_orm::FromQueryResult;
    let rows = ProviderConfigRow::find_by_statement(crate::db::stmt(
        "SELECT id, adapter, capabilities, credential_ref FROM telecom_providers ORDER BY id",
        vec![],
    ))
    .all(db)
    .await
    .map_err(|e| RegistryError::Database(crate::error::summarize_db_error(&e).0))?;
    let secrets_dir = std::env::var("TELECOM_PROVIDER_SECRETS_DIR").ok();
    let read_secret = |id: &str, r: &Option<String>| -> Result<Vec<u8>, RegistryError> {
        let (Some(dir), Some(name)) = (&secrets_dir, r) else {
            return Err(RegistryError::Invalid(
                id.to_string(),
                "credential_ref and TELECOM_PROVIDER_SECRETS_DIR are required".into(),
            ));
        };
        let path = std::path::Path::new(dir).join(name);
        let bytes = std::fs::read(&path).map_err(|_| {
            RegistryError::Invalid(id.to_string(), "the credential file cannot be read".into())
        })?;
        let trimmed: Vec<u8> = String::from_utf8_lossy(&bytes).trim().as_bytes().to_vec();
        if trimmed.len() < 32 {
            return Err(RegistryError::Invalid(
                id.to_string(),
                "the credential is too short".into(),
            ));
        }
        Ok(trimmed)
    };
    let mut registry = ProviderRegistry::new();
    for row in rows {
        let caps: Vec<ProviderCapability> = row
            .capabilities
            .iter()
            .filter_map(|c| ProviderCapability::parse(c))
            .collect();
        match row.adapter.as_str() {
            "DETERMINISTIC_TEST" => {
                use cf_service_auth::RuntimeEnvironment as E;
                if !matches!(environment, E::Local | E::Test) {
                    return Err(RegistryError::TestAdapterRefused(
                        row.id,
                        environment.to_string(),
                    ));
                }
                let secret = read_secret(&row.id, &row.credential_ref)?;
                registry = registry.with(Arc::new(deterministic::DeterministicProvider::new(
                    &row.id, &secret,
                )));
            }
            "SIP_TRUNK" => {
                registry = registry.with(Arc::new(sip_trunk::SipTrunkProvider::new(
                    &row.id,
                    caps,
                    edge.clone(),
                )));
            }
            other => {
                return Err(RegistryError::Invalid(
                    row.id,
                    format!("unknown adapter {other}"),
                ));
            }
        }
    }
    Ok(registry)
}
