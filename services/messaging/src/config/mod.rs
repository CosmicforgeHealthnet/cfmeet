//! Messaging configuration, read from the environment. Validation fails closed and error
//! messages never echo a value.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | Messaging's OWN PostgreSQL database (runtime role) |
//! | `MESSAGING_DATABASE_MAX_CONNECTIONS` | no | pool size (1–500, default 10) |
//! | `MESSAGING_BIND_ADDRESS` | no | default `127.0.0.1:8097` |
//! | `MESSAGING_METRICS_ADDRESS` | no | separate `GET /metrics` listener (e.g. `127.0.0.1:9197`) |
//! | `MESSAGING_E2EE_DEVICES_ENABLED` | no | `true` / `false`: device registration and KeyPackage publication. Default `true` only in `local` / `test` runtimes; staging requires an explicit `true` (owner approval of the cryptographic decisions, ADR 0045); `true` is refused in `production` (Part V J: production E2EE stays disabled pending external certification) |
//! | `MESSAGING_DEVICE_CHALLENGE_TTL_SECONDS` | no | registration challenge validity (60–3600, default 300) |
//! | `MESSAGING_DEVICE_CHALLENGES_PER_HOUR` | no | challenges per principal per hour (1–1000, default 20) |
//! | `MESSAGING_MAX_ACTIVE_DEVICES` | no | active devices per principal (1–100, default 10) |
//! | `MESSAGING_MAX_KEY_PACKAGES_PER_DEVICE` | no | AVAILABLE KeyPackages per device (1–1000, default 100) |
//! | `MESSAGING_KEY_PACKAGE_MAX_LIFETIME_DAYS` | no | longest accepted KeyPackage lifetime (1–90, default 90) |
//! | `MESSAGING_DIRECTORY_LOOKUPS_PER_MINUTE` | no | device-directory / proof lookups per credential per minute (1–100000, default 600) |
//! | `MESSAGING_AUTH_CACHE_TTL_SECONDS` | no | positive authentication cache for READ routes (0–60, default 10; 0 disables). Mutations always re-authenticate |
//! | `MESSAGING_IDENTITY_FEED_INTERVAL_MS` | no | Platform Access change-feed poll (250–60000, default 2000); any change flushes the cache, a failed poll disables it |
//! | `MESSAGING_CONVERSATIONS_PER_MINUTE` | no | conversation creations per acting principal per minute (1–100000, default 120) |
//! | `MESSAGING_EXPIRY_INTERVAL_MS` | no | conversation expiry worker interval (250–600000, default 5000) |
//! | `MESSAGING_WORKER_INTERVAL_MS` / `MESSAGING_WORKER_LEASE_SECONDS` / `MESSAGING_OUTBOX_MAX_ATTEMPTS` | no | outbox publisher (defaults 1000 / 60 / 12) |
//! | `MESSAGING_KT_LOG_ID` | yes | identifier of this deployment's key-transparency log, signed into every tree head (`[a-z0-9][a-z0-9.:-]{2,63}`, stable across key rotation) |
//! | `MESSAGING_KT_SIGNING_KEY_FILE` | yes | PKCS#8 PEM Ed25519 private key FILE that signs tree heads (Part V G.5 / §142). A server secret distinct from the service-authentication key; inline `MESSAGING_KT_SIGNING_KEY` is refused |
//! | `MESSAGING_MAX_MEMBERS` | no | ACTIVE members per conversation (2–100000, default 10000; policy may lower it) |
//! | `MESSAGING_ENVIRONMENT_MESSAGES_PER_MINUTE` | no | tenant-fairness bound per application environment (default 60000) |
//! | `MESSAGING_ABUSE_SIGNAL_THRESHOLD` / `MESSAGING_ABUSE_RESTRICTION_SECONDS` | no | automatic temporary sending restriction (defaults 10 per hour / 900 s) |
//! | `MESSAGING_MANAGED_KEYRING_FILE` | no | key-ring FILE sealing MANAGED message text at rest (`<id> <base64url 32-byte key>` per line, first = active). Without it MANAGED sending fails closed |
//! | `MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE` | no | PKCS#8 PEM Ed25519 key FILE signing Messaging Client Access Tokens (a Messaging-only secret). Without it no client token is issued: realtime is disabled (fail closed) |
//! | `MESSAGING_CLIENT_TOKEN_TTL_SECONDS` / `MESSAGING_REALTIME_TICKET_TTL_SECONDS` | no | token lifetime (60–86400, default 900) / single-use ticket lifetime (5–300, default 30) |
//! | `MESSAGING_REALTIME_ALLOWED_ORIGINS` | no | comma-separated exact browser origins allowed to connect (default: none; native clients send no Origin) |
//! | `MESSAGING_REALTIME_REQUIRE_TLS` | no | refuse handshakes not forwarded as `https` (default `true` outside local / test) |
//! | `MESSAGING_REALTIME_REDIS_URL` | no | self-hosted Redis for cross-instance fan-out and presence (secret; default: single-instance in-process broker) |
//! | `MESSAGING_REALTIME_HEARTBEAT_SECONDS` / `_IDLE_TIMEOUT_SECONDS` / `_REVALIDATE_SECONDS` | no | defaults 20 / 60 / 30 |
//! | `MESSAGING_REALTIME_MAX_FRAME_BYTES` / `_SEND_QUEUE` / `_MAX_SUBSCRIPTIONS` / `_MAX_REPLAY` | no | defaults 65536 / 512 / 200 / 500 |
//! | `MESSAGING_REALTIME_MAX_CONNECTIONS` / `_PER_TENANT_CONNECTIONS` / `_PER_PRINCIPAL_CONNECTIONS` / `_PER_DEVICE_CONNECTIONS` | no | per instance: defaults 50000 / 10000 / 20 / 5 |
//! | `MESSAGING_REALTIME_FRAMES_PER_SECOND` / `_DRAIN_SECONDS` | no | defaults 20 / 10 |
//! | `MESSAGING_PUSH_PROVIDER` | no | `none` (default) or `local-test` (local / test runtimes only): the explicitly local adapter |
//! | `MESSAGING_PUSH_APNS_KEY_FILE` / `_KEY_ID` / `_TEAM_ID` / `_TOPIC` | no | APNs token-based adapter (all four, or none): the `.p8` key file, its key id, the team id, the bundle id. `MESSAGING_PUSH_APNS_ENVIRONMENT` `production` (default) / `sandbox`; `MESSAGING_PUSH_APNS_BASE_URL` overrides the endpoint in local / test runtimes only |
//! | `MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE` | no | FCM HTTP v1 adapter: the Google service-account key file (secret). `MESSAGING_PUSH_FCM_BASE_URL` overrides the endpoint in local / test runtimes only |
//! | `MESSAGING_PUSH_WEB_VAPID_KEY_FILE` / `_VAPID_SUBJECT` | no | Web Push adapter (both, or neither): the VAPID P-256 key (PKCS#8 PEM) and its `mailto:` / `https:` contact. `MESSAGING_PUSH_WEB_ALLOWED_HOSTS` replaces the push-service host allowlist (comma-separated, `*.` = subdomains) |
//! | (unset adapters) | | jobs for that platform end `NOT_CONFIGURED`, never reported as delivered |
//! | `MESSAGING_ATTACHMENTS_S3_ENDPOINT` … | no | the PRIVATE attachment bucket (`ENDPOINT`, `PUBLIC_ENDPOINT`, `REGION`, `BUCKET`, `ACCESS_KEY_ID`, `SECRET_ACCESS_KEY[_FILE]`, `PATH_STYLE`, `SERVER_SIDE_ENCRYPTION`; see `packages/object-storage`). Unset: attachments fail closed |
//! | `MESSAGING_CLAMAV_ADDRESS` | no | self-hosted clamd `host:port` scanning MANAGED attachments. Unset: scans stay pending (never READY where policy requires scanning) |
//! | `MESSAGING_UPLOAD_INTENT_SECONDS` / `MESSAGING_DOWNLOAD_GRANT_SECONDS` | no | defaults 900 / 60 (residual access window of a grant) |
//! | `MESSAGING_STORAGE_BUDGET_BYTES` | no | deployment cap of stored bytes per environment (default: none; policies may set lower) |
//! | `MESSAGING_MANAGED_SCAN_MAX_BYTES` / `MESSAGING_VERIFY_READ_MAX_BYTES` / `MESSAGING_PROXY_DOWNLOAD_MAX_BYTES` | no | defaults 50 MiB / 64 MiB / 32 MiB |
//! | `MESSAGING_UNLINKED_ATTACHMENT_HOURS` | no | READY uploads never referenced by a message are deleted after (default 24) |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//!
//! Service authentication (`CF_*`) is read by [`service_auth`].

