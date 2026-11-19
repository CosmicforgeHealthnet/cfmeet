//! Partner push (ADR 0063, phase B5; contract `cfmeet.notifications.internal-api` 1.3.0),
//! behind `NOTIFICATIONS_PARTNER_PUSH_ENABLED` (default off: none of this is reachable).
//!
//! * **N-P1, provider vault.** A partner's APNs, APNs VoIP or FCM credential arrives sealed
//!   (HPKE, `cf_notification_client::seal::seal_push_credential`) to this service for one
//!   (tenant, application, environment). It is opened only here, checked, and stored
//!   sealed again under the service's data keys, bound to its scope and provider
//!   (`push-provider:<tenant>:<application>:<environment>:<provider>`): a sealed secret
//!   moved to another row never opens. Responses, logs and errors never carry it.
//! * **N-P2, registrations.** Messaging's registration facade (I13) also carries
//!   `platform-principal` subjects: endpoints keyed by (scope, principal, installation,
//!   platform), APNS / APNS_VOIP / FCM only, handles sealed like every other handle.
//! * **N-P3, call fast lane.** I3 also accepts a `platform-principal` recipient: its
//!   APNS_VOIP endpoints get a PushKit VoIP push (topic `<bundleId>.voip`), its FCM
//!   endpoints a high-priority data message; synchronous, ≤ 1 s, no retry.
//! * **N-P4, recipients.** `platform-principal:<environmentId>:<principalId>` recipients of
//!   `messaging.message.created` become metadata-only pushes (ids only, collapsed per
//!   conversation) delivered by [`PartnerPushWorker`].
//! * **N-P5, observability.** Per-scope (environment) sent / failed counters; a handle the
//!   provider refuses for good is disabled.
//!
//! Isolation: an endpoint is only ever paired with the credential of its **own** (tenant,
//! application, environment) and platform — every send selects the credential by the
//! endpoint's scope columns ([`load_credential`]) — and CF Meet's own adapters
//! (`AppState::push`) never see a partner endpoint (partner deliveries target
//! `partner:<endpoint>`, which the first-party dispatcher never claims). A recipient whose
//! environment has no credential for a platform is never sent to (`NOT_CONFIGURED`,
//! `NO_CREDENTIAL`).

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use cf_notification_client::seal::{open_push_credential, OpeningKey, Sealed};
use cf_push_transport::{
    adapters::{
        validate_handle_shape, Apns, ApnsSettings, Fcm, FcmSettings, PushAdapters, ServiceAccount,
    },
    PushOutcome, PushProvider, SendOptions,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    crypto::DataKeys,
    db,
    metrics::{label, Heartbeats, Metrics},
    push::{self, MirrorOutcome},
};

/// The provider slots of the vault (the path segment `{provider}`).
pub const PROVIDERS: &[&str] = &["APNS", "APNS_VOIP", "FCM"];
/// The platforms a partner registration can carry.
pub const PLATFORMS: &[&str] = &["APNS", "APNS_VOIP", "FCM"];
/// The subject / recipient kind of partner end users.
pub const SUBJECT_KIND: &str = "platform-principal";
/// The `target_ref` prefix of partner PUSH deliveries (never claimed by the first-party
/// dispatcher).
pub const TARGET_PREFIX: &str = "partner:";
const LEASE_SECS: i64 = 60;
/// Namespace of the deterministic `account_device_ref` of a partner installation.
const INSTALLATION_NAMESPACE: Uuid = Uuid::from_u128(0x6a1c_3f0e_8b52_4d7a_9e41_c0d2_b7f3_a906);

/// A partner (tenant, application, environment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Scope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

impl Scope {
    fn label(&self) -> String {
        format!("environment=\"{}\"", self.environment_id)
    }
}

/// The seal owner of a stored credential: a seal copied to another scope or provider slot
/// does not open.
pub fn credential_owner(scope: &Scope, provider: &str) -> String {
    format!(
        "push-provider:{}:{}:{}:{provider}",
        scope.tenant_id, scope.application_id, scope.environment_id
    )
}

fn provider_slot(p: &str) -> Option<&'static str> {
    PROVIDERS.iter().copied().find(|x| *x == p)
}

/// The adapter platform of a partner endpoint platform (APNs VoIP uses the APNs adapter).
fn adapter_platform(platform: &str) -> &'static str {
    match platform {
        "FCM" => "FCM",
        _ => "APNS",
    }
}

// ------------------------------------------------------------------------------------
// Credentials
// ------------------------------------------------------------------------------------

/// Non-secret facts of a credential (contract `PushProviderMetadata`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    /// `SANDBOX` or `PRODUCTION` (APNs only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apns_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

fn apple_id(s: &str) -> bool {
    s.len() == 10 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn bundle_ok(s: &str) -> bool {
    (3..=155).contains(&s.len())
        && s.contains('.')
        && !s.starts_with('.')
        && !s.ends_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

fn project_ok(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl ProviderMetadata {
    /// Checks (and normalizes) the metadata of a provider slot.
    pub fn validate(&mut self, provider: &str) -> Result<(), &'static str> {
        match provider {
            "APNS" | "APNS_VOIP" => {
                if self.project_id.is_some() {
                    return Err("projectId is for FCM credentials only");
                }
                if !self.team_id.as_deref().is_some_and(apple_id)
                    || !self.key_id.as_deref().is_some_and(apple_id)
                {
                    return Err("teamId and keyId are 10 letters or digits");
                }
                // The app's bundle id; the VoIP topic `<bundleId>.voip` is derived.
                if let Some(b) = self.bundle_id.as_mut() {
                    if let Some(base) = b.strip_suffix(".voip") {
                        *b = base.to_string();
                    }
                }
                if !self.bundle_id.as_deref().is_some_and(bundle_ok) {
                    return Err("bundleId is a reverse-DNS bundle identifier");
                }
                if !matches!(self.apns_host.as_deref(), Some("SANDBOX" | "PRODUCTION")) {
                    return Err("apnsHost is SANDBOX or PRODUCTION");
                }
                Ok(())
            }
            "FCM" => {
                if self.team_id.is_some()
                    || self.key_id.is_some()
                    || self.bundle_id.is_some()
                    || self.apns_host.is_some()
                {
                    return Err(
                        "teamId, keyId, bundleId and apnsHost are for APNs credentials only",
                    );
                }
                if self.project_id.as_deref().is_some_and(|p| !project_ok(p)) {
                    return Err("projectId is a Firebase project id");
                }
                Ok(())
            }
            _ => Err("provider is APNS, APNS_VOIP or FCM"),
        }
    }
}

/// An opened credential: exists only in memory while it is checked, verified or turned
/// into a sender. `Debug` never prints the secret.
pub struct OpenedCredential {
    pub scope: Scope,
    pub provider: &'static str,
    pub metadata: ProviderMetadata,
    pub fingerprint: String,
    secret: SecretString,
}

impl fmt::Debug for OpenedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenedCredential")
            .field("scope", &self.scope)
            .field("provider", &self.provider)
            .field("fingerprint", &self.fingerprint)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl OpenedCredential {
    pub fn new(
        scope: Scope,
        provider: &'static str,
        metadata: ProviderMetadata,
        fingerprint: String,
        secret: String,
    ) -> Self {
        Self {
            scope,
            provider,
            metadata,
            fingerprint,
            secret: SecretString::from(secret),
        }
    }

    pub fn secret(&self) -> &str {
        self.secret.expose_secret()
    }

    /// The APNs topic of an alert push (the bundle id); VoIP pushes use `<topic>.voip`.
    pub fn apns_topic(&self) -> Option<&str> {
        self.metadata.bundle_id.as_deref()
    }
}

/// The result of a live credential check (`POST …/verify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    Verified,
    /// The provider refused the credential (closed reason class).
    Failed(&'static str),
    /// The provider could not be reached; the stored status is kept.
    Unavailable,
}

/// The network boundary of partner pushes: the live adapters of `cf-push-transport`, or
/// the deterministic [`RecordingPartnerTransport`] in local / test runs.
#[async_trait]
pub trait PartnerTransport: Send + Sync {
    fn name(&self) -> &'static str;
    /// Local checks of a credential (nothing leaves the process): the key parses.
    fn check(&self, cred: &OpenedCredential) -> Result<(), &'static str>;
    /// A sender bound to exactly this credential.
    fn sender(&self, cred: &OpenedCredential) -> Result<Arc<dyn PushProvider>, &'static str>;
    /// A lightweight live check with the provider.
    async fn verify(&self, cred: &OpenedCredential) -> VerifyOutcome;
}

/// The live transport: the partner's own keys through the `cf-push-transport` adapters.
#[derive(Debug, Default)]
pub struct LivePartnerTransport;

impl LivePartnerTransport {
    fn adapters(cred: &OpenedCredential) -> Result<PushAdapters, &'static str> {
        match cred.provider {
            "APNS" | "APNS_VOIP" => {
                let m = &cred.metadata;
                let apns = Apns::new(ApnsSettings {
                    team_id: m.team_id.clone().unwrap_or_default(),
                    key_id: m.key_id.clone().unwrap_or_default(),
                    key_pem: cred.secret().to_string(),
                    topic: m.bundle_id.clone().unwrap_or_default(),
                    base_url: Apns::production_url(m.apns_host.as_deref() == Some("SANDBOX"))
                        .to_string(),
                })
                .map_err(|_| "INVALID_KEY")?;
                Ok(PushAdapters {
                    apns: Some(apns),
                    ..Default::default()
                })
            }
            "FCM" => {
                let account: ServiceAccount =
                    serde_json::from_str(cred.secret()).map_err(|_| "INVALID_KEY")?;
                if cred
                    .metadata
                    .project_id
                    .as_deref()
                    .is_some_and(|p| p != account.project_id)
                {
                    return Err("PROJECT_MISMATCH");
                }
                let fcm = Fcm::new(FcmSettings {
                    account,
                    base_url: Fcm::default_base_url().to_string(),
                    allow_insecure: false,
                })
                .map_err(|_| "INVALID_KEY")?;
                Ok(PushAdapters {
                    fcm: Some(fcm),
                    ..Default::default()
                })
            }
            _ => Err("INVALID_KEY"),
        }
    }
}

/// A placeholder registration token: the provider authenticates the credential first
/// and then refuses the token, so nothing ever reaches a device.
const APNS_PLACEHOLDER: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const FCM_PLACEHOLDER: &str = "cfmeet-credential-verification-placeholder-token";

#[async_trait]
impl PartnerTransport for LivePartnerTransport {
    fn name(&self) -> &'static str {
        "live"
    }

    fn check(&self, cred: &OpenedCredential) -> Result<(), &'static str> {
        Self::adapters(cred).map(|_| ())
    }

    fn sender(&self, cred: &OpenedCredential) -> Result<Arc<dyn PushProvider>, &'static str> {
        Ok(Arc::new(Self::adapters(cred)?))
    }

    async fn verify(&self, cred: &OpenedCredential) -> VerifyOutcome {
        let Ok(adapters) = Self::adapters(cred) else {
            return VerifyOutcome::Failed("INVALID_KEY");
        };
        let platform = adapter_platform(cred.provider);
        let handle = if platform == "FCM" {
            FCM_PLACEHOLDER
        } else {
            APNS_PLACEHOLDER
        };
        let options = SendOptions {
            ttl_seconds: Some(0),
            collapse_id: None,
            voip: cred.provider == "APNS_VOIP",
        };
        let payload = json!({ "v": 1, "type": "verify" });
        let outcome = match tokio::time::timeout(
            StdDuration::from_secs(10),
            adapters.send_with(platform, handle, &payload, &options),
        )
        .await
        {
            Ok(o) => o,
            Err(_) => return VerifyOutcome::Unavailable,
        };
        classify_verify(platform, &outcome)
    }
}

