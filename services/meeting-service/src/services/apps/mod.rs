//! CF Meet Apps (ADR 0064 / 0065, integrate contract §3): the Meeting Service as the
//! account authority of the OAuth front door.
//!
//! * [`service`]: the consent relay (`/api/v2/oauth/authorize/*`), Connected apps
//!   (`/api/v2/me/apps`), organization Apps (`/api/v2/organizations/{id}/apps`), the
//!   userinfo profile and in-meeting apps (`/api/v2/conferences/{id}/apps`, app-context
//!   tokens, `/api/v2/app-context-keys`).
//! * [`delegated`]: the `DelegatedUser` actor of OAuth access tokens on the machine
//!   operations (admitted by [`crate::middleware::platform_auth`]).
//! * [`oauth`]: Platform Access (`platform-oauth.authorize`).
//! * [`organizations`]: the Organizations install policy (`organization-apps.evaluate`)
//!   and organization administrators.
//! * [`context`]: the Ed25519 app-context key.
//! * [`userinfo`]: `GET /internal/v1/oauth/userinfo` (`meeting-oauth-userinfo.read`).
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `MEETING_OAUTH_AUTHORIZE_ENABLED` | `false` | the consent relay, Connected apps, organization Apps and the internal userinfo route; needs the Platform Access client (`CF_PLATFORM_ACCESS_BASE_URL`, `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`) and `CF_SERVICE_TRUST_BUNDLE_FILE` (userinfo receiver) |
//! | `MEETING_DELEGATED_APPS_ENABLED` | `false` | OAuth access tokens forwarded by the API Gateway act as `DelegatedUser` on the machine operations (needs `MEETING_PLATFORM_ACCESS_ENABLED`) |
//! | `MEETING_IN_MEETING_APPS_ENABLED` | `false` | in-meeting apps and app-context tokens; needs the Platform Access client and `MEETING_APP_CONTEXT_SIGNING_KEY_FILE` (Ed25519 PKCS#8 PEM) |
//!
//! Off (the default): every route answers `404 FEATURE_DISABLED`, OAuth credentials are
//! refused on the machine operations with the normalized 401 and nothing is called.
//! An enabled but misconfigured feature stops startup (fail closed).

pub mod context;
pub mod delegated;
pub mod oauth;
pub mod organizations;
pub mod service;
pub mod userinfo;

use std::{path::PathBuf, sync::Arc, time::Duration};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, SystemClock,
    TokenTtl,
};

use crate::config::env_bool;

/// The Apps integration of one process (cheap to clone behind an `Arc`).
#[derive(Clone, Default)]
pub struct Apps {
    pub authorize_enabled: bool,
    pub delegated_enabled: bool,
    pub in_meeting_enabled: bool,
    pub oauth: Option<oauth::SharedOauthAuthority>,
    pub organizations: Option<organizations::SharedAppOrganizations>,
    pub context: Option<Arc<context::AppContextSigner>>,
    /// The account's personal FIRST_PARTY scope context signed on USER authorization
    /// requests; `None`: the Communications bridge's scope (`CFMEET_COMMUNICATION_*`, the
    /// same context as `/internal/v1/principals/ensure`).
    pub personal_scope: Option<cf_service_auth::DelegatedPlatformContext>,
}

impl std::fmt::Debug for Apps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Apps")
            .field("authorize_enabled", &self.authorize_enabled)
            .field("delegated_enabled", &self.delegated_enabled)
            .field("in_meeting_enabled", &self.in_meeting_enabled)
            .finish_non_exhaustive()
    }
}

impl Apps {
    /// Everything off (the default).
    pub fn disabled() -> Self {
        Self::default()
    }
}

/// Configuration read from the environment.
#[derive(Debug, Clone)]
pub struct AppsConfig {
    pub authorize_enabled: bool,
    pub delegated_enabled: bool,
    pub in_meeting_enabled: bool,
    pub runtime: Option<RuntimeEnvironment>,
    pub key_id: Option<KeyId>,
    pub key_file: Option<PathBuf>,
    pub platform_access_url: Option<String>,
    pub trust_bundle_file: Option<PathBuf>,
    pub context_key_file: Option<PathBuf>,
    pub timeout: Duration,
    pub token_ttl_seconds: u64,
}

fn var(name: &str, why: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(format!("{name} is required when {why}")),
    }
}

fn bounded(name: &str, default: u64, min: u64, max: u64) -> Result<u64, String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| format!("{name} must be an integer between {min} and {max}")),
        _ => Ok(default),
    }
}