pub mod service_auth;

use std::{fmt, net::SocketAddr, time::Duration};

use cf_service_auth::RuntimeEnvironment;
use secrecy::{ExposeSecret, SecretString};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is not set")]
    Missing { var: &'static str },
    /// The message never echoes the offending value (it may be a secret).
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

impl ConfigError {
    pub fn invalid(var: &'static str, reason: impl Into<String>) -> Self {
        ConfigError::Invalid {
            var,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

/// Device / key-directory policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSettings {
    pub enabled: bool,
    pub challenge_ttl: chrono::Duration,
    pub challenges_per_hour: i64,
    pub max_active_devices: i64,
    pub max_key_packages_per_device: i64,
    pub key_package_max_lifetime_days: u64,
    pub directory_lookups_per_minute: i64,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            challenge_ttl: chrono::Duration::seconds(300),
            challenges_per_hour: 20,
            max_active_devices: 10,
            max_key_packages_per_device: 100,
            key_package_max_lifetime_days: 90,
            directory_lookups_per_minute: 600,
        }
    }
}

impl DeviceSettings {
    /// The KeyPackage lifetime policy derived from these settings.
    pub fn lifetime_policy(&self) -> cf_messaging_crypto::key_package::LifetimePolicy {
        cf_messaging_crypto::key_package::LifetimePolicy {
            max_total_seconds: self.key_package_max_lifetime_days * 86_400 + 3_600,
            ..Default::default()
        }
    }
}

/// Authorization caching (Part V B.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthCacheSettings {
    pub ttl: Duration,
    pub feed_interval: Duration,
}