/// Reads a provider answer to the placeholder push (the adapters' outcome texts).
fn classify_verify(platform: &str, outcome: &PushOutcome) -> VerifyOutcome {
    match (platform, outcome) {
        // The provider token / OAuth token was accepted; only the placeholder was refused.
        (_, PushOutcome::Sent | PushOutcome::InvalidHandle) => VerifyOutcome::Verified,
        ("APNS", PushOutcome::Retryable(m)) if m.contains("ProviderToken") => {
            VerifyOutcome::Failed("PROVIDER_TOKEN_REFUSED")
        }
        ("APNS", PushOutcome::Permanent(m)) if m.contains("Topic") => {
            VerifyOutcome::Failed("BAD_TOPIC")
        }
        ("APNS", PushOutcome::Permanent(m)) if m.starts_with("apns 400") => {
            // BadDeviceToken is InvalidHandle; any other 400 still proves authentication.
            VerifyOutcome::Verified
        }
        ("FCM", PushOutcome::Permanent(m)) if m.starts_with("fcm token refused") => {
            VerifyOutcome::Failed("PROVIDER_TOKEN_REFUSED")
        }
        ("FCM", PushOutcome::Permanent(m)) if m.starts_with("fcm 400") => VerifyOutcome::Verified,
        ("FCM", PushOutcome::Retryable(m)) if m == "fcm 401" => {
            VerifyOutcome::Failed("PROVIDER_TOKEN_REFUSED")
        }
        (_, PushOutcome::Permanent(_)) => VerifyOutcome::Failed("PROVIDER_REFUSED"),
        (_, PushOutcome::Retryable(_)) => VerifyOutcome::Unavailable,
    }
}

/// One send through the [`RecordingPartnerTransport`] (what a provider would have been
/// asked to deliver, and with whose credential).
#[derive(Debug, Clone, PartialEq)]
pub struct PartnerSend {
    pub scope: Scope,
    /// The vault slot whose credential signed the send.
    pub provider: &'static str,
    pub fingerprint: String,
    /// The adapter platform (`APNS` / `FCM`).
    pub platform: String,
    /// The APNs topic (`<bundleId>` or `<bundleId>.voip`); `None` for FCM.
    pub topic: Option<String>,
    pub handle: String,
    pub payload: Value,
    pub options: SendOptions,
}

/// The deterministic partner transport (local / test only; refused in staging and
/// production by the configuration). Checks credentials structurally and records sends.
#[derive(Default)]
pub struct RecordingPartnerTransport {
    pub sent: Arc<Mutex<Vec<PartnerSend>>>,
    /// Handles the "provider" refuses for good (APNs 410 / FCM UNREGISTERED).
    pub invalid_handles: Arc<Mutex<Vec<String>>>,
    /// The next verifications fail with this reason class.
    pub verify_failure: Mutex<Option<&'static str>>,
}

struct RecordingSender {
    scope: Scope,
    provider: &'static str,
    fingerprint: String,
    topic: Option<String>,
    sent: Arc<Mutex<Vec<PartnerSend>>>,
    invalid: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl PushProvider for RecordingSender {
    fn name(&self) -> &'static str {
        "partner-recording"
    }

    fn supports(&self, platform: &str) -> bool {
        platform == adapter_platform(self.provider)
    }

    async fn send(&self, platform: &str, handle: &str, payload: &Value) -> PushOutcome {
        self.send_with(platform, handle, payload, &SendOptions::default())
            .await
    }

    async fn send_with(
        &self,
        platform: &str,
        handle: &str,
        payload: &Value,
        options: &SendOptions,
    ) -> PushOutcome {
        if cf_push_transport::adapters::metadata(payload).is_none() {
            return PushOutcome::Permanent("payload is not metadata-only".into());
        }
        if self.invalid.lock().unwrap().iter().any(|h| h == handle) {
            return PushOutcome::InvalidHandle;
        }
        let topic = self.topic.as_ref().map(|t| {
            if options.voip {
                format!("{t}.voip")
            } else {
                t.clone()
            }
        });
        self.sent.lock().unwrap().push(PartnerSend {
            scope: self.scope,
            provider: self.provider,
            fingerprint: self.fingerprint.clone(),
            platform: platform.to_string(),
            topic,
            handle: handle.to_string(),
            payload: payload.clone(),
            options: options.clone(),
        });
        PushOutcome::Sent
    }
}

#[async_trait]
impl PartnerTransport for RecordingPartnerTransport {
    fn name(&self) -> &'static str {
        "local-test"
    }

    fn check(&self, cred: &OpenedCredential) -> Result<(), &'static str> {
        match cred.provider {
            "APNS" | "APNS_VOIP" => {
                let s = cred.secret();
                if s.contains("-----BEGIN PRIVATE KEY-----")
                    && s.contains("-----END PRIVATE KEY-----")
                {
                    Ok(())
                } else {
                    Err("INVALID_KEY")
                }
            }
            _ => {
                let account: ServiceAccount =
                    serde_json::from_str(cred.secret()).map_err(|_| "INVALID_KEY")?;
                if cred
                    .metadata
                    .project_id
                    .as_deref()
                    .is_some_and(|p| p != account.project_id)
                {
                    return Err("PROJECT_MISMATCH");
                }
                Ok(())
            }
        }
    }

    fn sender(&self, cred: &OpenedCredential) -> Result<Arc<dyn PushProvider>, &'static str> {
        self.check(cred)?;
        Ok(Arc::new(RecordingSender {
            scope: cred.scope,
            provider: cred.provider,
            fingerprint: cred.fingerprint.clone(),
            topic: cred.apns_topic().map(str::to_string),
            sent: self.sent.clone(),
            invalid: self.invalid_handles.clone(),
        }))
    }

    async fn verify(&self, cred: &OpenedCredential) -> VerifyOutcome {
        if self.check(cred).is_err() {
            return VerifyOutcome::Failed("INVALID_KEY");
        }
        match *self.verify_failure.lock().unwrap() {
            Some(reason) => VerifyOutcome::Failed(reason),
            None => VerifyOutcome::Verified,
        }
    }
}

