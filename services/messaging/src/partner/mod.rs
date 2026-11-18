//! CF Meet Platform "Build" (ADR 0062 / 0063; `docs/platform/build-internal-contracts.md`
//! §1, §2.2, §2.3, §4, §5, §6): partner devices of EXTERNAL tenants.
//!
//! A partner's end user (a Platform Access `EXTERNAL_APPLICATION_PRINCIPAL`) holds a CF
//! Meet user token (`cfut_<sbx|test|live>_<JWS>`) minted by the partner's server. Messaging
//! never verifies it itself: Platform Access does (`POST /internal/v1/user-tokens/verify`,
//! `platform-user-token.verify`), and a successful answer is reused for at most
//! [`VERIFY_CACHE_TTL`] keyed by its token id.
//!
//! * [`sessions`]: `POST /v1/messaging/user-sessions` exchanges the user token for a
//!   MANAGED-only *partner session device* (binding kind `PLATFORM_USER_TOKEN`) and a
//!   short-lived client token; the device records the token's minting credential (`crd`)
//!   and service account (`sva`).
//! * Partner session devices use a closed REST allowlist
//!   ([`crate::http::middleware::PARTNER_SESSION_DEVICE_ROUTES`]) and the realtime gateway;
//!   a PRIVATE_E2EE target is `404` on REST and `E2EE_DEVICE_REQUIRED` on realtime.
//! * Revocation: every validation of a partner session (tickets, REST, the periodic
//!   re-validation of open sockets) re-checks the minting credential with Platform Access
//!   (cached positively ≤ 30 s) and resolves the principal afresh; the identity change
//!   feed ([`revocation`]) revokes partner devices at once when their minting credential
//!   is revoked or their principal is suspended / deleted / has its sessions revoked.
//! * Origins: browsers of a partner website may connect when their `Origin` is one of the
//!   session's environment origins (§2.3), in addition to the static allow-list.
//! * [`push`]: partner push registrations and metadata-only wake-ups (§6 N-P2 / N-P4).
//!
//! Every capability is behind a default-off flag ([`PartnerSettings`]); with the flags off
//! nothing here is reachable and Messaging behaves exactly as before.

pub mod http;
pub mod push;
pub mod revocation;
pub mod sessions;

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    services, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::identity::{AuthorityUnavailable, EnvironmentKind, OwnerScope};

/// A positive user-token verification is reused at most this long (contract §2.2).
pub const VERIFY_CACHE_TTL: Duration = Duration::from_secs(30);
/// A positive minting-credential check is reused at most this long.
pub const CREDENTIAL_CACHE_TTL: Duration = Duration::from_secs(30);
/// An environment's allowed websites are reused at most this long.
pub const ORIGINS_CACHE_TTL: Duration = Duration::from_secs(60);
/// Largest presented user token (contract §1).
pub const MAX_USER_TOKEN_BYTES: usize = 2048;
/// Bounded caches are flushed when full.
const MAX_CACHE_ENTRIES: usize = 10_000;

/// The partner feature flags (all default off).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartnerSettings {
    /// `MESSAGING_PARTNER_SESSION_DEVICES_ENABLED`: the user-token exchange, partner
    /// session devices, environment origins and the `CFMEET_PLATFORM_PARTNER` baseline.
    pub session_devices_enabled: bool,
    /// `MESSAGING_PARTNER_PUSH_ENABLED`: partner push registrations and wake-ups.
    pub push_enabled: bool,
    /// `MESSAGING_MESSAGE_SENT_EXPORT_ENABLED`: export `MESSAGE_SENT` usage facts (only
    /// once the usage catalogue knows the type).
    pub message_sent_export_enabled: bool,
}

fn flag(get: &impl Fn(&str) -> Option<String>, name: &str) -> Result<bool, String> {
    match get(name).as_deref().map(str::trim) {
        None | Some("") | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(format!("{name} must be `true` or `false`")),
    }
}

impl PartnerSettings {
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        Ok(Self {
            session_devices_enabled: flag(&get, "MESSAGING_PARTNER_SESSION_DEVICES_ENABLED")?,
            push_enabled: flag(&get, "MESSAGING_PARTNER_PUSH_ENABLED")?,
            message_sent_export_enabled: flag(&get, "MESSAGING_MESSAGE_SENT_EXPORT_ENABLED")?,
        })
    }

    /// Whether `MESSAGE_SENT` facts are recorded locally for EXTERNAL environments.
    pub fn message_sent_recording(&self) -> bool {
        self.session_devices_enabled || self.message_sent_export_enabled
    }
}