impl Default for AuthCacheSettings {
    fn default() -> Self {
        Self {
            ttl: Duration::ZERO,
            feed_interval: Duration::from_millis(2_000),
        }
    }
}

/// Conversation-domain limits and workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationSettings {
    pub creations_per_minute: i64,
    pub expiry_interval: Duration,
    /// Platform bound of ACTIVE members per conversation (policy may lower it).
    pub max_members: i64,
    /// Messages per environment per minute (tenant fairness on shared capacity).
    pub environment_messages_per_minute: i64,
    /// Abuse signals per principal per hour that trigger an automatic restriction.
    pub abuse_signal_threshold: i64,
    pub abuse_restriction_seconds: i64,
}

impl Default for ConversationSettings {
    fn default() -> Self {
        Self {
            creations_per_minute: 120,
            expiry_interval: Duration::from_millis(5_000),
            max_members: 10_000,
            environment_messages_per_minute: 60_000,
            abuse_signal_threshold: 10,
            abuse_restriction_seconds: 900,
        }
    }
}

/// Attachment storage, verification and download settings (§69–§80).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentSettings {
    pub intent_ttl: chrono::Duration,
    pub grant_ttl: Duration,
    /// Deployment cap of stored bytes per application environment (`None`: policy only).
    pub storage_budget_bytes: Option<i64>,
    pub managed_scan_max_bytes: usize,
    /// Largest object finalization reads to hash when the store reports no checksum.
    pub verify_read_max_bytes: usize,
    pub proxy_max_bytes: usize,
    pub unlinked_ttl: chrono::Duration,
    pub scan_retry_max: i32,
}

impl Default for AttachmentSettings {
    fn default() -> Self {
        Self {
            intent_ttl: chrono::Duration::minutes(15),
            grant_ttl: Duration::from_secs(60),
            storage_budget_bytes: None,
            managed_scan_max_bytes: 50 * 1024 * 1024,
            verify_read_max_bytes: 64 * 1024 * 1024,
            proxy_max_bytes: 32 * 1024 * 1024,
            unlinked_ttl: chrono::Duration::hours(24),
            scan_retry_max: 10,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerSettings {
    pub interval: Duration,
    pub lease: chrono::Duration,
    pub outbox_max_attempts: i32,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(1_000),
            lease: chrono::Duration::seconds(60),
            outbox_max_attempts: 12,
        }
    }
}

/// The key-transparency tree-head signing key (Part V G.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransparencySettings {
    pub log_id: String,
    pub signing_key_file: std::path::PathBuf,
}

impl TransparencySettings {
    /// Loads the key; errors never echo key material.
    pub fn load(&self) -> Result<crate::transparency::TreeHeadSigner, ConfigError> {
        use ed25519_dalek::pkcs8::DecodePrivateKey as _;
        let pem = std::fs::read_to_string(&self.signing_key_file)
            .map_err(|_| ConfigError::invalid("MESSAGING_KT_SIGNING_KEY_FILE", "cannot be read"))?;
        let key = ed25519_dalek::SigningKey::from_pkcs8_pem(&pem).map_err(|_| {
            ConfigError::invalid(
                "MESSAGING_KT_SIGNING_KEY_FILE",
                "is not a PKCS#8 PEM Ed25519 private key",
            )
        })?;
        crate::transparency::TreeHeadSigner::new(&self.log_id, key)
            .ok_or_else(|| ConfigError::invalid("MESSAGING_KT_LOG_ID", "invalid log id"))
    }
}

pub struct MessagingConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub metrics_address: Option<SocketAddr>,
    pub devices: DeviceSettings,
    pub auth_cache: AuthCacheSettings,
    pub conversations: ConversationSettings,
    pub workers: WorkerSettings,
    pub transparency: TransparencySettings,
    pub managed_keyring_file: Option<std::path::PathBuf>,
    pub realtime: crate::realtime::RealtimeSettings,
    pub client_token_key_file: Option<std::path::PathBuf>,
    pub realtime_redis_url: Option<SecretString>,
    pub push_provider: PushProviderKind,
    pub push_adapters: PushAdapterConfig,
    pub attachments: AttachmentSettings,
    pub attachment_store: Option<cf_object_storage::S3Config>,
    pub clamav_address: Option<String>,
    pub log_format: LogFormat,
}