/// A sender built for one stored version (fingerprint, `updated_at`) of a credential.
type CachedSender = (String, DateTime<Utc>, Arc<dyn PushProvider>);

/// The partner push runtime (present only while `NOTIFICATIONS_PARTNER_PUSH_ENABLED`).
pub struct PartnerPush {
    pub transport: Arc<dyn PartnerTransport>,
    /// Senders per (scope, slot), valid for one stored version of the credential (a
    /// replaced or deleted credential is never used again: every send reads the row).
    senders: Mutex<HashMap<(Scope, &'static str), CachedSender>>,
}

impl PartnerPush {
    pub fn new(transport: Arc<dyn PartnerTransport>) -> Self {
        Self {
            transport,
            senders: Mutex::new(HashMap::new()),
        }
    }

    fn sender(
        &self,
        cred: &OpenedCredential,
        updated_at: DateTime<Utc>,
    ) -> Result<Arc<dyn PushProvider>, &'static str> {
        let key = (cred.scope, cred.provider);
        if let Some((fp, at, s)) = self.senders.lock().unwrap().get(&key) {
            if *fp == cred.fingerprint && *at == updated_at {
                return Ok(s.clone());
            }
        }
        let s = self.transport.sender(cred)?;
        self.senders
            .lock()
            .unwrap()
            .insert(key, (cred.fingerprint.clone(), updated_at, s.clone()));
        Ok(s)
    }

    fn evict(&self, scope: &Scope, provider: &'static str) {
        self.senders.lock().unwrap().remove(&(*scope, provider));
    }
}

/// Why a stored credential cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialError {
    /// No credential for this scope and provider.
    Missing,
    /// The seal does not open (wrong scope / key) or the key does not parse.
    Unusable(&'static str),
}

#[derive(Debug, FromQueryResult)]
struct StoredCredential {
    secret_sealed: String,
    fingerprint: String,
    metadata: Value,
    status: String,
    updated_at: DateTime<Utc>,
}

async fn stored<C: ConnectionTrait>(
    conn: &C,
    scope: &Scope,
    provider: &str,
) -> Result<Option<StoredCredential>, DbErr> {
    db::one(
        conn,
        "SELECT secret_sealed, fingerprint, metadata, status, updated_at
           FROM notif_push_providers
          WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND provider = $4",
        vec![
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            provider.into(),
        ],
    )
    .await
}

fn open_stored(
    keys: &DataKeys,
    scope: &Scope,
    provider: &'static str,
    row: &StoredCredential,
) -> Result<OpenedCredential, CredentialError> {
    let secret = keys
        .open(&row.secret_sealed, &credential_owner(scope, provider))
        .map_err(|_| CredentialError::Unusable("CREDENTIAL_UNAVAILABLE"))?;
    let metadata: ProviderMetadata = serde_json::from_value(row.metadata.clone())
        .map_err(|_| CredentialError::Unusable("CREDENTIAL_UNAVAILABLE"))?;
    Ok(OpenedCredential::new(
        *scope,
        provider,
        metadata,
        row.fingerprint.clone(),
        secret,
    ))
}

/// The sender of `scope`'s credential for `provider` (the endpoint's own scope and
/// platform: the only pairing that exists).
pub async fn load_credential<C: ConnectionTrait>(
    conn: &C,
    keys: &DataKeys,
    partner: &PartnerPush,
    scope: &Scope,
    provider: &str,
) -> Result<Result<Arc<dyn PushProvider>, CredentialError>, DbErr> {
    let Some(slot) = provider_slot(provider) else {
        return Ok(Err(CredentialError::Missing));
    };
    let Some(row) = stored(conn, scope, slot).await? else {
        return Ok(Err(CredentialError::Missing));
    };
    let cred = match open_stored(keys, scope, slot, &row) {
        Ok(c) => c,
        Err(e) => return Ok(Err(e)),
    };
    Ok(partner
        .sender(&cred, row.updated_at)
        .map_err(CredentialError::Unusable))
}

// ------------------------------------------------------------------------------------
// N-P1: the vault operations
// ------------------------------------------------------------------------------------

/// `PUT …/{provider}` body.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutProviderRequest {
    /// The compact seal `<kid>.<enc>.<ct>`, its JSON object `{kid, enc, ct}`, or that
    /// object JSON-encoded in a string.
    pub sealed: Value,
    #[serde(default)]
    pub metadata: ProviderMetadata,
    pub fingerprint: String,
}

impl fmt::Debug for PutProviderRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PutProviderRequest")
            .field("metadata", &self.metadata)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// The AAD producer of the multi-part form (the Developer Console).
pub const PARTS_PRODUCER: &str = "cfmeet.developer-platform";
/// Most parts of a multi-part seal (16 KiB of base64url in 512-character tokens).
const MAX_PARTS: usize = 64;

/// A sealed credential as the Developer Console sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealedCredential {
    /// One [`cf_notification_client::seal::seal_push_credential`] seal (compact
    /// `<kid>.<enc>.<ct>`, or the `{kid, enc, ct}` object, or that object as a string).
    Whole(Sealed),
    /// `{"v": 1, "parts": [Sealed, …]}` (as an object or a JSON string): the secret's
    /// base64url (no padding) split into `token` parts, each sealed with
    /// `cf_notification_client::seal::seal` for producer [`PARTS_PRODUCER`], event id
    /// `<tenantId>/<applicationId>/<environmentId>/<provider>` and event type
    /// `push-provider.secret.<index>-of-<count>`; a part opens only at its place, for that
    /// scope and provider slot.
    Parts(Vec<Sealed>),
}

/// Reads the `sealed` field in any of its accepted forms.
pub fn parse_sealed(v: &Value) -> Option<SealedCredential> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Multi {
        v: u8,
        parts: Vec<Sealed>,
    }
    let multi = |m: Multi| {
        (m.v == 1 && (1..=MAX_PARTS).contains(&m.parts.len()))
            .then_some(SealedCredential::Parts(m.parts))
    };
    match v {
        Value::String(s) => Sealed::parse_compact(s)
            .ok()
            .or_else(|| serde_json::from_str::<Sealed>(s).ok())
            .map(SealedCredential::Whole)
            .or_else(|| serde_json::from_str::<Multi>(s).ok().and_then(multi)),
        Value::Object(_) => serde_json::from_value::<Sealed>(v.clone())
            .ok()
            .map(SealedCredential::Whole)
            .or_else(|| {
                serde_json::from_value::<Multi>(v.clone())
                    .ok()
                    .and_then(multi)
            }),
        _ => None,
    }
}

