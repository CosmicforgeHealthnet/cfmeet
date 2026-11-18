//! Part VI §51 / §53 — platform-credential access to the Meeting Service.
//!
//! * [`authority`]: Platform Access re-verifies a gateway-forwarded credential LIVE
//!   (`delegated-authority`); the machine actor comes only from its authority chain.
//! * [`operations`]: the `/api/v2` operations machine callers may perform and their scopes.
//! * [`events`]: environment-scoped meeting events, staged in the transactional outbox and
//!   published to the Developer Platform as platform envelopes.
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `MEETING_PLATFORM_ACCESS_ENABLED` | `false` | accept API Gateway–forwarded platform credential requests on the machine-callable `/api/v2` operations (otherwise every service token there is the normalized 401) |
//! | `MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION` | `none` | `none` or `developer-platform` (see [`events`]) |
//! | `CF_RUNTIME_ENVIRONMENT` | — | `local`, `test`, `staging` or `production` (when either feature is on) |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | — | receiver trust bundle of `cfmeet.meeting-service` (platform access) |
//! | `CF_SERVICE_AUTH_KEY_ID` / `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | — | the Meeting Service's Ed25519 signing key (PKCS#8 PEM FILE) |
//! | `CF_SERVICE_TOKEN_TTL_SECONDS` | `120` | 10–300 |
//! | `CF_INTERNAL_HTTP_TIMEOUT_MS` | `5000` | 100–30000 |
//! | `CF_PLATFORM_ACCESS_BASE_URL` | — | Platform Access (platform access) |
//! | `CF_DEVELOPER_PLATFORM_BASE_URL` | — | Developer Platform (`developer-platform` destination, usage export) |
//! | `CFMEET_USAGE_EXPORT_ENABLED` | `false` | Part VI §55: export usage facts of environment-owned meetings to the Developer Platform (see [`usage_export`]; `CFMEET_USAGE_EXPORT_*` tuning) |
//! | `MEETING_SERVICE_PUBLIC_EVENTS_INTERVAL_MS` | `1000` | publisher poll interval (100–60000) |
//! | `MEETING_PLATFORM_USER_JOIN_ENABLED` | `false` | ADR 0062: partner users join environment-owned meetings with their CF Meet user token (`Authorization: Bearer cfut_…`, join only; see [`user_token`]); requires `CF_PLATFORM_ACCESS_BASE_URL` and the `platform-build-migration` history |
//! | `MEETING_PLATFORM_USER_TOKEN_CACHE_SECONDS` | `30` | positive verification cache (0–30; 0 disables) |
//! | `MEETING_PLATFORM_CHAT_ENABLED` | `false` | ADR 0062: partner meeting chat in the partner environment's scope (see [`partner_chat`]); requires `MEETING_PLATFORM_USER_JOIN_ENABLED` and `CF_MESSAGING_BASE_URL` |
//! | `MEETING_PLATFORM_CHAT_SYNC_INTERVAL_MS` | `2000` | partner chat background pass interval (200–60000) |
//! | `CF_MESSAGING_BASE_URL` | — | Messaging (partner meeting chat) |
//!
//! An enabled but misconfigured feature stops startup (fail closed).

pub mod authority;
pub mod events;
pub mod operations;
pub mod partner_chat;
pub mod usage_export;
pub mod user_token;

use std::{future::Future, path::PathBuf, sync::Arc, time::Duration};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    CorrelationId, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};

use crate::config::env_bool;

pub use authority::{CredentialAuthority, SharedCredentialAuthority};
pub use events::PublicEventsDestination;

/// Platform Access scope names the machine-callable operations require.
pub mod scopes {
    pub const MEETINGS_READ: &str = "meetings.read";
    pub const MEETINGS_WRITE: &str = "meetings.write";
}

/// ADR 0062: partner users (CF Meet user tokens) and, optionally, partner meeting chat.
#[derive(Clone)]
pub struct PlatformUsers {
    /// Platform Access's live verification (behind the ≤30 s positive cache).
    pub verifier: user_token::SharedUserTokenVerifier,
    /// Partner meeting chat (`MEETING_PLATFORM_CHAT_ENABLED`).
    pub chat: Option<partner_chat::SharedPartnerChat>,
}

/// The receiver side of gateway-forwarded platform credential requests.
#[derive(Clone)]
pub struct PlatformReceiver {
    /// The Meeting Service's own trust bundle (receiver `cfmeet.meeting-service`).
    pub verifier: Arc<ServiceTokenVerifier>,
    pub authority: SharedCredentialAuthority,
}

tokio::task_local! {
    static CORRELATION: CorrelationId;
}

/// Runs `fut` continuing the gateway's signed correlation id (carried into the platform
/// envelopes of the events the request produces).
pub async fn with_correlation<F: Future>(correlation: CorrelationId, fut: F) -> F::Output {
    CORRELATION.scope(correlation, fut).await
}