/// The live push adapters' configuration (files are read and checked at startup).
#[derive(Debug, Clone, Default)]
pub struct PushAdapterConfig {
    pub apns: Option<ApnsConfig>,
    pub fcm: Option<FcmConfig>,
    pub web: Option<WebPushConfig>,
}

#[derive(Debug, Clone)]
pub struct ApnsConfig {
    pub key_file: std::path::PathBuf,
    pub key_id: String,
    pub team_id: String,
    pub topic: String,
    pub base_url: String,
}

#[derive(Debug, Clone)]
pub struct FcmConfig {
    pub service_account_file: std::path::PathBuf,
    pub base_url: String,
}

#[derive(Debug, Clone)]
pub struct WebPushConfig {
    pub vapid_key_file: std::path::PathBuf,
    pub subject: String,
    pub allowed_hosts: Vec<String>,
}

/// Which push adapter dispatches notification jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushProviderKind {
    None,
    LocalTest,
}

impl fmt::Debug for MessagingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessagingConfig")
            .field("database_url", &"[REDACTED]")
            .field("bind_address", &self.bind_address)
            .field("metrics_address", &self.metrics_address)
            .field("devices", &self.devices)
            .field("auth_cache", &self.auth_cache)
            .field("workers", &self.workers)
            .field("transparency", &self.transparency)
            .finish_non_exhaustive()
    }
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8097";

fn bounded(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, ConfigError> {
    match get(var) {
        None => Ok(default),
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| {
                ConfigError::invalid(var, format!("must be an integer between {min} and {max}"))
            }),
    }
}

