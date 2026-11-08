//! Organizations Service configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | the Organizations Service's OWN PostgreSQL database |
//! | `ORGANIZATIONS_DATABASE_MAX_CONNECTIONS` | no | pool size (1–200, default 10) |
//! | `ORGANIZATIONS_BIND_ADDRESS` | no | default `127.0.0.1:8101` |
//! | `ORGANIZATIONS_WORKER_INTERVAL_MS` | no | maintenance loop interval (1000–600000, default 30000) |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.organizations` |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging` or `production` |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | the receiver-authoritative trust bundle of `cfmeet.organizations` |
//! | `ORGANIZATIONS_PROVISIONING_ENABLED` | no | `true` / `false` (default `false`): run the Section 7 Platform Access provisioning saga |
//! | `ORGANIZATIONS_EVENT_DESTINATIONS` | no | comma-separated consumers that receive organization events (today only `cfmeet.meeting-service`); empty (default): events stay PENDING in the outbox |
//! | `CF_SERVICE_AUTH_KEY_ID` | when provisioning or event delivery | `kid` of the Organizations signing key |
//! | `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | when provisioning or event delivery | PKCS#8 PEM Ed25519 private key FILE |
//! | `CF_PLATFORM_ACCESS_BASE_URL` | when provisioning | Platform Access (`platform-identity.provision-first-party`) |
//! | `CF_MEETING_SERVICE_BASE_URL` | when `cfmeet.meeting-service` receives events | Meeting Service internal base URL (`meeting-organization-events.apply`) |
//! | `CF_SERVICE_TOKEN_TTL_SECONDS` | no | 10–300 (default 120) |
//! | `CF_INTERNAL_HTTP_TIMEOUT_MS` | no | outbound timeout (100–30000, default 5000) |
//!
//! Outbound, Organizations calls Platform Access when provisioning is enabled and each
//! configured event consumer's signed ingress; both sign with its own key file (inline
//! keys are refused regardless). With provisioning off (the default) organizations stay
//! PENDING and the provisioning read reports it truthfully. Error messages never echo a
//! value.

use std::{fmt, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    services, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SharedClock, TokenTtl, TrustPolicy,
};

use crate::{
    outbox::{DisabledDispatcher, EventDispatcher, HttpEventDispatcher},
    provisioning::{HttpPlatformScopes, PlatformScopes},
};
use secrecy::{ExposeSecret, SecretString};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is not set")]
    Missing { var: &'static str },
    /// The message never echoes the offending value (it may be a secret).
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        var,
        reason: reason.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8101";

pub fn organizations_service_id() -> ServiceId {
    ServiceId::parse(services::ORGANIZATIONS).expect("canonical identity")
}

/// One organization-event consumer: its identity and internal base URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventConsumer {
    pub destination: String,
    pub base_url: String,
}

/// The consumers organization events may be delivered to, with the base-URL variable of
/// each (the names the other services already use).
pub const EVENT_CONSUMERS: &[(&str, &str)] =
    &[(services::MEETING_SERVICE, "CF_MEETING_SERVICE_BASE_URL")];

/// Outbound signing shared by provisioning and event delivery.
#[derive(Debug, Clone)]
pub struct SigningSettings {
    pub key_id: KeyId,
    pub private_key_file: PathBuf,
    pub token_ttl: TokenTtl,
    pub http_timeout: Duration,
}

/// Outbound service authentication for the provisioning saga.
#[derive(Debug, Clone)]
pub struct OutboundSettings {
    pub key_id: KeyId,
    pub private_key_file: PathBuf,
    pub platform_access_base_url: String,
    pub token_ttl: TokenTtl,
    pub http_timeout: Duration,
}