/// Opens a sealed credential for exactly (scope, provider); anything sealed for another
/// scope (or, multi-part, another slot or order) fails closed.
pub fn open_credential(
    opening: &OpeningKey,
    scope: &Scope,
    provider: &str,
    sealed: &SealedCredential,
) -> Option<zeroize::Zeroizing<String>> {
    match sealed {
        SealedCredential::Whole(s) => open_push_credential(
            opening,
            scope.tenant_id,
            scope.application_id,
            scope.environment_id,
            s,
        )
        .ok(),
        SealedCredential::Parts(parts) => {
            use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
            let event_id = format!(
                "{}/{}/{}/{provider}",
                scope.tenant_id, scope.application_id, scope.environment_id
            );
            let mut encoded = zeroize::Zeroizing::new(String::new());
            for (index, part) in parts.iter().enumerate() {
                let opened = cf_notification_client::seal::open(
                    opening,
                    PARTS_PRODUCER,
                    &event_id,
                    &format!("push-provider.secret.{index}-of-{}", parts.len()),
                    part,
                )
                .ok()?;
                let token = opened.token.as_deref()?;
                if opened.fields().len() != 1 {
                    return None;
                }
                encoded.push_str(token);
            }
            let bytes = zeroize::Zeroizing::new(URL_SAFE_NO_PAD.decode(encoded.as_bytes()).ok()?);
            if bytes.is_empty()
                || bytes.len() > cf_notification_client::seal::MAX_PUSH_CREDENTIAL_BYTES
            {
                return None;
            }
            let text = std::str::from_utf8(&bytes).ok()?;
            Some(zeroize::Zeroizing::new(text.to_string()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    NotFound,
    Invalid(&'static str),
    /// The environment's credential belongs to another (tenant, application).
    ScopeConflict,
    /// The service cannot open sealed credentials (no HPKE key configured).
    Unavailable,
    Db,
}

impl From<DbErr> for VaultError {
    fn from(e: DbErr) -> Self {
        tracing::error!(summary = %db::summarize_db_error(&e), "partner push vault database failure");
        VaultError::Db
    }
}

fn fingerprint_of(secret: &str) -> String {
    cf_notification_client::push_credential_fingerprint(secret)
}

/// N-P1 store: opens the seal for exactly this scope, checks fingerprint, metadata and
/// key, and stores it sealed under the data keys. Never returns or logs the secret.
#[allow(clippy::too_many_arguments)]
pub async fn put_provider(
    conn: &DatabaseConnection,
    keys: &DataKeys,
    opening: Option<&OpeningKey>,
    partner: &PartnerPush,
    metrics: &Metrics,
    scope: Scope,
    provider: &'static str,
    mut req: PutProviderRequest,
) -> Result<Value, VaultError> {
    let fingerprint = req.fingerprint.trim().to_ascii_lowercase();
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(VaultError::Invalid(
            "fingerprint is the SHA-256 of the secret, 64 hex",
        ));
    }
    req.metadata
        .validate(provider)
        .map_err(VaultError::Invalid)?;
    let sealed = parse_sealed(&req.sealed).ok_or(VaultError::Invalid(
        "sealed is not a notification-client seal",
    ))?;
    let opening = opening.ok_or(VaultError::Unavailable)?;
    let secret = open_credential(opening, &scope, provider, &sealed).ok_or(VaultError::Invalid(
        "sealed does not open for this scope and provider",
    ))?;
    if fingerprint_of(&secret) != fingerprint && fingerprint_of(secret.trim()) != fingerprint {
        return Err(VaultError::Invalid(
            "fingerprint does not match the sealed credential",
        ));
    }
    if provider == "FCM" && req.metadata.project_id.is_none() {
        // The project of the service account (non-secret) is kept as metadata.
        req.metadata.project_id = serde_json::from_str::<Value>(&secret)
            .ok()
            .and_then(|v| v["project_id"].as_str().map(str::to_string))
            .filter(|p| project_ok(p));
    }
    let cred = OpenedCredential::new(
        scope,
        provider,
        req.metadata.clone(),
        fingerprint.clone(),
        secret.to_string(),
    );
    partner.transport.check(&cred).map_err(|class| {
        VaultError::Invalid(match class {
            "PROJECT_MISMATCH" => "projectId does not match the service account",
            _ if provider == "FCM" => "the credential is not an FCM service-account key",
            _ => "the credential is not an APNs .p8 (PKCS#8 P-256) key",
        })
    })?;
    // An environment's slot belongs to one (tenant, application) only.
    let other = db::scalar_i64(
        conn,
        "SELECT count(*)::bigint AS n FROM notif_push_providers
          WHERE environment_id = $1 AND provider = $2 AND (tenant_id <> $3 OR application_id <> $4)",
        vec![scope.environment_id.into(), provider.into(), scope.tenant_id.into(), scope.application_id.into()],
    )
    .await?;
    if other > 0 {
        return Err(VaultError::ScopeConflict);
    }
    let sealed_at_rest = keys.seal(cred.secret(), &credential_owner(&scope, provider));
    drop(cred);
    let metadata = serde_json::to_value(&req.metadata).unwrap_or_else(|_| json!({}));
    let row = conn
        .query_one(db::stmt(
            "INSERT INTO notif_push_providers (tenant_id, application_id, environment_id, provider, secret_sealed,
                 secret_key_id, fingerprint, metadata)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (tenant_id, application_id, environment_id, provider) DO UPDATE SET
                 secret_sealed = EXCLUDED.secret_sealed, secret_key_id = EXCLUDED.secret_key_id,
                 fingerprint = EXCLUDED.fingerprint, metadata = EXCLUDED.metadata, status = 'UNVERIFIED',
                 last_error = NULL, last_verified_at = NULL, updated_at = now()
             RETURNING status, updated_at",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                provider.into(),
                sealed_at_rest.into(),
                keys.kid().into(),
                fingerprint.clone().into(),
                metadata.into(),
            ],
        ))
        .await?
        .ok_or(VaultError::Db)?;
    let status: String = row.try_get("", "status").map_err(VaultError::from)?;
    let updated_at: DateTime<Utc> = row.try_get("", "updated_at").map_err(VaultError::from)?;
    partner.evict(&scope, provider);
    metrics.inc(
        "notif_partner_push_providers_total",
        format!("{},provider=\"{provider}\",action=\"PUT\"", scope.label()),
    );
    tracing::info!(environment = %scope.environment_id, provider, "partner push credential stored");
    Ok(json!({
        "provider": provider,
        "status": status,
        "fingerprint": fingerprint,
        "updatedAt": updated_at,
    }))
}

/// N-P1 list: metadata, fingerprint and status only.
pub async fn list_providers<C: ConnectionTrait>(conn: &C, scope: Scope) -> Result<Value, DbErr> {
    #[derive(FromQueryResult)]
    struct Row {
        provider: String,
        fingerprint: String,
        metadata: Value,
        status: String,
        last_error: Option<String>,
        last_verified_at: Option<DateTime<Utc>>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    }
    let rows: Vec<Row> = db::all(
        conn,
        "SELECT provider, fingerprint, metadata, status, last_error, last_verified_at, created_at, updated_at
           FROM notif_push_providers WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3
          ORDER BY provider",
        vec![scope.tenant_id.into(), scope.application_id.into(), scope.environment_id.into()],
    )
    .await?;
    Ok(json!({
        "items": rows.iter().map(|r| json!({
            "provider": r.provider, "status": r.status, "fingerprint": r.fingerprint,
            "metadata": r.metadata, "lastError": r.last_error, "lastVerifiedAt": r.last_verified_at,
            "createdAt": r.created_at, "updatedAt": r.updated_at,
        })).collect::<Vec<_>>(),
    }))
}

/// N-P1 delete (idempotent). Sends stop at once: every send reads the stored row.
pub async fn delete_provider<C: ConnectionTrait>(
    conn: &C,
    partner: &PartnerPush,
    metrics: &Metrics,
    scope: Scope,
    provider: &'static str,
) -> Result<u64, DbErr> {
    let n = db::exec(
        conn,
        "DELETE FROM notif_push_providers WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND provider = $4",
        vec![scope.tenant_id.into(), scope.application_id.into(), scope.environment_id.into(), provider.into()],
    )
    .await?;
    partner.evict(&scope, provider);
    if n > 0 {
        metrics.inc(
            "notif_partner_push_providers_total",
            format!(
                "{},provider=\"{provider}\",action=\"DELETE\"",
                scope.label()
            ),
        );
        tracing::info!(environment = %scope.environment_id, provider, "partner push credential deleted");
    }
    Ok(n)
}

/// N-P1 verify: a live check of the stored credential; `{status, lastError?}`.
pub async fn verify_provider(
    conn: &DatabaseConnection,
    keys: &DataKeys,
    partner: &PartnerPush,
    scope: Scope,
    provider: &'static str,
) -> Result<Value, VaultError> {
    let row = stored(conn, &scope, provider)
        .await?
        .ok_or(VaultError::NotFound)?;
    let outcome = match open_stored(keys, &scope, provider, &row) {
        Ok(cred) => partner.transport.verify(&cred).await,
        Err(_) => VerifyOutcome::Failed("CREDENTIAL_UNAVAILABLE"),
    };
    let (status, last_error): (String, Option<&str>) = match outcome {
        VerifyOutcome::Verified => ("VERIFIED".into(), None),
        VerifyOutcome::Failed(class) => ("FAILED".into(), Some(class)),
        VerifyOutcome::Unavailable => (row.status.clone(), Some("PROVIDER_UNAVAILABLE")),
    };
    let checked = !matches!(outcome, VerifyOutcome::Unavailable);
    db::exec(
        conn,
        "UPDATE notif_push_providers SET status = $5, last_error = $6,
                last_verified_at = CASE WHEN $7 THEN now() ELSE last_verified_at END
          WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND provider = $4 AND fingerprint = $8",
        vec![
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            provider.into(),
            status.clone().into(),
            last_error.map(str::to_string).into(),
            checked.into(),
            row.fingerprint.clone().into(),
        ],
    )
    .await?;
    let mut out = json!({ "status": status });
    if let Some(e) = last_error {
        out["lastError"] = json!(e);
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------
// N-P2: registrations through Messaging's facade (I13)
// ------------------------------------------------------------------------------------

/// The `platform-principal` subject of a registration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartnerSubject {
    pub kind: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Uuid,
    pub installation_id: String,
}

/// I13 item with a nested subject: `{subject, platform, handle?, enabled?}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NestedRegistration {
    pub subject: PartnerSubject,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// I13 item with the subject's fields inline: `{kind, tenantId, …, installationId,
/// platform, handle?, enabled?}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlatRegistration {
    pub kind: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Uuid,
    pub installation_id: String,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// One I13 item: a first-party mirror item (unchanged) or a partner registration.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum SyncItem {
    Mirror(push::MirrorItem),
    Nested(NestedRegistration),
    Flat(FlatRegistration),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncRequest {
    pub registrations: Vec<SyncItem>,
}

impl fmt::Debug for SyncRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncRequest")
            .field("registrations", &self.registrations.len())
            .finish()
    }
}

/// A partner registration (normalized).
pub struct PartnerRegistration {
    pub kind: String,
    pub scope: Scope,
    pub principal_id: Uuid,
    pub installation_id: String,
    pub platform: Option<String>,
    pub handle: Option<String>,
    pub enabled: bool,
}

impl From<NestedRegistration> for PartnerRegistration {
    fn from(n: NestedRegistration) -> Self {
        let enabled = n.enabled.unwrap_or(n.handle.is_some());
        Self {
            kind: n.subject.kind,
            scope: Scope {
                tenant_id: n.subject.tenant_id,
                application_id: n.subject.application_id,
                environment_id: n.subject.environment_id,
            },
            principal_id: n.subject.principal_id,
            installation_id: n.subject.installation_id,
            platform: n.platform,
            handle: n.handle,
            enabled,
        }
    }
}

impl From<FlatRegistration> for PartnerRegistration {
    fn from(n: FlatRegistration) -> Self {
        let enabled = n.enabled.unwrap_or(n.handle.is_some());
        Self {
            kind: n.kind,
            scope: Scope {
                tenant_id: n.tenant_id,
                application_id: n.application_id,
                environment_id: n.environment_id,
            },
            principal_id: n.principal_id,
            installation_id: n.installation_id,
            platform: n.platform,
            handle: n.handle,
            enabled,
        }
    }
}

/// `installationId`: 8–128 of `[A-Za-z0-9._-]` (contract §4).
pub fn installation_ok(id: &str) -> bool {
    (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl PartnerRegistration {
    fn device_ref(&self) -> Uuid {
        Uuid::new_v5(
            &INSTALLATION_NAMESPACE,
            format!(
                "{SUBJECT_KIND}:{}:{}:{}:{}:{}",
                self.scope.tenant_id,
                self.scope.application_id,
                self.scope.environment_id,
                self.principal_id,
                self.installation_id
            )
            .as_bytes(),
        )
    }

    /// The platform as returned in the result (closed set only).
    pub fn platform_label(&self) -> Option<&'static str> {
        self.platform
            .as_deref()
            .and_then(|p| PLATFORMS.iter().copied().find(|x| *x == p))
    }

    async fn remove<C: ConnectionTrait>(
        &self,
        conn: &C,
        platform: Option<&str>,
    ) -> Result<u64, DbErr> {
        db::exec(
            conn,
            "DELETE FROM notif_push_endpoints
              WHERE subject_kind = 'platform-principal' AND tenant_id = $1 AND application_id = $2
                AND environment_id = $3 AND principal_id = $4 AND installation_id = $5
                AND ($6::text IS NULL OR platform = $6)",
            vec![
                self.scope.tenant_id.into(),
                self.scope.application_id.into(),
                self.scope.environment_id.into(),
                self.principal_id.into(),
                self.installation_id.clone().into(),
                platform.map(str::to_string).into(),
            ],
        )
        .await
    }
}

/// N-P2 for one registration (inside the caller's transaction). The handle is checked
/// structurally, sealed under the endpoint id and digested in its environment's own
/// domain (never colliding with a first-party handle); it is never returned or logged.
pub async fn register<C: ConnectionTrait>(
    conn: &C,
    keys: &DataKeys,
    item: &PartnerRegistration,
    first_party: Option<(Uuid, Uuid, Uuid)>,
) -> Result<(MirrorOutcome, Option<Uuid>), DbErr> {
    let s = item.scope;
    if item.kind != SUBJECT_KIND {
        return Ok((MirrorOutcome::Invalid("SUBJECT"), None));
    }
    if s.environment_id == push::FIRST_PARTY_SCOPE
        || first_party == Some((s.tenant_id, s.application_id, s.environment_id))
    {
        return Ok((MirrorOutcome::Invalid("SCOPE"), None));
    }
    if !installation_ok(&item.installation_id) {
        return Ok((MirrorOutcome::Invalid("INSTALLATION"), None));
    }
    let platform = match item.platform.as_deref() {
        None if !item.enabled => None,
        Some(p) if PLATFORMS.contains(&p) => Some(p),
        _ => return Ok((MirrorOutcome::Invalid("PLATFORM"), None)),
    };
    if !item.enabled {
        let n = item.remove(conn, platform).await?;
        return Ok((
            if n > 0 {
                MirrorOutcome::Removed
            } else {
                MirrorOutcome::Absent
            },
            None,
        ));
    }
    let platform = platform.expect("enabled registrations name their platform");
    let Some(handle) = item
        .handle
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty() && h.len() <= 4096)
    else {
        return Ok((MirrorOutcome::Invalid("HANDLE"), None));
    };
    if validate_handle_shape(adapter_platform(platform), handle).is_err() {
        return Ok((MirrorOutcome::Invalid("HANDLE"), None));
    }
    let digest = keys.digest(
        "partner-push-handle",
        &format!("{}\u{0}{platform}\u{0}{handle}", s.environment_id),
    );
    let device = item.device_ref();
    #[derive(FromQueryResult)]
    struct Existing {
        id: Uuid,
        handle_digest: String,
        enabled: bool,
    }
    let existing: Option<Existing> = db::one(
        conn,
        "SELECT id, handle_digest, enabled FROM notif_push_endpoints
          WHERE subject_kind = 'platform-principal' AND environment_id = $1 AND account_device_ref = $2 AND platform = $3",
        vec![s.environment_id.into(), device.into(), platform.into()],
    )
    .await?;
    if let Some(row) = &existing {
        if row.handle_digest == digest && row.enabled {
            return Ok((MirrorOutcome::Unchanged, Some(row.id)));
        }
    }
    // A handle that moved to another installation or principal of the environment leaves
    // its old row (the digest domain is the environment's own).
    db::exec(
        conn,
        "DELETE FROM notif_push_endpoints WHERE handle_digest = $1
            AND NOT (environment_id = $2 AND account_device_ref = $3 AND platform = $4)",
        vec![
            digest.clone().into(),
            s.environment_id.into(),
            device.into(),
            platform.into(),
        ],
    )
    .await?;
    let row = conn
        .query_one(db::stmt(
            "INSERT INTO notif_push_endpoints (id, tenant_id, application_id, environment_id, account_ref, principal_id,
                 account_device_ref, platform, handle_sealed, handle_key_id, handle_digest, subject_kind, installation_id)
             VALUES ($1, $2, $3, $4, NULL, $5, $6, $7, '', $8, $9, 'platform-principal', $10)
             ON CONFLICT (environment_id, account_device_ref, platform) DO UPDATE SET
                 handle_digest = EXCLUDED.handle_digest, handle_key_id = EXCLUDED.handle_key_id,
                 enabled = true, disabled_reason = NULL, updated_at = now()
               WHERE notif_push_endpoints.subject_kind = 'platform-principal'
             RETURNING id, (xmax = 0) AS inserted",
            vec![
                Uuid::new_v4().into(),
                s.tenant_id.into(),
                s.application_id.into(),
                s.environment_id.into(),
                item.principal_id.into(),
                device.into(),
                platform.into(),
                keys.kid().into(),
                digest.into(),
                item.installation_id.clone().into(),
            ],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("no partner endpoint row".into()))?;
    let endpoint: Uuid = row.try_get("", "id")?;
    let inserted: bool = row.try_get("", "inserted")?;
    db::exec(
        conn,
        "UPDATE notif_push_endpoints SET handle_sealed = $2 WHERE id = $1",
        vec![
            endpoint.into(),
            keys.seal(handle, &format!("push:{endpoint}")).into(),
        ],
    )
    .await?;
    Ok((
        if inserted {
            MirrorOutcome::Created
        } else {
            MirrorOutcome::Updated
        },
        Some(endpoint),
    ))
}

// ------------------------------------------------------------------------------------
// N-P3: the call fast lane for a platform-principal recipient
// ------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartnerRecipient {
    pub kind: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Uuid,
}