fn address(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<Option<SocketAddr>, ConfigError> {
    get(var)
        .map(|v| {
            v.parse::<SocketAddr>().map_err(|_| {
                ConfigError::invalid(var, "must be a socket address such as 127.0.0.1:8097")
            })
        })
        .transpose()
}

impl MessagingConfig {
    pub fn from_env(runtime: RuntimeEnvironment) -> Result<Self, ConfigError> {
        Self::from_lookup(runtime, |key| std::env::var(key).ok())
    }

    /// Builds the configuration for `runtime` from an arbitrary key lookup.
    pub fn from_lookup(
        runtime: RuntimeEnvironment,
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let get = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let database_url =
            get("DATABASE_URL")
                .map(SecretString::from)
                .ok_or(ConfigError::Missing {
                    var: "DATABASE_URL",
                })?;
        let bind_address = address(&get, "MESSAGING_BIND_ADDRESS")?
            .unwrap_or_else(|| DEFAULT_BIND_ADDRESS.parse().expect("valid default"));
        let metrics_address = address(&get, "MESSAGING_METRICS_ADDRESS")?;
        if metrics_address == Some(bind_address) {
            return Err(ConfigError::invalid(
                "MESSAGING_METRICS_ADDRESS",
                "must differ from MESSAGING_BIND_ADDRESS",
            ));
        }
        let enabled = match get("MESSAGING_E2EE_DEVICES_ENABLED").as_deref() {
            None => runtime.allows_insecure_transport(),
            // Part V J: production E2EE stays disabled until the cryptographic design
            // has external certification; no configuration value enables it.
            Some("true") if runtime == RuntimeEnvironment::Production => {
                return Err(ConfigError::invalid(
                    "MESSAGING_E2EE_DEVICES_ENABLED",
                    "production E2EE is disabled pending external cryptographic certification",
                ))
            }
            Some("true") => true,
            Some("false") => false,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "MESSAGING_E2EE_DEVICES_ENABLED",
                    "must be `true` or `false`",
                ))
            }
        };
        let devices = DeviceSettings {
            enabled,
            challenge_ttl: chrono::Duration::seconds(bounded(
                &get,
                "MESSAGING_DEVICE_CHALLENGE_TTL_SECONDS",
                300,
                60,
                3_600,
            )?),
            challenges_per_hour: bounded(
                &get,
                "MESSAGING_DEVICE_CHALLENGES_PER_HOUR",
                20,
                1,
                1_000,
            )?,
            max_active_devices: bounded(&get, "MESSAGING_MAX_ACTIVE_DEVICES", 10, 1, 100)?,
            max_key_packages_per_device: bounded(
                &get,
                "MESSAGING_MAX_KEY_PACKAGES_PER_DEVICE",
                100,
                1,
                1_000,
            )?,
            key_package_max_lifetime_days: bounded(
                &get,
                "MESSAGING_KEY_PACKAGE_MAX_LIFETIME_DAYS",
                90,
                1,
                90,
            )? as u64,
            directory_lookups_per_minute: bounded(
                &get,
                "MESSAGING_DIRECTORY_LOOKUPS_PER_MINUTE",
                600,
                1,
                100_000,
            )?,
        };
        let auth_cache = AuthCacheSettings {
            ttl: Duration::from_secs(
                bounded(&get, "MESSAGING_AUTH_CACHE_TTL_SECONDS", 10, 0, 60)? as u64,
            ),
            feed_interval: Duration::from_millis(bounded(
                &get,
                "MESSAGING_IDENTITY_FEED_INTERVAL_MS",
                2_000,
                250,
                60_000,
            )? as u64),
        };
        let conversations = ConversationSettings {
            creations_per_minute: bounded(
                &get,
                "MESSAGING_CONVERSATIONS_PER_MINUTE",
                120,
                1,
                100_000,
            )?,
            expiry_interval: Duration::from_millis(bounded(
                &get,
                "MESSAGING_EXPIRY_INTERVAL_MS",
                5_000,
                250,
                600_000,
            )? as u64),
            max_members: bounded(&get, "MESSAGING_MAX_MEMBERS", 10_000, 2, 100_000)?,
            environment_messages_per_minute: bounded(
                &get,
                "MESSAGING_ENVIRONMENT_MESSAGES_PER_MINUTE",
                60_000,
                1,
                10_000_000,
            )?,
            abuse_signal_threshold: bounded(
                &get,
                "MESSAGING_ABUSE_SIGNAL_THRESHOLD",
                10,
                1,
                100_000,
            )?,
            abuse_restriction_seconds: bounded(
                &get,
                "MESSAGING_ABUSE_RESTRICTION_SECONDS",
                900,
                60,
                7 * 86_400,
            )?,
        };
        let workers = WorkerSettings {
            interval: Duration::from_millis(bounded(
                &get,
                "MESSAGING_WORKER_INTERVAL_MS",
                1_000,
                100,
                60_000,
            )? as u64),
            lease: chrono::Duration::seconds(bounded(
                &get,
                "MESSAGING_WORKER_LEASE_SECONDS",
                60,
                5,
                3_600,
            )?),
            outbox_max_attempts: bounded(&get, "MESSAGING_OUTBOX_MAX_ATTEMPTS", 12, 1, 100)? as i32,
        };
        if get("MESSAGING_CLIENT_TOKEN_SIGNING_KEY").is_some() {
            return Err(ConfigError::invalid(
                "MESSAGING_CLIENT_TOKEN_SIGNING_KEY",
                "inline private keys are not accepted; set MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE",
            ));
        }
        if get("MESSAGING_KT_SIGNING_KEY").is_some() {
            return Err(ConfigError::invalid(
                "MESSAGING_KT_SIGNING_KEY",
                "inline private keys are not accepted; mount the key as a secret file and set MESSAGING_KT_SIGNING_KEY_FILE",
            ));
        }
        let log_id = get("MESSAGING_KT_LOG_ID").ok_or(ConfigError::Missing {
            var: "MESSAGING_KT_LOG_ID",
        })?;
        if !crate::transparency::valid_log_id(&log_id) {
            return Err(ConfigError::invalid(
                "MESSAGING_KT_LOG_ID",
                "must match [a-z0-9][a-z0-9.:-]{2,63}",
            ));
        }
        let transparency = TransparencySettings {
            log_id,
            signing_key_file: get("MESSAGING_KT_SIGNING_KEY_FILE")
                .map(std::path::PathBuf::from)
                .ok_or(ConfigError::Missing {
                    var: "MESSAGING_KT_SIGNING_KEY_FILE",
                })?,
        };
        let secs = |var: &'static str,
                    default: i64,
                    min: i64,
                    max: i64|
         -> Result<Duration, ConfigError> {
            Ok(Duration::from_secs(
                bounded(&get, var, default, min, max)? as u64
            ))
        };
        let count =
            |var: &'static str, default: i64, min: i64, max: i64| -> Result<usize, ConfigError> {
                Ok(bounded(&get, var, default, min, max)? as usize)
            };
        let heartbeat = secs("MESSAGING_REALTIME_HEARTBEAT_SECONDS", 20, 5, 120)?;
        let idle = secs("MESSAGING_REALTIME_IDLE_TIMEOUT_SECONDS", 60, 10, 600)?;
        if idle < heartbeat * 2 {
            return Err(ConfigError::invalid(
                "MESSAGING_REALTIME_IDLE_TIMEOUT_SECONDS",
                "must be at least twice the heartbeat interval",
            ));
        }
        let allowed_origins: Vec<String> = get("MESSAGING_REALTIME_ALLOWED_ORIGINS")
            .map(|v| {
                v.split(',')
                    .map(|o| o.trim().to_string())
                    .filter(|o| !o.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        for o in &allowed_origins {
            let ok = (o.starts_with("https://")
                || (runtime.allows_insecure_transport() && o.starts_with("http://")))
                && !o.ends_with('/')
                && o.len() <= 200
                && !o.contains(['*', ' ']);
            if !ok {
                return Err(ConfigError::invalid(
                    "MESSAGING_REALTIME_ALLOWED_ORIGINS",
                    "each origin must be an exact https://host[:port] (no wildcard, no path)",
                ));
            }
        }
        let require_tls = match get("MESSAGING_REALTIME_REQUIRE_TLS").as_deref() {
            None => !runtime.allows_insecure_transport(),
            Some("true") => true,
            Some("false") if runtime.allows_insecure_transport() => false,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "MESSAGING_REALTIME_REQUIRE_TLS",
                    "must be `true` (or `false` in local / test runtimes only)",
                ))
            }
        };
        let realtime = crate::realtime::RealtimeSettings {
            token_ttl: chrono::Duration::seconds(bounded(
                &get,
                "MESSAGING_CLIENT_TOKEN_TTL_SECONDS",
                900,
                60,
                86_400,
            )?),
            ticket_ttl: chrono::Duration::seconds(bounded(
                &get,
                "MESSAGING_REALTIME_TICKET_TTL_SECONDS",
                30,
                5,
                300,
            )?),
            allowed_origins,
            require_tls,
            heartbeat_interval: heartbeat,
            idle_timeout: idle,
            revalidate_interval: secs("MESSAGING_REALTIME_REVALIDATE_SECONDS", 30, 5, 600)?,
            max_frame_bytes: count(
                "MESSAGING_REALTIME_MAX_FRAME_BYTES",
                65_536,
                1_024,
                1_048_576,
            )?,
            send_queue: count("MESSAGING_REALTIME_SEND_QUEUE", 512, 16, 65_536)?,
            send_timeout: Duration::from_secs(10),
            max_subscriptions: count("MESSAGING_REALTIME_MAX_SUBSCRIPTIONS", 200, 1, 10_000)?,
            max_connections: count("MESSAGING_REALTIME_MAX_CONNECTIONS", 50_000, 1, 1_000_000)?,
            per_tenant_connections: count(
                "MESSAGING_REALTIME_PER_TENANT_CONNECTIONS",
                10_000,
                1,
                1_000_000,
            )?,
            per_principal_connections: count(
                "MESSAGING_REALTIME_PER_PRINCIPAL_CONNECTIONS",
                20,
                1,
                1_000,
            )?,
            per_device_connections: count("MESSAGING_REALTIME_PER_DEVICE_CONNECTIONS", 5, 1, 100)?,
            inbound_frames_per_second: bounded(
                &get,
                "MESSAGING_REALTIME_FRAMES_PER_SECOND",
                20,
                1,
                1_000,
            )? as u32,
            max_replay: bounded(&get, "MESSAGING_REALTIME_MAX_REPLAY", 500, 10, 100_000)?,
            typing_ttl: Duration::from_secs(8),
            drain: secs("MESSAGING_REALTIME_DRAIN_SECONDS", 10, 0, 300)?,
            publisher_interval: Duration::from_millis(250),
        };
        let push_provider = match get("MESSAGING_PUSH_PROVIDER").as_deref() {
            None | Some("none") => PushProviderKind::None,
            Some("local-test") if runtime.allows_insecure_transport() => {
                PushProviderKind::LocalTest
            }
            Some(_) => {
                return Err(ConfigError::invalid(
                    "MESSAGING_PUSH_PROVIDER",
                    "must be `none` (or `local-test` in local / test runtimes only)",
                ))
            }
        };
        let push_adapters = {
            let insecure = runtime.allows_insecure_transport();
            let override_url = |var: &'static str, default: &str| -> Result<String, ConfigError> {
                match get(var) {
                    None => Ok(default.to_string()),
                    Some(u) if insecure => Ok(u),
                    Some(_) => Err(ConfigError::invalid(
                        var,
                        "may be set in local / test runtimes only",
                    )),
                }
            };
            let apns_vars = [
                "MESSAGING_PUSH_APNS_KEY_FILE",
                "MESSAGING_PUSH_APNS_KEY_ID",
                "MESSAGING_PUSH_APNS_TEAM_ID",
                "MESSAGING_PUSH_APNS_TOPIC",
            ];
            let apns_set: Vec<Option<String>> = apns_vars.iter().map(|v| get(v)).collect();
            let apns = match apns_set.iter().filter(|v| v.is_some()).count() {
                0 => None,
                4 => {
                    let sandbox = match get("MESSAGING_PUSH_APNS_ENVIRONMENT").as_deref() {
                        None | Some("production") => false,
                        Some("sandbox") => true,
                        Some(_) => {
                            return Err(ConfigError::invalid(
                                "MESSAGING_PUSH_APNS_ENVIRONMENT",
                                "must be `production` or `sandbox`",
                            ))
                        }
                    };
                    let v = |i: usize| apns_set[i].clone().unwrap();
                    Some(ApnsConfig {
                        key_file: std::path::PathBuf::from(v(0)),
                        key_id: v(1),
                        team_id: v(2),
                        topic: v(3),
                        base_url: override_url(
                            "MESSAGING_PUSH_APNS_BASE_URL",
                            crate::realtime::push_adapters::Apns::production_url(sandbox),
                        )?,
                    })
                }
                _ => {
                    return Err(ConfigError::invalid(
                        "MESSAGING_PUSH_APNS_KEY_FILE",
                        "the APNs adapter needs KEY_FILE, KEY_ID, TEAM_ID and TOPIC together",
                    ))
                }
            };
            let fcm = match get("MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE") {
                None => None,
                Some(f) => Some(FcmConfig {
                    service_account_file: std::path::PathBuf::from(f),
                    base_url: override_url(
                        "MESSAGING_PUSH_FCM_BASE_URL",
                        crate::realtime::push_adapters::Fcm::default_base_url(),
                    )?,
                }),
            };
            let web = match (
                get("MESSAGING_PUSH_WEB_VAPID_KEY_FILE"),
                get("MESSAGING_PUSH_WEB_VAPID_SUBJECT"),
            ) {
                (None, None) => None,
                (Some(f), Some(subject)) => Some(WebPushConfig {
                    vapid_key_file: std::path::PathBuf::from(f),
                    subject,
                    allowed_hosts: match get("MESSAGING_PUSH_WEB_ALLOWED_HOSTS") {
                        Some(h) => h
                            .split(',')
                            .map(|x| x.trim().to_string())
                            .filter(|x| !x.is_empty())
                            .collect(),
                        None => crate::realtime::push_adapters::DEFAULT_WEB_PUSH_HOSTS
                            .iter()
                            .map(|h| h.to_string())
                            .collect(),
                    },
                }),
                _ => {
                    return Err(ConfigError::invalid(
                        "MESSAGING_PUSH_WEB_VAPID_KEY_FILE",
                        "the Web Push adapter needs VAPID_KEY_FILE and VAPID_SUBJECT together",
                    ))
                }
            };
            PushAdapterConfig { apns, fcm, web }
        };
        let attachments = AttachmentSettings {
            intent_ttl: chrono::Duration::seconds(bounded(
                &get,
                "MESSAGING_UPLOAD_INTENT_SECONDS",
                900,
                60,
                3_600,
            )?),
            grant_ttl: Duration::from_secs(bounded(
                &get,
                "MESSAGING_DOWNLOAD_GRANT_SECONDS",
                60,
                5,
                900,
            )? as u64),
            storage_budget_bytes: match get("MESSAGING_STORAGE_BUDGET_BYTES") {
                None => None,
                Some(_) => Some(bounded(
                    &get,
                    "MESSAGING_STORAGE_BUDGET_BYTES",
                    0,
                    0,
                    i64::MAX,
                )?),
            },
            managed_scan_max_bytes: bounded(
                &get,
                "MESSAGING_MANAGED_SCAN_MAX_BYTES",
                50 * 1024 * 1024,
                1024,
                1024 * 1024 * 1024,
            )? as usize,
            verify_read_max_bytes: bounded(
                &get,
                "MESSAGING_VERIFY_READ_MAX_BYTES",
                64 * 1024 * 1024,
                1024,
                1024 * 1024 * 1024,
            )? as usize,
            proxy_max_bytes: bounded(
                &get,
                "MESSAGING_PROXY_DOWNLOAD_MAX_BYTES",
                32 * 1024 * 1024,
                1024,
                512 * 1024 * 1024,
            )? as usize,
            unlinked_ttl: chrono::Duration::hours(bounded(
                &get,
                "MESSAGING_UNLINKED_ATTACHMENT_HOURS",
                24,
                1,
                720,
            )?),
            scan_retry_max: 10,
        };
        let attachment_store =
            cf_object_storage::S3Config::from_lookup("MESSAGING_ATTACHMENTS_S3_", &|k: &str| {
                get(k)
            })
            .map_err(|e| ConfigError::invalid("MESSAGING_ATTACHMENTS_S3_*", e))?;
        let clamav_address = get("MESSAGING_CLAMAV_ADDRESS");
        if let Some(a) = &clamav_address {
            if a.parse::<SocketAddr>().is_err() && !a.contains(':') {
                return Err(ConfigError::invalid(
                    "MESSAGING_CLAMAV_ADDRESS",
                    "must be host:port",
                ));
            }
        }
        let log_format = match get("LOG_FORMAT").as_deref() {
            None | Some("pretty") => LogFormat::Pretty,
            Some("json") => LogFormat::Json,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "LOG_FORMAT",
                    "must be `pretty` or `json`",
                ))
            }
        };
        Ok(Self {
            database_url,
            database_max_connections: bounded(
                &get,
                "MESSAGING_DATABASE_MAX_CONNECTIONS",
                10,
                1,
                500,
            )? as u32,
            bind_address,
            metrics_address,
            devices,
            auth_cache,
            conversations,
            workers,
            transparency,
            managed_keyring_file: get("MESSAGING_MANAGED_KEYRING_FILE")
                .map(std::path::PathBuf::from),
            realtime,
            client_token_key_file: get("MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE")
                .map(std::path::PathBuf::from),
            realtime_redis_url: get("MESSAGING_REALTIME_REDIS_URL").map(SecretString::from),
            push_provider,
            push_adapters,
            attachments,
            attachment_store,
            clamav_address,
            log_format,
        })
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    const BASE: &[(&str, &str)] = &[
        ("DATABASE_URL", "postgres://messaging:pw-xyz-123@db/m"),
        ("MESSAGING_KT_LOG_ID", "cfmeet-messaging:test"),
        ("MESSAGING_KT_SIGNING_KEY_FILE", "/run/secrets/kt.pem"),
    ];

    #[test]
    fn requires_its_own_database_and_applies_safe_defaults() {
        let cfg = MessagingConfig::from_lookup(RuntimeEnvironment::Test, lookup(BASE)).unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert!(cfg.devices.enabled);
        assert!(!format!("{cfg:?}").contains("pw-xyz-123"));
        assert!(MessagingConfig::from_lookup(RuntimeEnvironment::Test, lookup(&[])).is_err());
    }

    #[test]
    fn e2ee_device_operations_need_an_explicit_decision_in_staging_and_never_run_in_production() {
        for env in [RuntimeEnvironment::Staging, RuntimeEnvironment::Production] {
            let cfg = MessagingConfig::from_lookup(env, lookup(BASE)).unwrap();
            assert!(!cfg.devices.enabled, "{env}");
        }
        let mut on = BASE.to_vec();
        on.push(("MESSAGING_E2EE_DEVICES_ENABLED", "true"));
        assert!(
            MessagingConfig::from_lookup(RuntimeEnvironment::Staging, lookup(&on))
                .unwrap()
                .devices
                .enabled
        );
        // Part V J: production E2EE stays disabled (pending external certification).
        let err = MessagingConfig::from_lookup(RuntimeEnvironment::Production, lookup(&on))
            .unwrap_err()
            .to_string();
        assert!(err.contains("MESSAGING_E2EE_DEVICES_ENABLED"), "{err}");
    }

    #[test]
    fn bad_values_are_refused_without_echo() {
        for (var, value) in [
            ("MESSAGING_E2EE_DEVICES_ENABLED", "yes-please"),
            ("MESSAGING_MAX_ACTIVE_DEVICES", "-7777"),
            ("MESSAGING_KEY_PACKAGE_MAX_LIFETIME_DAYS", "365"),
            ("MESSAGING_AUTH_CACHE_TTL_SECONDS", "3600"),
            ("MESSAGING_BIND_ADDRESS", "everywhere-port"),
            ("LOG_FORMAT", "xml-format"),
            ("MESSAGING_KT_LOG_ID", "Has Spaces And CAPS"),
            ("MESSAGING_KT_SIGNING_KEY", "inline-secret-key-material"),
        ] {
            let mut pairs = BASE.to_vec();
            pairs.push((var, value));
            let err =
                MessagingConfig::from_lookup(RuntimeEnvironment::Test, lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
        let mut same = BASE.to_vec();
        same.push(("MESSAGING_METRICS_ADDRESS", DEFAULT_BIND_ADDRESS));
        assert!(MessagingConfig::from_lookup(RuntimeEnvironment::Test, lookup(&same)).is_err());
    }

    #[test]
    fn the_tree_head_signing_key_is_a_required_secret_file() {
        for missing in ["MESSAGING_KT_LOG_ID", "MESSAGING_KT_SIGNING_KEY_FILE"] {
            let pairs: Vec<_> = BASE
                .iter()
                .copied()
                .filter(|(k, _)| *k != missing)
                .collect();
            let err =
                MessagingConfig::from_lookup(RuntimeEnvironment::Test, lookup(&pairs)).unwrap_err();
            assert!(err.to_string().contains(missing), "{err}");
        }
        let dir = std::env::temp_dir().join(format!("cf-msg-kt-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kt.pem");
        let settings = TransparencySettings {
            log_id: "cfmeet-messaging:test".into(),
            signing_key_file: path.clone(),
        };
        assert!(settings.load().is_err(), "missing file");
        std::fs::write(&path, "not a key").unwrap();
        let err = settings.load().unwrap_err().to_string();
        assert!(!err.contains("not a key"));
        use ed25519_dalek::pkcs8::{spki::der::pem::LineEnding, EncodePrivateKey as _};
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        std::fs::write(&path, key.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes()).unwrap();
        let signer = settings.load().unwrap();
        assert_eq!(signer.public_key(), key.verifying_key().to_bytes());
        assert_eq!(signer.log_id(), "cfmeet-messaging:test");
        assert!(!format!("{signer:?}").contains("SigningKey"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