// Process-wide switches read where no application state is at hand (the effective
// policy is resolved from the database alone; the accepted-message transaction records
// its usage facts). Set once at startup ([`install_process_flags`]); default off.
static BASELINE: AtomicBool = AtomicBool::new(false);
static MESSAGE_SENT: AtomicBool = AtomicBool::new(false);

/// Installs the process-wide switches of `settings` (startup; tests of partner features).
pub fn install_process_flags(settings: &PartnerSettings) {
    BASELINE.store(settings.session_devices_enabled, Ordering::SeqCst);
    MESSAGE_SENT.store(settings.message_sent_recording(), Ordering::SeqCst);
}

/// Whether the `CFMEET_PLATFORM_PARTNER` baseline applies (partner flag on).
pub fn baseline_enabled() -> bool {
    BASELINE.load(Ordering::SeqCst)
}

/// Whether `MESSAGE_SENT` facts are recorded for EXTERNAL environments.
pub fn message_sent_recording() -> bool {
    MESSAGE_SENT.load(Ordering::SeqCst)
}

// ------------------------------------------------------------------------ the user token

/// Effective grants of a verified user token (contract §1 / §2.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grants {
    #[serde(default)]
    pub msg: Vec<String>,
    #[serde(default)]
    pub call: Vec<String>,
    #[serde(default)]
    pub mtg: Vec<Value>,
}

/// The user of a verified token.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedUser {
    pub id: Uuid,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    pub status: String,
}

/// Platform Access's `200` answer to `POST /internal/v1/user-tokens/verify`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedUserToken {
    pub token_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub credential_id: Uuid,
    pub service_account_id: Uuid,
    pub user: VerifiedUser,
    #[serde(default)]
    pub grants: Grants,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl VerifiedUserToken {
    /// The token's application environment as a trusted scope (its minting service
    /// account is the initiating service account).
    pub fn owner_scope(&self) -> Option<OwnerScope> {
        Some(OwnerScope {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
            environment_kind: EnvironmentKind::parse(&self.environment_kind)?,
            service_account_id: self.service_account_id,
        })
    }

    /// The Messaging scopes the effective `msg` grants allow (client scopes ∩ grants).
    /// Attachments follow the matching message grant (§5 lists them for partner devices).
    pub fn messaging_scopes(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.grants.msg.iter().any(|g| g == "read") {
            out.extend(["messages.read", "attachments.read"]);
        }
        if self.grants.msg.iter().any(|g| g == "send") {
            out.extend(["messages.send", "attachments.write"]);
        }
        out
    }
}

/// Platform Access's verdict on a presented user token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    Valid(Box<VerifiedUserToken>),
    /// `401`: `USER_TOKEN_INVALID`, `USER_TOKEN_EXPIRED` or `USER_TOKEN_REVOKED`.
    Rejected(&'static str),
}

/// The canonical rejection code of a Platform Access answer (anything unknown is INVALID).
pub fn rejection_code(code: Option<&str>) -> &'static str {
    match code {
        Some("USER_TOKEN_EXPIRED") => "USER_TOKEN_EXPIRED",
        Some("USER_TOKEN_REVOKED") => "USER_TOKEN_REVOKED",
        _ => "USER_TOKEN_INVALID",
    }
}

/// The wire shape of a user token (`cfut_<sbx|test|live>_<compact JWS>`, ≤ 2048 bytes):
/// its environment tag and the UNVERIFIED token id (`jti`), used only as a cache key.
pub fn token_shape(token: &str) -> Option<(&'static str, Option<Uuid>)> {
    if token.len() > MAX_USER_TOKEN_BYTES {
        return None;
    }
    let rest = token.strip_prefix("cfut_")?;
    let (tag, jws) = rest.split_once('_')?;
    let kind = match tag {
        "sbx" => "SANDBOX",
        "test" => "TEST",
        "live" => "PRODUCTION",
        _ => return None,
    };
    let parts: Vec<&str> = jws.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    if !jws
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return None;
    }
    let jti = URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()
        .and_then(|c| serde_json::from_slice::<Value>(&c).ok())
        .and_then(|c| c.get("jti")?.as_str().and_then(|s| Uuid::parse_str(s).ok()));
    Some((kind, jti))
}

// ----------------------------------------------------------------------- Platform Access

/// One page of Platform Access's identity change feed (raw changes).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChangePage {
    pub changes: Vec<Value>,
    pub next_after: i64,
    pub has_more: bool,
}

