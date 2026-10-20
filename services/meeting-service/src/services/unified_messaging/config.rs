//! Configuration of the Unified Messaging integration (Part V H).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `FEATURE_UNIFIED_MESSAGING` | no (default off) | enables the integration (identity bridge, sync worker, cutover) |
//! | `CF_RUNTIME_ENVIRONMENT` | when enabled | `local`, `test`, `staging` or `production` |
//! | `CF_SERVICE_AUTH_KEY_ID` | when enabled | `kid` of Meeting Service's Ed25519 signing key |
//! | `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | when enabled | PKCS#8 PEM private key FILE (never inline) |
//! | `CF_SERVICE_TOKEN_TTL_SECONDS` | no | 10–300 (default 120) |
//! | `CF_INTERNAL_HTTP_TIMEOUT_MS` | no | 100–30000 (default 5000) |
//! | `CF_PLATFORM_ACCESS_BASE_URL` | when enabled | Platform Access (principal ensure) |
//! | `CF_MESSAGING_BASE_URL` | when enabled | Messaging |
//! | `CFMEET_COMMUNICATION_TENANT_ID` / `_APPLICATION_ID` / `_ENVIRONMENT_ID` / `_SERVICE_ACCOUNT_ID` | when enabled | CF Meet's own FIRST_PARTY application environment in Platform Access |
//! | `CFMEET_COMMUNICATION_ENVIRONMENT_KIND` | when enabled | `SANDBOX`, `TEST` or `PRODUCTION` |
//! | `UNIFIED_MESSAGING_SYNC_INTERVAL_MS` | no | worker poll interval (200–60000, default 2000) |
//!
//! The scope ids are identifiers, not credentials: Messaging and Platform Access re-validate
//! them on every call (the signed delegated context). The signing key is a server secret
//! of Meeting Service only and never reaches a browser or mobile client.

use std::{path::PathBuf, time::Duration};

use cf_service_auth::{ApplicationEnvironmentKind, KeyId, RuntimeEnvironment};
use uuid::Uuid;

use crate::config::env_bool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommunicationScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: ApplicationEnvironmentKind,
    pub service_account_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct UnifiedMessagingConfig {
    pub runtime: RuntimeEnvironment,
    pub key_id: KeyId,
    pub key_file: PathBuf,
    pub token_ttl_seconds: u64,
    pub timeout: Duration,
    pub platform_access_url: String,
    pub messaging_url: String,
    pub scope: CommunicationScope,
    pub sync_interval: Duration,
}

fn var(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(format!(
            "{name} is required when FEATURE_UNIFIED_MESSAGING is on"
        )),
    }
}

fn uuid_var(name: &str) -> Result<Uuid, String> {
    Uuid::parse_str(&var(name)?).map_err(|_| format!("{name} must be a UUID"))
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

impl UnifiedMessagingConfig {
    /// `Ok(None)`: the integration is off (legacy chat only). `Err`: it is on but
    /// misconfigured — startup must fail closed rather than run half-integrated.
    pub fn from_env() -> Result<Option<Self>, String> {
        if !env_bool("FEATURE_UNIFIED_MESSAGING", false) {
            return Ok(None);
        }
        let runtime = RuntimeEnvironment::parse(&var("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT must be local, test, staging or production")?;
        let key_id = KeyId::parse(&var("CF_SERVICE_AUTH_KEY_ID")?)
            .map_err(|_| "CF_SERVICE_AUTH_KEY_ID is not a valid key id")?;
        let kind = match var("CFMEET_COMMUNICATION_ENVIRONMENT_KIND")?.as_str() {
            "SANDBOX" => ApplicationEnvironmentKind::Sandbox,
            "TEST" => ApplicationEnvironmentKind::Test,
            "PRODUCTION" => ApplicationEnvironmentKind::Production,
            _ => {
                return Err(
                    "CFMEET_COMMUNICATION_ENVIRONMENT_KIND must be SANDBOX, TEST or PRODUCTION"
                        .into(),
                )
            }
        };
        Ok(Some(Self {
            runtime,
            key_id,
            key_file: PathBuf::from(var("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?),
            token_ttl_seconds: bounded("CF_SERVICE_TOKEN_TTL_SECONDS", 120, 10, 300)?,
            timeout: Duration::from_millis(bounded(
                "CF_INTERNAL_HTTP_TIMEOUT_MS",
                5000,
                100,
                30_000,
            )?),
            platform_access_url: var("CF_PLATFORM_ACCESS_BASE_URL")?,
            messaging_url: var("CF_MESSAGING_BASE_URL")?,
            scope: CommunicationScope {
                tenant_id: uuid_var("CFMEET_COMMUNICATION_TENANT_ID")?,
                application_id: uuid_var("CFMEET_COMMUNICATION_APPLICATION_ID")?,
                environment_id: uuid_var("CFMEET_COMMUNICATION_ENVIRONMENT_ID")?,
                kind,
                service_account_id: uuid_var("CFMEET_COMMUNICATION_SERVICE_ACCOUNT_ID")?,
            },
            sync_interval: Duration::from_millis(bounded(
                "UNIFIED_MESSAGING_SYNC_INTERVAL_MS",
                2000,
                200,
                60_000,
            )?),
        }))
    }
}