/// I3 with a `platform-principal` recipient (contract 1.3.0). `pushTargetRefs` is
/// optional: without it every APNS_VOIP / FCM endpoint of the principal rings.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartnerCallPushRequest {
    pub recipient: PartnerRecipient,
    pub payload: Value,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub push_target_refs: Option<Vec<Uuid>>,
}

fn sent_metric(metrics: &Metrics, scope: &Scope, lane: &str, platform: &str) {
    metrics.inc(
        "notif_partner_push_sent_total",
        format!(
            "{},lane=\"{lane}\",platform=\"{}\"",
            scope.label(),
            label(platform)
        ),
    );
}

fn failed_metric(metrics: &Metrics, scope: &Scope, lane: &str, platform: &str, class: &str) {
    metrics.inc(
        "notif_partner_push_failed_total",
        format!(
            "{},lane=\"{lane}\",platform=\"{}\",class=\"{}\"",
            scope.label(),
            label(platform),
            label(class)
        ),
    );
}

/// N-P3: synchronous, ≤ 1 s overall, never queued or retried. Each endpoint is sent with
/// its own scope's credential for its own platform (APNS_VOIP: PushKit VoIP push, topic
/// `<bundleId>.voip`; FCM: high-priority data message); CF Meet's own credentials are
/// never involved.
pub async fn call_push(
    conn: &DatabaseConnection,
    keys: &DataKeys,
    partner: &PartnerPush,
    metrics: &Metrics,
    req: &PartnerCallPushRequest,
    now: DateTime<Utc>,
) -> Result<Value, DbErr> {
    let r = &req.recipient;
    let scope = Scope {
        tenant_id: r.tenant_id,
        application_id: r.application_id,
        environment_id: r.environment_id,
    };
    #[derive(Debug, FromQueryResult)]
    struct Target {
        id: Uuid,
        platform: String,
        handle_sealed: String,
        enabled: bool,
    }
    let targets: Vec<Target> = db::all(
        conn,
        "SELECT id, platform, handle_sealed, enabled FROM notif_push_endpoints
          WHERE subject_kind = 'platform-principal' AND tenant_id = $1 AND application_id = $2
            AND environment_id = $3 AND principal_id = $4 AND platform IN ('APNS_VOIP', 'FCM')
          ORDER BY id",
        vec![
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            r.principal_id.into(),
        ],
    )
    .await?;
    let mut results = Vec::new();
    // Explicit references: those that are not this principal's endpoints are unknown.
    let selected: Vec<&Target> = match &req.push_target_refs {
        Some(refs) => {
            for id in refs {
                if !targets.iter().any(|t| t.id == *id) {
                    results.push(json!({ "pushTargetRef": id, "outcome": "UNKNOWN_TARGET" }));
                }
            }
            targets.iter().filter(|t| refs.contains(&t.id)).collect()
        }
        None => targets.iter().collect(),
    };
    if req.expires_at <= now {
        for t in &selected {
            results.push(json!({ "pushTargetRef": t.id, "outcome": "EXPIRED" }));
        }
        return Ok(json!({ "results": results, "recipientStatus": "EXPIRED" }));
    }
    if selected.is_empty() {
        failed_metric(metrics, &scope, "call", "NONE", "NO_REGISTRATION");
        return Ok(json!({ "results": results, "recipientStatus": "NO_REGISTRATION" }));
    }
    // Metadata only, exactly like the first-party lane: the call id travels through
    // Calling's own channels.
    let payload = json!({ "v": 1, "type": "call" });
    let deadline = tokio::time::Instant::now() + StdDuration::from_millis(1000);
    let mut senders: HashMap<String, Result<Arc<dyn PushProvider>, CredentialError>> =
        HashMap::new();
    let (mut sent, mut no_credential) = (0usize, 0usize);
    for t in selected {
        if !senders.contains_key(&t.platform) {
            let s = load_credential(conn, keys, partner, &scope, &t.platform).await?;
            senders.insert(t.platform.clone(), s);
        }
        let outcome = if !t.enabled {
            "DISABLED"
        } else {
            match senders.get(&t.platform).expect("loaded above") {
                Err(CredentialError::Missing) => {
                    no_credential += 1;
                    "NOT_CONFIGURED"
                }
                Err(CredentialError::Unusable(_)) => "PERMANENT_FAILURE",
                Ok(sender) => match keys.open(&t.handle_sealed, &format!("push:{}", t.id)) {
                    Err(_) => "PERMANENT_FAILURE",
                    Ok(handle) => {
                        let options = push::call_send_options(&t.platform, req.expires_at, now);
                        let platform = adapter_platform(&t.platform);
                        match tokio::time::timeout_at(
                            deadline,
                            sender.send_with(platform, &handle, &payload, &options),
                        )
                        .await
                        {
                            Ok(PushOutcome::Sent) => "SENT",
                            Ok(PushOutcome::InvalidHandle) => {
                                disable_endpoint(conn, t.id).await?;
                                "INVALID_HANDLE"
                            }
                            Ok(PushOutcome::Retryable(_)) | Err(_) => "RETRYABLE_FAILURE",
                            Ok(PushOutcome::Permanent(_)) => "PERMANENT_FAILURE",
                        }
                    }
                },
            }
        };
        if outcome == "SENT" {
            sent += 1;
            sent_metric(metrics, &scope, "call", &t.platform);
        } else {
            failed_metric(metrics, &scope, "call", &t.platform, outcome);
        }
        results.push(json!({ "pushTargetRef": t.id, "outcome": outcome }));
    }
    let status = if sent > 0 {
        "SENT"
    } else if no_credential > 0 && results.iter().all(|r| r["outcome"] != "DISABLED") {
        "NO_CREDENTIAL"
    } else {
        "FAILED"
    };
    Ok(json!({ "results": results, "recipientStatus": status }))
}