/// What Messaging asks Platform Access about partner devices.
#[async_trait::async_trait]
pub trait PartnerDirectory: Send + Sync {
    /// `POST /internal/v1/user-tokens/verify` (`platform-user-token.verify`).
    async fn verify_user_token(&self, token: &str) -> Result<Verification, AuthorityUnavailable>;

    /// Whether the credential still authenticates (`GET /internal/v1/credentials/{id}`,
    /// `platform-identity.read`, signed delegated context of its scope).
    async fn credential_active(
        &self,
        scope: &OwnerScope,
        credential_id: Uuid,
    ) -> Result<bool, AuthorityUnavailable>;

    /// The allowed websites of an environment
    /// (`GET /internal/v1/environments/{environmentId}/origins`,
    /// `platform-environment-origins.read`).
    async fn environment_origins(
        &self,
        environment_id: Uuid,
    ) -> Result<Vec<String>, AuthorityUnavailable>;

    /// The identity change feed (`POST /internal/v1/identity/changes`,
    /// `platform-identity.read`).
    async fn identity_changes(&self, after: i64) -> Result<ChangePage, AuthorityUnavailable>;
}

fn platform_access() -> ServiceId {
    ServiceId::parse(services::PLATFORM_ACCESS).expect("canonical identity")
}

fn unavailable(e: InternalCallError) -> AuthorityUnavailable {
    AuthorityUnavailable(e.to_string())
}

/// Platform Access over authenticated internal HTTP.
pub struct PlatformAccessPartners {
    client: Arc<InternalServiceClient>,
}

impl PlatformAccessPartners {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl PartnerDirectory for PlatformAccessPartners {
    async fn verify_user_token(&self, token: &str) -> Result<Verification, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(platform_access());
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/user-tokens/verify",
                &json!({ "token": token }),
            )
            .await
            .map_err(unavailable)?;
        match response.status() {
            200 => response
                .json::<VerifiedUserToken>()
                .map(|v| Verification::Valid(Box::new(v)))
                .map_err(|_| AuthorityUnavailable("malformed user-token verification".into())),
            401 => Ok(Verification::Rejected(rejection_code(
                response.error_code().as_deref(),
            ))),
            s => Err(AuthorityUnavailable(format!(
                "HTTP {s} {}",
                response.error_code().unwrap_or_default()
            ))),
        }
    }

    async fn credential_active(
        &self,
        scope: &OwnerScope,
        credential_id: Uuid,
    ) -> Result<bool, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(platform_access())
            .with_delegated_context(scope.delegated());
        let response = self
            .client
            .get(
                &request,
                &format!("/internal/v1/credentials/{credential_id}"),
            )
            .await
            .map_err(unavailable)?;
        match response.status() {
            200 => {
                let body: Value = response
                    .json()
                    .map_err(|_| AuthorityUnavailable("malformed credential status".into()))?;
                // Defence in depth: the answer is about this credential.
                let same =
                    body["credential"]["id"].as_str() == Some(credential_id.to_string().as_str());
                Ok(same && body["authenticates"].as_bool() == Some(true))
            }
            404 => Ok(false),
            403 if response.error_code().as_deref() == Some("DELEGATED_CONTEXT_INVALID") => {
                Ok(false)
            }
            s => Err(AuthorityUnavailable(format!(
                "HTTP {s} {}",
                response.error_code().unwrap_or_default()
            ))),
        }
    }

    async fn environment_origins(
        &self,
        environment_id: Uuid,
    ) -> Result<Vec<String>, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(platform_access());
        let response = self
            .client
            .get(
                &request,
                &format!("/internal/v1/environments/{environment_id}/origins"),
            )
            .await
            .map_err(unavailable)?;
        match response.status() {
            200 => {
                let body: Value = response
                    .json()
                    .map_err(|_| AuthorityUnavailable("malformed environment origins".into()))?;
                if body["environmentId"].as_str() != Some(environment_id.to_string().as_str()) {
                    return Err(AuthorityUnavailable(
                        "inconsistent environment origins".into(),
                    ));
                }
                Ok(body["origins"]
                    .as_array()
                    .map(|o| {
                        o.iter()
                            .filter_map(Value::as_str)
                            .filter(|o| valid_origin(o))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default())
            }
            404 => Ok(Vec::new()),
            s => Err(AuthorityUnavailable(format!(
                "HTTP {s} {}",
                response.error_code().unwrap_or_default()
            ))),
        }
    }

    async fn identity_changes(&self, after: i64) -> Result<ChangePage, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(platform_access());
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/identity/changes",
                &json!({ "after": after, "limit": 500 }),
            )
            .await
            .map_err(unavailable)?;
        if response.status() != 200 {
            return Err(AuthorityUnavailable(format!("HTTP {}", response.status())));
        }
        let body: Value = response
            .json()
            .map_err(|_| AuthorityUnavailable("malformed change page".into()))?;
        Ok(ChangePage {
            changes: body["changes"]
                .as_array()
                .cloned()
                .ok_or_else(|| AuthorityUnavailable("malformed change page".into()))?,
            next_after: body["nextAfter"]
                .as_i64()
                .ok_or_else(|| AuthorityUnavailable("malformed change page".into()))?,
            has_more: body["hasMore"].as_bool().unwrap_or(false),
        })
    }
}