impl AppsConfig {
    /// `Ok(None)`: every feature off.
    pub fn from_env() -> Result<Option<Self>, String> {
        let authorize_enabled = env_bool("MEETING_OAUTH_AUTHORIZE_ENABLED", false);
        let delegated_enabled = env_bool("MEETING_DELEGATED_APPS_ENABLED", false);
        let in_meeting_enabled = env_bool("MEETING_IN_MEETING_APPS_ENABLED", false);
        if !authorize_enabled && !delegated_enabled && !in_meeting_enabled {
            return Ok(None);
        }
        if delegated_enabled && !env_bool("MEETING_PLATFORM_ACCESS_ENABLED", false) {
            return Err(
                "MEETING_DELEGATED_APPS_ENABLED requires MEETING_PLATFORM_ACCESS_ENABLED".into(),
            );
        }
        let needs_client = authorize_enabled || in_meeting_enabled;
        let why = "MEETING_OAUTH_AUTHORIZE_ENABLED or MEETING_IN_MEETING_APPS_ENABLED is on";
        let (runtime, key_id, key_file, platform_access_url) = if needs_client {
            (
                Some(
                    RuntimeEnvironment::parse(&var("CF_RUNTIME_ENVIRONMENT", why)?).map_err(
                        |_| "CF_RUNTIME_ENVIRONMENT must be local, test, staging or production",
                    )?,
                ),
                Some(
                    KeyId::parse(&var("CF_SERVICE_AUTH_KEY_ID", why)?)
                        .map_err(|_| "CF_SERVICE_AUTH_KEY_ID is not a valid key id")?,
                ),
                Some(PathBuf::from(var("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", why)?)),
                Some(var("CF_PLATFORM_ACCESS_BASE_URL", why)?),
            )
        } else {
            (None, None, None, None)
        };
        let trust_bundle_file = if authorize_enabled {
            Some(PathBuf::from(var(
                "CF_SERVICE_TRUST_BUNDLE_FILE",
                "MEETING_OAUTH_AUTHORIZE_ENABLED is on",
            )?))
        } else {
            None
        };
        let context_key_file = if in_meeting_enabled {
            Some(PathBuf::from(var(
                "MEETING_APP_CONTEXT_SIGNING_KEY_FILE",
                "MEETING_IN_MEETING_APPS_ENABLED is on",
            )?))
        } else {
            None
        };
        Ok(Some(Self {
            authorize_enabled,
            delegated_enabled,
            in_meeting_enabled,
            runtime,
            key_id,
            key_file,
            platform_access_url,
            trust_bundle_file,
            context_key_file,
            timeout: Duration::from_millis(bounded(
                "CF_INTERNAL_HTTP_TIMEOUT_MS",
                5000,
                100,
                30_000,
            )?),
            token_ttl_seconds: bounded("CF_SERVICE_TOKEN_TTL_SECONDS", 120, 10, 300)?,
        }))
    }

    /// The signed Platform Access client (`None` when no feature needs it).
    pub fn client(&self) -> Result<Option<Arc<InternalServiceClient>>, String> {
        let (Some(runtime), Some(key_id), Some(key_file), Some(url)) = (
            self.runtime,
            self.key_id.clone(),
            self.key_file.as_ref(),
            self.platform_access_url.as_ref(),
        ) else {
            return Ok(None);
        };
        let key = ServiceSigningKey::from_pkcs8_pem_file(key_id, key_file)
            .map_err(|_| "CF_SERVICE_AUTH_PRIVATE_KEY_FILE is not a readable PKCS#8 Ed25519 key")?;
        let ttl = TokenTtl::seconds(self.token_ttl_seconds)
            .map_err(|_| "CF_SERVICE_TOKEN_TTL_SECONDS is out of range")?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            runtime,
            key,
            ttl,
            Arc::new(SystemClock),
        ));
        let mut targets = InternalTargets::new(runtime);
        targets
            .insert(ServiceId::platform_access(), url)
            .map_err(|e| format!("CF_PLATFORM_ACCESS_BASE_URL: {e}"))?;
        Ok(Some(Arc::new(
            InternalServiceClient::new(signer, targets, self.timeout)
                .map_err(|e| format!("internal client: {e}"))?,
        )))
    }

    /// The receiver verifier of the internal userinfo route (`MEETING_OAUTH_AUTHORIZE_ENABLED`).
    pub fn userinfo_verifier(
        &self,
    ) -> Result<Option<Arc<cf_service_auth::ServiceTokenVerifier>>, String> {
        let (Some(bundle), Some(runtime)) = (self.trust_bundle_file.as_ref(), self.runtime) else {
            return Ok(None);
        };
        let policy =
            cf_service_auth::TrustPolicy::from_file(bundle, &ServiceId::meeting_service(), runtime)
                .map_err(|e| format!("CF_SERVICE_TRUST_BUNDLE_FILE: {e}"))?;
        Ok(Some(Arc::new(cf_service_auth::ServiceTokenVerifier::new(
            policy,
            Arc::new(SystemClock),
        ))))
    }

    /// Builds the integration. `organizations` is the authoritative Organizations relay
    /// (organization installs and organization Apps need it).
    pub fn build(
        &self,
        organizations: Option<
            crate::services::organizations_authority::SharedOrganizationsAuthority,
        >,
    ) -> Result<Apps, String> {
        let oauth = self
            .client()?
            .map(|c| Arc::new(oauth::PlatformAccessOauth::new(c)) as oauth::SharedOauthAuthority);
        let context = self
            .context_key_file
            .as_ref()
            .map(|p| context::AppContextSigner::from_pem_file(p).map(Arc::new))
            .transpose()?;
        Ok(Apps {
            authorize_enabled: self.authorize_enabled,
            delegated_enabled: self.delegated_enabled,
            in_meeting_enabled: self.in_meeting_enabled,
            oauth,
            organizations: organizations.filter(|a| a.is_authoritative()).map(|a| {
                Arc::new(organizations::OrganizationsAppPolicy::new(a))
                    as organizations::SharedAppOrganizations
            }),
            context,
            personal_scope: None,
        })
    }
}
