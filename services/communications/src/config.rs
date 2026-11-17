//! Communications Hub runtime configuration (environment variables).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `CF_RUNTIME_ENVIRONMENT` | yes | runtime environment of the service-token signer and trust bundle |
//! | `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | yes | the Hub's signing identity (`cfmeet.communications`) for every outbound internal call |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | the Hub's inbound trust bundle |
//! | `CF_PLATFORM_ACCESS_BASE_URL`, `CF_MEETING_SERVICE_BASE_URL` | yes | Platform Access and the Account (Meeting Service) |
//! | `CF_CALLING_BASE_URL`, `CF_MESSAGING_BASE_URL` | no | Calling and Messaging owners; unset: their features answer `DEPENDENCY_NOT_CONFIGURED` |
//! | `CF_NOTIFICATIONS_BASE_URL` | no | the Notification Service's I1 (`notifications-event.publish`, audience `cfmeet.notifications`; roadmap N7). Set: contact-request notifications are written to `communication_notification_publications` and relayed ([`crate::notification_publications`]). Unset: nothing is written or sent (the Hub behaves exactly as before) |
//! | `COMMUNICATIONS_INVITE_DIGEST_KEY_FILE` | yes | 32-byte hex invitation digest key |
//! | `CFMEET_PUBLIC_APP_URL` | yes | public application URL (invitations) |
//! | `DATABASE_URL` | yes | the Hub's own PostgreSQL |
//! | `COMMUNICATIONS_BIND_ADDRESS` | no | listener (default `0.0.0.0:8080`) |
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use secrecy::{ExposeSecret, SecretString};
use std::{path::PathBuf, sync::Arc, time::Duration};

pub struct Config {
    pub bind: std::net::SocketAddr,
    pub database_url: SecretString,
    pub public_app_url: String,
    pub invite_key: SecretString,
    pub verifier: Arc<ServiceTokenVerifier>,
    pub client: Arc<InternalServiceClient>,
    pub calling_configured: bool,
    pub messaging_configured: bool,
    /// `CF_NOTIFICATIONS_BASE_URL` is set: contact-request notifications are published.
    pub notifications_configured: bool,
}
fn required(k: &str) -> Result<String, String> {
    std::env::var(k)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("{k} is required"))
}
impl Config {
    pub fn from_env() -> Result<Self, String> {
        let runtime = RuntimeEnvironment::parse(&required("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| "invalid runtime")?;
        let id = ServiceId::communications();
        let key = ServiceSigningKey::from_pkcs8_pem_file(
            KeyId::parse(&required("CF_SERVICE_AUTH_KEY_ID")?).map_err(|_| "invalid key id")?,
            &PathBuf::from(required("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?),
        )
        .map_err(|_| "invalid signing-key file")?;
        let signer = Arc::new(ServiceTokenSigner::new(
            id.clone(),
            runtime,
            key,
            TokenTtl::seconds(60).map_err(|_| "invalid TTL")?,
            Arc::new(SystemClock),
        ));
        let policy = TrustPolicy::from_file(
            &PathBuf::from(required("CF_SERVICE_TRUST_BUNDLE_FILE")?),
            &id,
            runtime,
        )
        .map_err(|_| "invalid Communications trust bundle")?;
        let verifier = Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)));
        let mut targets = InternalTargets::new(runtime);
        for (key, service, mandatory) in [
            (
                "CF_PLATFORM_ACCESS_BASE_URL",
                ServiceId::platform_access(),
                true,
            ),
            (
                "CF_MEETING_SERVICE_BASE_URL",
                ServiceId::meeting_service(),
                true,
            ),
            ("CF_CALLING_BASE_URL", ServiceId::calling(), false),
            ("CF_MESSAGING_BASE_URL", ServiceId::messaging(), false),
            // Roadmap N7: I1 of the Notification Service; unset, nothing is published.
            (
                "CF_NOTIFICATIONS_BASE_URL",
                ServiceId::notifications(),
                false,
            ),
        ] {
            match std::env::var(key).ok().filter(|v| !v.is_empty()) {
                Some(url) => targets
                    .insert(service, &url)
                    .map_err(|_| format!("invalid {key}"))?,
                None if mandatory => return Err(format!("{key} is required")),
                _ => {}
            }
        }
        let calling_configured = targets.contains(&ServiceId::calling());
        let messaging_configured = targets.contains(&ServiceId::messaging());
        let notifications_configured = targets.contains(&ServiceId::notifications());
        let client = Arc::new(
            InternalServiceClient::new(signer, targets, Duration::from_secs(5))
                .map_err(|_| "invalid internal client")?,
        );
        let key = std::fs::read_to_string(required("COMMUNICATIONS_INVITE_DIGEST_KEY_FILE")?)
            .map_err(|_| "invitation digest key file unreadable")?;
        let key = key.trim().to_owned();
        if hex::decode(&key).map(|v| v.len()).unwrap_or(0) != 32 {
            return Err("invitation digest key must contain 32 random bytes encoded as hex".into());
        }
        let public_app_url = required("CFMEET_PUBLIC_APP_URL")?
            .trim_end_matches('/')
            .to_owned();
        if runtime == RuntimeEnvironment::Production && public_app_url != "https://cfmeet.com" {
            return Err(
                "production invitations require the verified https://cfmeet.com application domain"
                    .into(),
            );
        }
        if !(public_app_url.starts_with("https://")
            || runtime != RuntimeEnvironment::Production
                && public_app_url.starts_with("http://127.0.0.1"))
            || public_app_url.contains(['?', '#', '@'])
        {
            return Err("invalid public application URL".into());
        }
        let database_url = SecretString::from(required("DATABASE_URL")?);
        // Never log the URL: it contains this service's isolated database credentials.
        if !database_url.expose_secret().starts_with("postgres") {
            return Err("PostgreSQL is required".into());
        }
        Ok(Self {
            bind: std::env::var("COMMUNICATIONS_BIND_ADDRESS")
                .unwrap_or_else(|_| "0.0.0.0:8080".into())
                .parse()
                .map_err(|_| "invalid bind address")?,
            database_url,
            public_app_url,
            invite_key: SecretString::from(key),
            verifier,
            client,
            calling_configured,
            messaging_configured,
            notifications_configured,
        })
    }
}