/// Nothing configured: every question fails closed.
pub struct UnconfiguredPartners;

#[async_trait::async_trait]
impl PartnerDirectory for UnconfiguredPartners {
    async fn verify_user_token(&self, _: &str) -> Result<Verification, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no partner directory is configured".into(),
        ))
    }
    async fn credential_active(
        &self,
        _: &OwnerScope,
        _: Uuid,
    ) -> Result<bool, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no partner directory is configured".into(),
        ))
    }
    async fn environment_origins(&self, _: Uuid) -> Result<Vec<String>, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no partner directory is configured".into(),
        ))
    }
    async fn identity_changes(&self, _: i64) -> Result<ChangePage, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no partner directory is configured".into(),
        ))
    }
}

/// An exact `scheme://host[:port]` origin (§2.3 rules: no path, lowercase host, no
/// wildcard; `http` only for localhost / 127.0.0.1).
pub fn valid_origin(origin: &str) -> bool {
    let (scheme, rest) = match origin.split_once("://") {
        Some(p) => p,
        None => return false,
    };
    if rest.is_empty()
        || rest.len() > 253
        || rest.bytes().any(|b| {
            !(b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b':'))
        })
    {
        return false;
    }
    let host = rest.split(':').next().unwrap_or("");
    match scheme {
        "https" => !host.is_empty(),
        "http" => host == "localhost" || host == "127.0.0.1",
        _ => false,
    }
}

// ---------------------------------------------------------------------------- runtime

/// The partner runtime of this instance.
pub struct Partner {
    pub settings: PartnerSettings,
    pub directory: Arc<dyn PartnerDirectory>,
    /// N-P2: partner push registrations towards the Notification Service.
    pub registrations: Arc<dyn push::PartnerRegistrationSink>,
    /// N-P4: metadata-only partner wake-ups towards the Notification Service (I1).
    pub wakeups: Arc<dyn crate::notification_publications::NotificationEventSink>,
    verified: Mutex<HashMap<Uuid, ([u8; 32], VerifiedUserToken, Instant)>>,
    credentials: Mutex<HashMap<Uuid, Instant>>,
    origins: Mutex<HashMap<Uuid, (Vec<String>, Instant)>>,
    baselines: Mutex<HashSet<Uuid>>,
}