pub struct OrganizationsConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub worker_interval: Duration,
    pub log_format: LogFormat,
    pub runtime_environment: RuntimeEnvironment,
    pub trust_bundle_file: PathBuf,
    /// `Some` iff `ORGANIZATIONS_PROVISIONING_ENABLED=true`.
    pub provisioning: Option<OutboundSettings>,
    /// Consumers organization events are delivered to (empty: delivery disabled).
    pub event_consumers: Vec<EventConsumer>,
    /// `Some` iff provisioning or event delivery is enabled.
    pub signing: Option<SigningSettings>,
}

impl fmt::Debug for OrganizationsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OrganizationsConfig")
            .field("database_url", &"[REDACTED]")
            .field("database_max_connections", &self.database_max_connections)
            .field("bind_address", &self.bind_address)
            .field("worker_interval", &self.worker_interval)
            .field("runtime_environment", &self.runtime_environment)
            .field("provisioning_enabled", &self.provisioning.is_some())
            .field(
                "event_destinations",
                &self
                    .event_consumers
                    .iter()
                    .map(|c| c.destination.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

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
            .ok_or_else(|| invalid(var, format!("must be an integer between {min} and {max}"))),
    }
}

impl OrganizationsConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Builds the configuration from an arbitrary key lookup (environment, tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
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
        let bind_address = match get("ORGANIZATIONS_BIND_ADDRESS") {
            None => DEFAULT_BIND_ADDRESS.parse().expect("valid default"),
            Some(v) => v.parse::<SocketAddr>().map_err(|_| {
                invalid(
                    "ORGANIZATIONS_BIND_ADDRESS",
                    "must be a socket address such as 127.0.0.1:8101",
                )
            })?,
        };
        let database_max_connections =
            bounded(&get, "ORGANIZATIONS_DATABASE_MAX_CONNECTIONS", 10, 1, 200)? as u32;
        let worker_interval = Duration::from_millis(bounded(
            &get,
            "ORGANIZATIONS_WORKER_INTERVAL_MS",
            30_000,
            1_000,
            600_000,
        )? as u64);
        let log_format = match get("LOG_FORMAT").as_deref() {
            None | Some("pretty") => LogFormat::Pretty,
            Some("json") => LogFormat::Json,
            Some(_) => return Err(invalid("LOG_FORMAT", "must be `pretty` or `json`")),
        };
        if get("CF_SERVICE_AUTH_PRIVATE_KEY").is_some() {
            return Err(invalid(
                "CF_SERVICE_AUTH_PRIVATE_KEY",
                "inline private keys are not accepted",
            ));
        }
        if let Some(id) = get("CF_SERVICE_ID") {
            if id != services::ORGANIZATIONS {
                return Err(invalid("CF_SERVICE_ID", "must be cfmeet.organizations"));
            }
        }
        let runtime_environment = RuntimeEnvironment::parse(&get("CF_RUNTIME_ENVIRONMENT").ok_or(
            ConfigError::Missing {
                var: "CF_RUNTIME_ENVIRONMENT",
            },
        )?)
        .map_err(|_| {
            invalid(
                "CF_RUNTIME_ENVIRONMENT",
                "must be local, test, staging or production",
            )
        })?;
        let trust_bundle_file = PathBuf::from(get("CF_SERVICE_TRUST_BUNDLE_FILE").ok_or(
            ConfigError::Missing {
                var: "CF_SERVICE_TRUST_BUNDLE_FILE",
            },
        )?);
        let enabled = match get("ORGANIZATIONS_PROVISIONING_ENABLED").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => {
                return Err(invalid(
                    "ORGANIZATIONS_PROVISIONING_ENABLED",
                    "must be `true` or `false`",
                ))
            }
        };
        let required = |var: &'static str| get(var).ok_or(ConfigError::Missing { var });
        let mut event_consumers: Vec<EventConsumer> = Vec::new();
        for name in get("ORGANIZATIONS_EVENT_DESTINATIONS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            let Some((destination, url_var)) = EVENT_CONSUMERS.iter().find(|(d, _)| *d == name)
            else {
                return Err(invalid(
                    "ORGANIZATIONS_EVENT_DESTINATIONS",
                    "names a service that does not consume organization events",
                ));
            };
            if event_consumers
                .iter()
                .any(|c| c.destination == *destination)
            {
                return Err(invalid(
                    "ORGANIZATIONS_EVENT_DESTINATIONS",
                    "names a consumer twice",
                ));
            }
            event_consumers.push(EventConsumer {
                destination: destination.to_string(),
                base_url: required(url_var)?,
            });
        }
        let signing = if enabled || !event_consumers.is_empty() {
            let key_id = KeyId::parse(&required("CF_SERVICE_AUTH_KEY_ID")?)
                .map_err(|_| invalid("CF_SERVICE_AUTH_KEY_ID", "is not a valid key id"))?;
            let token_ttl = match get("CF_SERVICE_TOKEN_TTL_SECONDS") {
                None => TokenTtl::default(),
                Some(v) => v
                    .parse::<u64>()
                    .ok()
                    .and_then(|s| TokenTtl::seconds(s).ok())
                    .ok_or_else(|| {
                        invalid(
                            "CF_SERVICE_TOKEN_TTL_SECONDS",
                            "must be an integer between 10 and 300",
                        )
                    })?,
            };
            Some(SigningSettings {
                key_id,
                private_key_file: PathBuf::from(required("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?),
                token_ttl,
                http_timeout: Duration::from_millis(bounded(
                    &get,
                    "CF_INTERNAL_HTTP_TIMEOUT_MS",
                    5_000,
                    100,
                    30_000,
                )? as u64),
            })
        } else {
            None
        };
        let provisioning = match (&signing, enabled) {
            (Some(s), true) => Some(OutboundSettings {
                key_id: s.key_id.clone(),
                private_key_file: s.private_key_file.clone(),
                platform_access_base_url: required("CF_PLATFORM_ACCESS_BASE_URL")?,
                token_ttl: s.token_ttl,
                http_timeout: s.http_timeout,
            }),
            _ => None,
        };
        Ok(Self {
            database_url,
            database_max_connections,
            bind_address,
            worker_interval,
            log_format,
            runtime_environment,
            trust_bundle_file,
            provisioning,
            event_consumers,
            signing,
        })
    }

    /// The event dispatcher: signed delivery to every configured consumer, or the
    /// disabled dispatcher (rows stay PENDING) when none is configured. Loads the signing
    /// key and validates every target URL; fails closed.
    pub fn event_dispatcher(
        &self,
        clock: SharedClock,
    ) -> Result<Arc<dyn EventDispatcher>, ConfigError> {
        let Some(s) = self
            .signing
            .as_ref()
            .filter(|_| !self.event_consumers.is_empty())
        else {
            return Ok(Arc::new(DisabledDispatcher));
        };
        let key = ServiceSigningKey::from_pkcs8_pem_file(s.key_id.clone(), &s.private_key_file)
            .map_err(|e| invalid("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", e.to_string()))?;
        let signer = Arc::new(ServiceTokenSigner::new(
            organizations_service_id(),
            self.runtime_environment,
            key,
            s.token_ttl,
            clock,
        ));
        let mut targets = InternalTargets::new(self.runtime_environment);
        for consumer in &self.event_consumers {
            let var = EVENT_CONSUMERS
                .iter()
                .find(|(d, _)| *d == consumer.destination)
                .map(|(_, v)| *v)
                .unwrap_or("ORGANIZATIONS_EVENT_DESTINATIONS");
            let id = ServiceId::parse(&consumer.destination)
                .map_err(|_| invalid("ORGANIZATIONS_EVENT_DESTINATIONS", "is not a service id"))?;
            targets
                .insert(id, &consumer.base_url)
                .map_err(|e| invalid(var, e.to_string()))?;
        }
        let client = InternalServiceClient::new(signer, targets, s.http_timeout)
            .map_err(|e| invalid("CF_INTERNAL_HTTP_TIMEOUT_MS", e.to_string()))?;
        Ok(Arc::new(HttpEventDispatcher::new(
            Arc::new(client),
            self.event_consumers
                .iter()
                .map(|c| c.destination.clone())
                .collect(),
        )))
    }

    /// The Platform Access port of the provisioning saga (`None` when provisioning is
    /// off). Loads the signing key and validates the target URL; fails closed.
    pub fn platform_scopes(
        &self,
        clock: SharedClock,
    ) -> Result<Option<Arc<dyn PlatformScopes>>, ConfigError> {
        let Some(s) = &self.provisioning else {
            return Ok(None);
        };
        let key = ServiceSigningKey::from_pkcs8_pem_file(s.key_id.clone(), &s.private_key_file)
            .map_err(|e| invalid("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", e.to_string()))?;
        let signer = Arc::new(ServiceTokenSigner::new(
            organizations_service_id(),
            self.runtime_environment,
            key,
            s.token_ttl,
            clock,
        ));
        let mut targets = InternalTargets::new(self.runtime_environment);
        targets
            .insert(
                ServiceId::parse(services::PLATFORM_ACCESS).expect("canonical identity"),
                &s.platform_access_base_url,
            )
            .map_err(|e| invalid("CF_PLATFORM_ACCESS_BASE_URL", e.to_string()))?;
        let client = InternalServiceClient::new(signer, targets, s.http_timeout)
            .map_err(|e| invalid("CF_INTERNAL_HTTP_TIMEOUT_MS", e.to_string()))?;
        Ok(Some(Arc::new(HttpPlatformScopes::new(Arc::new(client)))))
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }

    /// Loads the trust bundle and builds the receiver-authoritative verifier; fails closed
    /// on anything malformed (wrong receiver, grants the registry forbids, …).
    pub fn verifier(&self, clock: SharedClock) -> Result<Arc<ServiceTokenVerifier>, ConfigError> {
        let policy = TrustPolicy::from_file(
            &self.trust_bundle_file,
            &organizations_service_id(),
            self.runtime_environment,
        )
        .map_err(|e| invalid("CF_SERVICE_TRUST_BUNDLE_FILE", e.to_string()))?;
        Ok(Arc::new(ServiceTokenVerifier::new(policy, clock)))
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
        (
            "DATABASE_URL",
            "postgres://org:pw-secret-9@db/organizations",
        ),
        ("CF_RUNTIME_ENVIRONMENT", "staging"),
        ("CF_SERVICE_TRUST_BUNDLE_FILE", "/etc/cfmeet/trust.json"),
    ];

    #[test]
    fn requires_its_own_database_and_trust_bundle() {
        let cfg = OrganizationsConfig::from_lookup(lookup(BASE)).unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert!(!format!("{cfg:?}").contains("pw-secret-9"));
        for missing in BASE.iter().map(|(k, _)| *k) {
            let pairs: Vec<_> = BASE
                .iter()
                .filter(|(k, _)| *k != missing)
                .copied()
                .collect();
            assert!(
                OrganizationsConfig::from_lookup(lookup(&pairs)).is_err(),
                "{missing}"
            );
        }
    }

    #[test]
    fn provisioning_is_off_by_default_and_needs_its_outbound_settings_when_on() {
        let cfg = OrganizationsConfig::from_lookup(lookup(BASE)).unwrap();
        assert!(cfg.provisioning.is_none());
        let mut on: Vec<_> = BASE.to_vec();
        on.push(("ORGANIZATIONS_PROVISIONING_ENABLED", "true"));
        assert!(OrganizationsConfig::from_lookup(lookup(&on)).is_err());
        on.extend([
            ("CF_SERVICE_AUTH_KEY_ID", "organizations-staging-1"),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                "/run/secrets/organizations.pem",
            ),
            (
                "CF_PLATFORM_ACCESS_BASE_URL",
                "https://platform-access.internal:8443",
            ),
        ]);
        let cfg = OrganizationsConfig::from_lookup(lookup(&on)).unwrap();
        let s = cfg.provisioning.as_ref().unwrap();
        assert_eq!(s.http_timeout, Duration::from_millis(5_000));
        // A missing key file fails closed when the port is built.
        assert!(cfg
            .platform_scopes(Arc::new(cf_service_auth::SystemClock))
            .is_err());
        for (var, value) in [
            ("ORGANIZATIONS_PROVISIONING_ENABLED", "yes-please"),
            ("CF_SERVICE_TOKEN_TTL_SECONDS", "9999"),
            ("CF_INTERNAL_HTTP_TIMEOUT_MS", "7"),
        ] {
            let mut pairs: Vec<_> = on.iter().filter(|(k, _)| *k != var).copied().collect();
            pairs.push((var, value));
            let err = OrganizationsConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
    }

    #[test]
    fn event_delivery_is_off_by_default_and_needs_a_known_consumer_url_and_key() {
        let cfg = OrganizationsConfig::from_lookup(lookup(BASE)).unwrap();
        assert!(cfg.event_consumers.is_empty() && cfg.signing.is_none());
        assert!(!cfg
            .event_dispatcher(Arc::new(cf_service_auth::SystemClock))
            .unwrap()
            .enabled());
        let mut on: Vec<_> = BASE.to_vec();
        on.push(("ORGANIZATIONS_EVENT_DESTINATIONS", "cfmeet.meeting-service"));
        // The consumer URL and the signing key are required.
        assert!(OrganizationsConfig::from_lookup(lookup(&on)).is_err());
        on.push((
            "CF_MEETING_SERVICE_BASE_URL",
            "https://meeting.internal:8443",
        ));
        assert!(OrganizationsConfig::from_lookup(lookup(&on)).is_err());
        on.extend([
            ("CF_SERVICE_AUTH_KEY_ID", "organizations-staging-1"),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                "/run/secrets/organizations.pem",
            ),
        ]);
        let cfg = OrganizationsConfig::from_lookup(lookup(&on)).unwrap();
        assert_eq!(cfg.event_consumers.len(), 1);
        // Event delivery alone does not enable provisioning.
        assert!(cfg.provisioning.is_none());
        // A missing key file fails closed when the dispatcher is built.
        assert!(cfg
            .event_dispatcher(Arc::new(cf_service_auth::SystemClock))
            .is_err());
        for value in [
            "cfmeet.calling-unknown-consumer",
            "cfmeet.meeting-service,cfmeet.meeting-service",
        ] {
            let mut pairs: Vec<_> = on
                .iter()
                .filter(|(k, _)| *k != "ORGANIZATIONS_EVENT_DESTINATIONS")
                .copied()
                .collect();
            pairs.push(("ORGANIZATIONS_EVENT_DESTINATIONS", value));
            let err = OrganizationsConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains("unknown-consumer"), "{value}");
        }
    }

    #[test]
    fn invalid_values_are_refused_without_echo() {
        for (var, value) in [
            ("ORGANIZATIONS_BIND_ADDRESS", "not-an-address-xyz"),
            ("ORGANIZATIONS_DATABASE_MAX_CONNECTIONS", "999"),
            ("ORGANIZATIONS_WORKER_INTERVAL_MS", "5"),
            ("LOG_FORMAT", "xml-format"),
            ("CF_SERVICE_ID", "cfmeet.calling-x"),
            ("CF_SERVICE_AUTH_PRIVATE_KEY", "inline-key-material"),
            ("CF_RUNTIME_ENVIRONMENT", "prod-ish"),
        ] {
            let mut pairs: Vec<_> = BASE.iter().filter(|(k, _)| *k != var).copied().collect();
            pairs.push((var, value));
            let err = OrganizationsConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
    }
}