async fn disable_endpoint<C: ConnectionTrait>(conn: &C, endpoint: Uuid) -> Result<(), DbErr> {
    db::exec(
        conn,
        "UPDATE notif_push_endpoints SET enabled = false, disabled_reason = 'INVALID_HANDLE', updated_at = now()
          WHERE id = $1 AND subject_kind = 'platform-principal'",
        vec![endpoint.into()],
    )
    .await?;
    Ok(())
}

// ------------------------------------------------------------------------------------
// N-P4: the partner message dispatcher
// ------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PartnerTick {
    pub claimed: u64,
    pub sent: u64,
    pub retried: u64,
    pub failed: u64,
    pub expired: u64,
}

/// Dispatches `partner:<endpoint>` PUSH deliveries (only those) with the endpoint's own
/// scope credential; retries back off like the first-party dispatcher; nothing is sent
/// after the notification expires.
pub struct PartnerPushWorker {
    db: DatabaseConnection,
    keys: Arc<DataKeys>,
    partner: Arc<PartnerPush>,
    metrics: Arc<Metrics>,
}

#[derive(Debug, FromQueryResult)]
struct Claimed {
    id: Uuid,
    lease_id: Option<Uuid>,
    target_ref: String,
    attempts: i32,
    template_key: String,
    params: Value,
    expires_at: Option<DateTime<Utc>>,
    recipient_ref: String,
}

#[derive(Debug, FromQueryResult)]
struct PartnerEndpoint {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    principal_id: Option<Uuid>,
    platform: String,
    handle_sealed: String,
    enabled: bool,
}

impl PartnerPushWorker {
    pub fn new(
        db: DatabaseConnection,
        keys: Arc<DataKeys>,
        partner: Arc<PartnerPush>,
        metrics: Arc<Metrics>,
    ) -> Self {
        Self {
            db,
            keys,
            partner,
            metrics,
        }
    }