/// The correlation id of the forwarded request being served, if any.
pub fn current_correlation() -> Option<CorrelationId> {
    CORRELATION.try_with(|c| c.clone()).ok()
}

/// Configuration of the platform features.
#[derive(Debug, Clone)]
pub struct PlatformConfig {
    pub access_enabled: bool,
    pub destination: PublicEventsDestination,
    pub runtime: RuntimeEnvironment,
    pub trust_bundle_file: Option<PathBuf>,
    pub key_id: KeyId,
    pub key_file: PathBuf,
    pub token_ttl_seconds: u64,
    pub timeout: Duration,
    pub platform_access_url: Option<String>,
    pub developer_platform_url: Option<String>,
    pub publish_interval: Duration,
    /// Part VI §55 usage export settings (`None`: off).
    pub usage_export: Option<cf_usage_facts::exporter::ExportSettings>,
    /// ADR 0062: `MEETING_PLATFORM_USER_JOIN_ENABLED`.
    pub user_join_enabled: bool,
    /// ADR 0062: `MEETING_PLATFORM_CHAT_ENABLED` (implies user join).
    pub chat_enabled: bool,
    pub user_token_cache: Duration,
    pub messaging_url: Option<String>,
    pub chat_sync_interval: Duration,
}

fn var(name: &str, why: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(format!("{name} is required when {why}")),
    }
}

fn bounded(name: &str, default: u64, min: u64, max: u64) -> Result<u64, String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => {
            let n: u64 = v
                .trim()
                .parse()
                .map_err(|_| format!("{name} must be an integer"))?;
            if (min..=max).contains(&n) {
                Ok(n)
            } else {
                Err(format!("{name} must be between {min} and {max}"))
            }
        }
        _ => Ok(default),
    }
}

impl PlatformConfig {
    /// `Ok(None)`: both features off. `Err`: on but misconfigured.
    pub fn from_env() -> Result<Option<Self>, String> {
        let access_enabled = env_bool("MEETING_PLATFORM_ACCESS_ENABLED", false);
        let destination = match std::env::var(PublicEventsDestination::VARIABLE) {
            Ok(v) if !v.trim().is_empty() => {
                PublicEventsDestination::parse(&v).ok_or_else(|| {
                    format!(
                        "{} must be none or developer-platform",
                        PublicEventsDestination::VARIABLE
                    )
                })?
            }
            _ => PublicEventsDestination::default(),
        };
        let usage_export =
            cf_usage_facts::exporter::ExportSettings::from_env(|k| std::env::var(k).ok())?;
        let usage_export = usage_export.enabled.then_some(usage_export);
        let user_join_enabled = env_bool("MEETING_PLATFORM_USER_JOIN_ENABLED", false);
        let chat_enabled = env_bool("MEETING_PLATFORM_CHAT_ENABLED", false);
        if chat_enabled && !user_join_enabled {
            return Err(
                "MEETING_PLATFORM_CHAT_ENABLED requires MEETING_PLATFORM_USER_JOIN_ENABLED".into(),
            );
        }
        if !access_enabled
            && destination == PublicEventsDestination::None
            && usage_export.is_none()
            && !user_join_enabled
        {
            return Ok(None);
        }
        let why = "MEETING_PLATFORM_ACCESS_ENABLED, MEETING_PLATFORM_USER_JOIN_ENABLED, a public events destination or usage export is set";
        let runtime = RuntimeEnvironment::parse(&var("CF_RUNTIME_ENVIRONMENT", why)?)
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT must be local, test, staging or production")?;
        let key_id = KeyId::parse(&var("CF_SERVICE_AUTH_KEY_ID", why)?)
            .map_err(|_| "CF_SERVICE_AUTH_KEY_ID is not a valid key id")?;
        let access_why = "MEETING_PLATFORM_ACCESS_ENABLED is on";
        let (trust_bundle_file, platform_access_url) = if access_enabled {
            (
                Some(PathBuf::from(var(
                    "CF_SERVICE_TRUST_BUNDLE_FILE",
                    access_why,
                )?)),
                Some(var("CF_PLATFORM_ACCESS_BASE_URL", access_why)?),
            )
        } else if user_join_enabled {
            (
                None,
                Some(var(
                    "CF_PLATFORM_ACCESS_BASE_URL",
                    "MEETING_PLATFORM_USER_JOIN_ENABLED is on",
                )?),
            )
        } else {
            (None, None)
        };
        let messaging_url = if chat_enabled {
            Some(var(
                "CF_MESSAGING_BASE_URL",
                "MEETING_PLATFORM_CHAT_ENABLED is on",
            )?)
        } else {
            None
        };
        let developer_platform_url = match (destination, &usage_export) {
            (PublicEventsDestination::DeveloperPlatform, _) => Some(var(
                "CF_DEVELOPER_PLATFORM_BASE_URL",
                "MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION is developer-platform",
            )?),
            (PublicEventsDestination::None, Some(_)) => Some(var(
                "CF_DEVELOPER_PLATFORM_BASE_URL",
                "CFMEET_USAGE_EXPORT_ENABLED is on",
            )?),
            (PublicEventsDestination::None, None) => None,
        };
        Ok(Some(Self {
            access_enabled,
            destination,
            runtime,
            trust_bundle_file,
            key_id,
            key_file: PathBuf::from(var("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", why)?),
            token_ttl_seconds: bounded("CF_SERVICE_TOKEN_TTL_SECONDS", 120, 10, 300)?,
            timeout: Duration::from_millis(bounded(
                "CF_INTERNAL_HTTP_TIMEOUT_MS",
                5000,
                100,
                30_000,
            )?),
            platform_access_url,
            developer_platform_url,
            publish_interval: Duration::from_millis(bounded(
                "MEETING_SERVICE_PUBLIC_EVENTS_INTERVAL_MS",
                1000,
                100,
                60_000,
            )?),
            usage_export,
            user_join_enabled,
            chat_enabled,
            user_token_cache: Duration::from_secs(bounded(
                "MEETING_PLATFORM_USER_TOKEN_CACHE_SECONDS",
                user_token::MAX_CACHE_SECONDS,
                0,
                user_token::MAX_CACHE_SECONDS,
            )?),
            messaging_url,
            chat_sync_interval: Duration::from_millis(bounded(
                "MEETING_PLATFORM_CHAT_SYNC_INTERVAL_MS",
                2000,
                200,
                60_000,
            )?),
        }))
    }