impl std::fmt::Debug for Partner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Partner")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Partner {
    pub fn new(settings: PartnerSettings, directory: Arc<dyn PartnerDirectory>) -> Self {
        Self {
            settings,
            directory,
            registrations: Arc::new(push::UnconfiguredRegistrations),
            wakeups: Arc::new(push::UnconfiguredWakeups),
            verified: Mutex::new(HashMap::new()),
            credentials: Mutex::new(HashMap::new()),
            origins: Mutex::new(HashMap::new()),
            baselines: Mutex::new(HashSet::new()),
        }
    }

    /// Every flag off (the default).
    pub fn disabled() -> Self {
        Self::new(PartnerSettings::default(), Arc::new(UnconfiguredPartners))
    }

    pub fn with_push(
        mut self,
        registrations: Arc<dyn push::PartnerRegistrationSink>,
        wakeups: Arc<dyn crate::notification_publications::NotificationEventSink>,
    ) -> Self {
        self.registrations = registrations;
        self.wakeups = wakeups;
        self
    }

    /// Verifies a presented user token, reusing a positive answer for the same token for
    /// at most [`VERIFY_CACHE_TTL`] (keyed by token id, bound to the token's digest).
    pub async fn verify(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<Verification, AuthorityUnavailable> {
        let Some((kind, jti)) = token_shape(token) else {
            return Ok(Verification::Rejected("USER_TOKEN_INVALID"));
        };
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if let Some(jti) = jti {
            if let Some((d, v, at)) = lock(&self.verified).get(&jti).cloned() {
                if d == digest && at.elapsed() < VERIFY_CACHE_TTL && v.expires_at > now {
                    crate::metrics::inc(
                        "messaging_user_token_verify_total",
                        &[("outcome", "cached")],
                    );
                    return Ok(Verification::Valid(Box::new(v)));
                }
            }
        }
        let outcome = self.directory.verify_user_token(token).await;
        match &outcome {
            Ok(Verification::Valid(v)) => {
                // Defence in depth: the answer is about this token and its environment
                // kind matches the token's tag.
                if jti.is_some_and(|j| j != v.token_id) || v.environment_kind != kind {
                    crate::metrics::inc(
                        "messaging_user_token_verify_total",
                        &[("outcome", "inconsistent")],
                    );
                    return Err(AuthorityUnavailable(
                        "inconsistent user-token verification".into(),
                    ));
                }
                crate::metrics::inc("messaging_user_token_verify_total", &[("outcome", "valid")]);
                let mut cache = lock(&self.verified);
                if cache.len() >= MAX_CACHE_ENTRIES {
                    cache.clear();
                }
                cache.insert(v.token_id, (digest, (**v).clone(), Instant::now()));
            }
            Ok(Verification::Rejected(code)) => {
                crate::metrics::inc("messaging_user_token_verify_total", &[("outcome", code)]);
                if let Some(j) = jti {
                    lock(&self.verified).remove(&j);
                }
            }
            Err(_) => {
                crate::metrics::inc(
                    "messaging_user_token_verify_total",
                    &[("outcome", "unavailable")],
                );
            }
        }
        outcome
    }

    /// Whether a partner device's minting credential still authenticates (positive
    /// answers reused ≤ [`CREDENTIAL_CACHE_TTL`]; a negative answer evicts).
    pub async fn credential_active(
        &self,
        scope: &OwnerScope,
        credential_id: Uuid,
    ) -> Result<bool, AuthorityUnavailable> {
        if let Some(at) = lock(&self.credentials).get(&credential_id).copied() {
            if at.elapsed() < CREDENTIAL_CACHE_TTL {
                return Ok(true);
            }
        }
        let active = self
            .directory
            .credential_active(scope, credential_id)
            .await?;
        let mut cache = lock(&self.credentials);
        if active {
            if cache.len() >= MAX_CACHE_ENTRIES {
                cache.clear();
            }
            cache.insert(credential_id, Instant::now());
        } else {
            cache.remove(&credential_id);
        }
        Ok(active)
    }

    /// Forgets a credential's positive answer (the change feed reported it).
    pub fn forget_credential(&self, credential_id: Uuid) {
        lock(&self.credentials).remove(&credential_id);
    }

    /// Whether `origin` is one of the environment's allowed websites (cached ≤
    /// [`ORIGINS_CACHE_TTL`]; Platform Access unreachable = not allowed).
    pub async fn origin_allowed(&self, environment_id: Uuid, origin: &str) -> bool {
        let cached = lock(&self.origins)
            .get(&environment_id)
            .filter(|(_, at)| at.elapsed() < ORIGINS_CACHE_TTL)
            .map(|(o, _)| o.clone());
        let origins = match cached {
            Some(o) => o,
            None => match self.directory.environment_origins(environment_id).await {
                Ok(o) => {
                    let mut cache = lock(&self.origins);
                    if cache.len() >= MAX_CACHE_ENTRIES {
                        cache.clear();
                    }
                    cache.insert(environment_id, (o.clone(), Instant::now()));
                    o
                }
                Err(e) => {
                    tracing::warn!(error = %e, "environment origins unavailable");
                    return false;
                }
            },
        };
        origins.iter().any(|o| o == origin)
    }

    /// Registers the `CFMEET_PLATFORM_PARTNER` baseline for an EXTERNAL environment once
    /// per process (never replacing another baseline). Best effort: a failure is logged
    /// and retried on the next request.
    pub async fn ensure_baseline<C: sea_orm::ConnectionTrait>(
        &self,
        db: &C,
        scope: &OwnerScope,
        now: DateTime<Utc>,
    ) {
        if !self.settings.session_devices_enabled
            || lock(&self.baselines).contains(&scope.environment_id)
        {
            return;
        }
        match crate::conversations::policy::register_partner_baseline(db, scope, now).await {
            Ok(()) => {
                let mut set = lock(&self.baselines);
                if set.len() >= MAX_CACHE_ENTRIES {
                    set.clear();
                }
                set.insert(scope.environment_id);
            }
            Err(e) => {
                tracing::warn!(error = %e, "partner policy baseline not registered (retried)")
            }
        }
    }
}