    async fn finish(
        &self,
        job: &Claimed,
        status: &str,
        class: Option<&str>,
        outcome: &str,
    ) -> Result<(), DbErr> {
        db::exec(
            &self.db,
            "UPDATE notif_deliveries SET status = $3, lease_id = NULL, lease_until = NULL, last_error_class = $4,
                    completed_at = CASE WHEN $3 IN ('SENT', 'FAILED', 'EXPIRED', 'CANCELLED') THEN now() ELSE completed_at END
              WHERE id = $1 AND lease_id IS NOT DISTINCT FROM $2",
            vec![job.id.into(), job.lease_id.into(), status.into(), class.map(str::to_string).into()],
        )
        .await?;
        db::exec(
            &self.db,
            "INSERT INTO notif_delivery_attempts (delivery_id, attempt, started_at, finished_at, outcome, error_class)
             VALUES ($1, $2, now(), now(), $3, $4) ON CONFLICT DO NOTHING",
            vec![job.id.into(), job.attempts.into(), outcome.into(), class.map(str::to_string).into()],
        )
        .await?;
        Ok(())
    }

    pub async fn tick(&self, now: DateTime<Utc>) -> Result<PartnerTick, DbErr> {
        let lease = Uuid::new_v4();
        let claimed: Vec<Claimed> = db::all(
            &self.db,
            "UPDATE notif_deliveries d SET status = 'DISPATCHING', lease_id = $1,
                    lease_until = now() + make_interval(secs => $2), attempts = LEAST(d.attempts + 1, 64)
               FROM notif_notifications n
              WHERE n.id = d.notification_id AND d.id IN (
                    SELECT id FROM notif_deliveries
                     WHERE channel = 'PUSH' AND target_ref LIKE 'partner:%'
                       AND ((status IN ('PENDING', 'DEFERRED') AND next_attempt_at <= now())
                        OR (status = 'DISPATCHING' AND lease_until < now()))
                     ORDER BY next_attempt_at
                     LIMIT 16 FOR UPDATE SKIP LOCKED)
          RETURNING d.id, d.lease_id, d.target_ref, d.attempts, n.template_key, n.params, n.expires_at, n.recipient_ref",
            vec![lease.into(), LEASE_SECS.into()],
        )
        .await?;
        let mut t = PartnerTick {
            claimed: claimed.len() as u64,
            ..Default::default()
        };
        for job in claimed {
            if job.expires_at.is_some_and(|e| e <= now) || job.attempts > 8 {
                self.finish(&job, "EXPIRED", None, "PERMANENT").await?;
                t.expired += 1;
                continue;
            }
            let endpoint = job
                .target_ref
                .strip_prefix(TARGET_PREFIX)
                .and_then(|s| Uuid::parse_str(s).ok());
            let row: Option<PartnerEndpoint> = match endpoint {
                Some(id) => {
                    db::one(
                        &self.db,
                        "SELECT tenant_id, application_id, environment_id, principal_id, platform, handle_sealed, enabled
                           FROM notif_push_endpoints WHERE id = $1 AND subject_kind = 'platform-principal'",
                        vec![id.into()],
                    )
                    .await?
                }
                None => None,
            };
            let (Some(endpoint), Some(row)) = (endpoint, row) else {
                self.finish(&job, "FAILED", Some("BAD_TARGET"), "PERMANENT")
                    .await?;
                t.failed += 1;
                continue;
            };
            let scope = Scope {
                tenant_id: row.tenant_id,
                application_id: row.application_id,
                environment_id: row.environment_id,
            };
            // Defence in depth: the endpoint must belong to the notification's recipient.
            let owner = row
                .principal_id
                .map(|p| cf_notification_client::platform_principal_ref(row.environment_id, p));
            if owner.as_deref() != Some(job.recipient_ref.as_str()) {
                self.finish(&job, "FAILED", Some("RECIPIENT_MISMATCH"), "PERMANENT")
                    .await?;
                failed_metric(
                    &self.metrics,
                    &scope,
                    "message",
                    &row.platform,
                    "RECIPIENT_MISMATCH",
                );
                t.failed += 1;
                continue;
            }
            if !row.enabled {
                self.finish(&job, "CANCELLED", Some("ENDPOINT_DISABLED"), "PERMANENT")
                    .await?;
                failed_metric(
                    &self.metrics,
                    &scope,
                    "message",
                    &row.platform,
                    "ENDPOINT_DISABLED",
                );
                t.failed += 1;
                continue;
            }
            let sender =
                match load_credential(&self.db, &self.keys, &self.partner, &scope, &row.platform)
                    .await?
                {
                    Ok(s) => s,
                    Err(e) => {
                        let class = match e {
                            CredentialError::Missing => "NO_CREDENTIAL",
                            CredentialError::Unusable(_) => "CREDENTIAL_UNUSABLE",
                        };
                        self.finish(&job, "FAILED", Some(class), "PERMANENT")
                            .await?;
                        failed_metric(&self.metrics, &scope, "message", &row.platform, class);
                        t.failed += 1;
                        continue;
                    }
                };
            let Ok(handle) = self
                .keys
                .open(&row.handle_sealed, &format!("push:{endpoint}"))
            else {
                self.finish(&job, "FAILED", Some("HANDLE_UNAVAILABLE"), "PERMANENT")
                    .await?;
                failed_metric(
                    &self.metrics,
                    &scope,
                    "message",
                    &row.platform,
                    "HANDLE_UNAVAILABLE",
                );
                t.failed += 1;
                continue;
            };
            let data: std::collections::BTreeMap<String, Value> = job
                .params
                .as_object()
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            // Ids only (never content), collapsed per conversation.
            let payload = crate::templates::push_payload(&job.template_key, &data, 0);
            let options = SendOptions {
                ttl_seconds: job
                    .expires_at
                    .map(|e| (e - now).num_seconds().clamp(0, 86_400) as u64),
                collapse_id: data
                    .get("conversationId")
                    .and_then(Value::as_str)
                    .and_then(|c| Uuid::parse_str(c).ok())
                    .map(|c| c.simple().to_string()),
                voip: false,
            };
            let outcome = match tokio::time::timeout(
                StdDuration::from_secs(20),
                sender.send_with(adapter_platform(&row.platform), &handle, &payload, &options),
            )
            .await
            {
                Ok(o) => o,
                Err(_) => PushOutcome::Retryable("timeout".into()),
            };
            match outcome {
                PushOutcome::Sent => {
                    self.finish(&job, "SENT", None, "OK").await?;
                    sent_metric(&self.metrics, &scope, "message", &row.platform);
                    t.sent += 1;
                }
                PushOutcome::InvalidHandle => {
                    disable_endpoint(&self.db, endpoint).await?;
                    self.finish(&job, "FAILED", Some("INVALID_HANDLE"), "PERMANENT")
                        .await?;
                    failed_metric(
                        &self.metrics,
                        &scope,
                        "message",
                        &row.platform,
                        "INVALID_HANDLE",
                    );
                    t.failed += 1;
                }
                PushOutcome::Permanent(_) => {
                    self.finish(&job, "FAILED", Some("PROVIDER_PERMANENT"), "PERMANENT")
                        .await?;
                    failed_metric(&self.metrics, &scope, "message", &row.platform, "PERMANENT");
                    t.failed += 1;
                }
                PushOutcome::Retryable(_) => {
                    let next = now + crate::email::backoff(job.attempts);
                    db::exec(
                        &self.db,
                        "UPDATE notif_deliveries SET status = 'PENDING', next_attempt_at = $3, lease_id = NULL, lease_until = NULL,
                                last_error_class = 'PROVIDER_RETRYABLE'
                          WHERE id = $1 AND lease_id IS NOT DISTINCT FROM $2",
                        vec![job.id.into(), job.lease_id.into(), next.into()],
                    )
                    .await?;
                    failed_metric(&self.metrics, &scope, "message", &row.platform, "RETRYABLE");
                    t.retried += 1;
                }
            }
        }
        Ok(t)
    }

    pub async fn run(
        &self,
        poll: StdDuration,
        heartbeats: Arc<Heartbeats>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        tracing::info!(
            transport = self.partner.transport.name(),
            "partner push dispatcher started"
        );
        loop {
            if *shutdown.borrow() {
                break;
            }
            heartbeats.beat("partner-push");
            let busy = match self.tick(Utc::now()).await {
                Ok(t) => t.claimed > 0,
                Err(e) => {
                    tracing::error!(summary = %db::summarize_db_error(&e), "partner push dispatcher tick failed");
                    false
                }
            };
            if !busy {
                tokio::select! {
                    _ = tokio::time::sleep(poll) => {}
                    _ = shutdown.changed() => {}
                }
            }
        }
    }
}