    /// The signed internal client (Platform Access and / or the Developer Platform).
    pub fn client(&self) -> Result<Arc<InternalServiceClient>, String> {
        let key = ServiceSigningKey::from_pkcs8_pem_file(self.key_id.clone(), &self.key_file)
            .map_err(|_| "CF_SERVICE_AUTH_PRIVATE_KEY_FILE is not a readable PKCS#8 Ed25519 key")?;
        let ttl = TokenTtl::seconds(self.token_ttl_seconds)
            .map_err(|_| "CF_SERVICE_TOKEN_TTL_SECONDS is out of range")?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            self.runtime,
            key,
            ttl,
            Arc::new(SystemClock),
        ));
        let mut targets = InternalTargets::new(self.runtime);
        if let Some(url) = &self.platform_access_url {
            targets
                .insert(ServiceId::platform_access(), url)
                .map_err(|e| format!("CF_PLATFORM_ACCESS_BASE_URL: {e}"))?;
        }
        if let Some(url) = &self.developer_platform_url {
            targets
                .insert(ServiceId::developer_platform(), url)
                .map_err(|e| format!("CF_DEVELOPER_PLATFORM_BASE_URL: {e}"))?;
        }
        if let Some(url) = &self.messaging_url {
            targets
                .insert(ServiceId::messaging(), url)
                .map_err(|e| format!("CF_MESSAGING_BASE_URL: {e}"))?;
        }
        Ok(Arc::new(
            InternalServiceClient::new(signer, targets, self.timeout)
                .map_err(|e| format!("internal client: {e}"))?,
        ))
    }

    /// ADR 0062: partner-user joins (and partner meeting chat), when enabled.
    pub fn platform_users(
        &self,
        db: &sea_orm::DatabaseConnection,
        client: Arc<InternalServiceClient>,
    ) -> Option<PlatformUsers> {
        if !self.user_join_enabled {
            return None;
        }
        let verifier = Arc::new(user_token::CachedUserTokens::new(
            Arc::new(user_token::PlatformAccessUserTokens::new(client.clone())),
            self.user_token_cache,
        ));
        let chat = self.chat_enabled.then(|| {
            Arc::new(partner_chat::PartnerChat::new(
                db.clone(),
                Arc::new(partner_chat::ServiceAuthPartnerChatGateway::new(client)),
            ))
        });
        Some(PlatformUsers { verifier, chat })
    }

    /// The receiver (verifier + Platform Access authority), when platform access is on.
    pub fn receiver(
        &self,
        client: Arc<InternalServiceClient>,
    ) -> Result<Option<PlatformReceiver>, String> {
        let Some(bundle) = self
            .trust_bundle_file
            .as_ref()
            .filter(|_| self.access_enabled)
        else {
            return Ok(None);
        };
        let policy = TrustPolicy::from_file(bundle, &ServiceId::meeting_service(), self.runtime)
            .map_err(|e| format!("CF_SERVICE_TRUST_BUNDLE_FILE: {e}"))?;
        Ok(Some(PlatformReceiver {
            verifier: Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock))),
            authority: Arc::new(authority::PlatformAccessAuthority::new(client)),
        }))
    }
}