/// The opaque binding of a partner session device:
/// SHA-256("cfmeet-user-binding:v1|env|principal|installationId"), lower hex.
pub fn binding_reference(
    environment_id: Uuid,
    principal_id: Uuid,
    installation_id: &str,
) -> String {
    hex::encode(Sha256::digest(
        format!(
            "cfmeet-user-binding:v1|{}|{}|{installation_id}",
            environment_id.hyphenated(),
            principal_id.hyphenated()
        )
        .as_bytes(),
    ))
}

/// `installationId`: 8–128 characters of `[A-Za-z0-9._-]` (contract §4).
pub fn valid_installation_id(id: &str) -> bool {
    (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(tag: &str, claims: Value) -> String {
        let h = URL_SAFE_NO_PAD
            .encode(br#"{"alg":"EdDSA","typ":"cfmeet-user+jwt","kid":"0011223344556677"}"#);
        let c = URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("cfut_{tag}_{h}.{c}.c2ln")
    }

    #[test]
    fn token_shape_reads_the_tag_and_the_unverified_token_id() {
        let jti = Uuid::new_v4();
        assert_eq!(
            token_shape(&token("sbx", json!({ "jti": jti }))),
            Some(("SANDBOX", Some(jti)))
        );
        assert_eq!(
            token_shape(&token("live", json!({}))),
            Some(("PRODUCTION", None))
        );
        assert_eq!(token_shape(&token("prod", json!({}))), None);
        assert_eq!(token_shape("cfmk_test_abc"), None);
        assert_eq!(token_shape("cfut_test_a.b"), None);
        assert_eq!(
            token_shape(&format!("cfut_test_{}", "a.".repeat(1100))),
            None
        );
    }

    #[test]
    fn grants_map_to_messaging_scopes() {
        let v = |msg: &[&str]| VerifiedUserToken {
            token_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            environment_kind: "SANDBOX".into(),
            credential_id: Uuid::new_v4(),
            service_account_id: Uuid::new_v4(),
            user: VerifiedUser {
                id: Uuid::new_v4(),
                external_id: None,
                display_name: None,
                image_url: None,
                status: "ACTIVE".into(),
            },
            grants: Grants {
                msg: msg.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            issued_at: Utc::now(),
            expires_at: Utc::now(),
        };
        assert!(v(&[]).messaging_scopes().is_empty());
        assert_eq!(
            v(&["read"]).messaging_scopes(),
            vec!["messages.read", "attachments.read"]
        );
        assert_eq!(v(&["send", "read"]).messaging_scopes().len(), 4);
        assert_eq!(v(&["admin"]).messaging_scopes(), Vec::<&str>::new());
    }

    #[test]
    fn origins_and_installations_follow_the_contract() {
        assert!(valid_origin("https://shop.example.com"));
        assert!(valid_origin("https://shop.example.com:8443"));
        assert!(valid_origin("http://localhost:3000"));
        assert!(valid_origin("http://127.0.0.1"));
        assert!(!valid_origin("http://shop.example.com"));
        assert!(!valid_origin("https://Shop.example.com"));
        assert!(!valid_origin("https://shop.example.com/path"));
        assert!(!valid_origin("https://*.example.com"));
        assert!(valid_installation_id("inst-1234.ab_C"));
        assert!(!valid_installation_id("short"));
        assert!(!valid_installation_id("has space here"));
        assert!(!valid_installation_id(&"a".repeat(129)));
        let r = binding_reference(Uuid::nil(), Uuid::nil(), "installation-1");
        assert!(crate::realtime::session_devices::valid_reference(&r));
    }

    #[test]
    fn flags_default_off_and_parse_strictly() {
        let none = PartnerSettings::from_env(|_| None).unwrap();
        assert_eq!(none, PartnerSettings::default());
        let on = PartnerSettings::from_env(|k| {
            (k == "MESSAGING_PARTNER_SESSION_DEVICES_ENABLED").then(|| "true".to_string())
        })
        .unwrap();
        assert!(on.session_devices_enabled && !on.push_enabled && on.message_sent_recording());
        assert!(PartnerSettings::from_env(|_| Some("yes".into())).is_err());
    }
}