/// The enabled message endpoints (APNS / FCM; VoIP endpoints only ring calls) of a partner
/// recipient, optionally restricted to the event's scope.
pub async fn message_endpoints<C: ConnectionTrait>(
    conn: &C,
    environment_id: Uuid,
    principal_id: Uuid,
    scope: Option<(Uuid, Uuid, Uuid)>,
) -> Result<Vec<push::EndpointRef>, DbErr> {
    db::all(
        conn,
        "SELECT id, platform FROM notif_push_endpoints
          WHERE subject_kind = 'platform-principal' AND environment_id = $1 AND principal_id = $2 AND enabled
            AND platform IN ('APNS', 'FCM')
            AND ($3::uuid IS NULL OR tenant_id = $3) AND ($4::uuid IS NULL OR application_id = $4)
          ORDER BY id",
        vec![
            environment_id.into(),
            principal_id.into(),
            scope.map(|s| s.0).into(),
            scope.map(|s| s.1).into(),
        ],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            tenant_id: Uuid::from_u128(1),
            application_id: Uuid::from_u128(2),
            environment_id: Uuid::from_u128(3),
        }
    }

    #[test]
    fn metadata_is_checked_per_provider() {
        let mut apns = ProviderMetadata {
            team_id: Some("ABCDE12345".into()),
            key_id: Some("KEY1234567".into()),
            bundle_id: Some("com.example.app.voip".into()),
            apns_host: Some("SANDBOX".into()),
            project_id: None,
        };
        apns.validate("APNS_VOIP").unwrap();
        assert_eq!(apns.bundle_id.as_deref(), Some("com.example.app"));
        let mut bad = apns.clone();
        bad.apns_host = None;
        assert!(bad.validate("APNS").is_err());
        let mut bad = apns.clone();
        bad.team_id = Some("short".into());
        assert!(bad.validate("APNS").is_err());
        assert!(apns.clone().validate("FCM").is_err());
        let mut fcm = ProviderMetadata {
            project_id: Some("demo-project".into()),
            ..Default::default()
        };
        fcm.validate("FCM").unwrap();
        fcm.project_id = Some("Demo Project".into());
        assert!(fcm.validate("FCM").is_err());
        assert!(ProviderMetadata::default().validate("WEB_PUSH").is_err());
    }

    #[test]
    fn opened_credentials_never_print_their_secret() {
        let c = OpenedCredential::new(
            scope(),
            "FCM",
            ProviderMetadata::default(),
            "ab".repeat(32),
            "TOP-SECRET-KEY".into(),
        );
        assert!(!format!("{c:?}").contains("TOP-SECRET"));
        assert_eq!(c.secret(), "TOP-SECRET-KEY");
    }

    #[test]
    fn credential_seals_are_bound_to_scope_and_provider() {
        let keys = crate::crypto::test_keys();
        let s = scope();
        let sealed = keys.seal("secret", &credential_owner(&s, "APNS"));
        assert!(keys.open(&sealed, &credential_owner(&s, "APNS")).is_ok());
        assert!(keys
            .open(&sealed, &credential_owner(&s, "APNS_VOIP"))
            .is_err());
        let other = Scope {
            environment_id: Uuid::from_u128(4),
            ..s
        };
        assert!(keys
            .open(&sealed, &credential_owner(&other, "APNS"))
            .is_err());
    }

    #[test]
    fn sealed_values_are_read_in_every_accepted_form() {
        let s = Sealed {
            kid: "notif-hpke-2026-11".into(),
            enc: "abc".into(),
            ct: "def".into(),
        };
        let whole = Some(SealedCredential::Whole(s.clone()));
        assert_eq!(parse_sealed(&json!(s.to_compact())), whole);
        assert_eq!(parse_sealed(&serde_json::to_value(&s).unwrap()), whole);
        assert_eq!(
            parse_sealed(&json!(serde_json::to_string(&s).unwrap())),
            whole
        );
        let multi = json!({ "v": 1, "parts": [s.clone(), s.clone()] });
        let parts = Some(SealedCredential::Parts(vec![s.clone(), s.clone()]));
        assert_eq!(parse_sealed(&multi), parts);
        assert_eq!(parse_sealed(&json!(multi.to_string())), parts);
        assert_eq!(parse_sealed(&json!({ "v": 2, "parts": [s.clone()] })), None);
        assert_eq!(parse_sealed(&json!({ "v": 1, "parts": [] })), None);
        assert_eq!(parse_sealed(&json!(42)), None);
        assert_eq!(parse_sealed(&json!("plain text secret")), None);
    }

    #[test]
    fn provider_answers_to_the_placeholder_classify_the_credential() {
        use PushOutcome::*;
        assert_eq!(
            classify_verify("APNS", &InvalidHandle),
            VerifyOutcome::Verified
        );
        assert_eq!(
            classify_verify("APNS", &Retryable("apns 403 InvalidProviderToken".into())),
            VerifyOutcome::Failed("PROVIDER_TOKEN_REFUSED")
        );
        assert_eq!(
            classify_verify("APNS", &Permanent("apns 400 TopicDisallowed".into())),
            VerifyOutcome::Failed("BAD_TOPIC")
        );
        assert_eq!(
            classify_verify("APNS", &Retryable("apns transport: connect".into())),
            VerifyOutcome::Unavailable
        );
        assert_eq!(
            classify_verify("FCM", &Permanent("fcm token refused 400".into())),
            VerifyOutcome::Failed("PROVIDER_TOKEN_REFUSED")
        );
        assert_eq!(
            classify_verify("FCM", &Permanent("fcm 400 INVALID_ARGUMENT".into())),
            VerifyOutcome::Verified
        );
        assert_eq!(
            classify_verify("FCM", &InvalidHandle),
            VerifyOutcome::Verified
        );
        assert_eq!(
            classify_verify("FCM", &Permanent("fcm 403 PERMISSION_DENIED".into())),
            VerifyOutcome::Failed("PROVIDER_REFUSED")
        );
    }

    /// A fresh APNs-style `.p8` (PKCS#8 P-256 PEM).
    pub(crate) fn p8() -> String {
        use p256::pkcs8::{EncodePrivateKey, LineEnding};
        let secret = p256::SecretKey::from_slice(&rand::random::<[u8; 32]>()).unwrap();
        secret.to_pkcs8_pem(LineEnding::LF).unwrap().to_string()
    }

    #[test]
    fn the_live_transport_parses_partner_keys_locally() {
        let live = LivePartnerTransport;
        let meta = ProviderMetadata {
            team_id: Some("ABCDE12345".into()),
            key_id: Some("KEY1234567".into()),
            bundle_id: Some("com.example.app".into()),
            apns_host: Some("SANDBOX".into()),
            project_id: None,
        };
        let fp = "ab".repeat(32);
        let ok = OpenedCredential::new(scope(), "APNS_VOIP", meta.clone(), fp.clone(), p8());
        live.check(&ok).unwrap();
        assert!(live.sender(&ok).unwrap().supports("APNS"));
        let bad = OpenedCredential::new(
            scope(),
            "APNS",
            meta,
            fp.clone(),
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n".into(),
        );
        assert_eq!(live.check(&bad).unwrap_err(), "INVALID_KEY");
        // FCM: not a service account; a service account of another project; a key that is
        // not RSA.
        let fcm = |secret: &str, project: Option<&str>| {
            OpenedCredential::new(
                scope(),
                "FCM",
                ProviderMetadata {
                    project_id: project.map(str::to_string),
                    ..Default::default()
                },
                fp.clone(),
                secret.into(),
            )
        };
        assert_eq!(live.check(&fcm("{}", None)).unwrap_err(), "INVALID_KEY");
        let account = json!({ "type": "service_account", "project_id": "demo", "client_email": "x@demo.iam.gserviceaccount.com", "private_key": p8() }).to_string();
        assert_eq!(
            live.check(&fcm(&account, Some("other"))).unwrap_err(),
            "PROJECT_MISMATCH"
        );
        assert_eq!(
            live.check(&fcm(&account, Some("demo"))).unwrap_err(),
            "INVALID_KEY"
        );
    }

    #[test]
    fn installation_ids_follow_the_contract() {
        assert!(installation_ok("inst-1234"));
        assert!(installation_ok("A.b_c-0123"));
        assert!(!installation_ok("short"));
        assert!(!installation_ok("has space 123"));
        assert!(!installation_ok(&"x".repeat(129)));
    }
}
