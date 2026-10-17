//! Routing grant signing (Workstream K), read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `NUMBERING_GRANT_KEY_ID` | with the file | `kid` of Numbering's DEDICATED grant key |
//! | `NUMBERING_GRANT_PRIVATE_KEY_FILE` | with the id | PKCS#8 PEM Ed25519 private key FILE |
//! | `NUMBERING_GRANT_TTL_SECONDS` | no | grant lifetime, 10–300 (default 60) |
//!
//! Without a grant key no grant is issued or verified (the routing grant endpoints answer
//! 503): fail closed. The grant key is never the service-auth key (same `kid` or the same
//! public key refuses to start), never inline, never logged.

use std::path::PathBuf;

use cf_service_auth::{
    number_grant::{NumberGrantSigner, DEFAULT_NUMBER_GRANT_TTL_SECONDS},
    KeyId, RuntimeEnvironment, ServiceSigningKey, SharedClock,
};

use super::{
    service_auth::{numbering_service_id, ServiceAuthSettings},
    ConfigError,
};

#[derive(Debug, Clone)]
pub struct GrantSettings {
    pub key_id: KeyId,
    pub private_key_file: PathBuf,
    pub ttl_seconds: u64,
}

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::invalid(var, reason.into())
}

impl GrantSettings {
    /// `None`: grants are not configured (issuance and verification refused).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, ConfigError> {
        let value = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        if value("NUMBERING_GRANT_PRIVATE_KEY").is_some() {
            return Err(invalid(
                "NUMBERING_GRANT_PRIVATE_KEY",
                "inline private keys are not accepted; mount the key as a secret file and set NUMBERING_GRANT_PRIVATE_KEY_FILE",
            ));
        }
        let ttl_seconds = match value("NUMBERING_GRANT_TTL_SECONDS") {
            None => DEFAULT_NUMBER_GRANT_TTL_SECONDS,
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| (10..=300).contains(n))
                .ok_or_else(|| {
                    invalid(
                        "NUMBERING_GRANT_TTL_SECONDS",
                        "must be an integer between 10 and 300",
                    )
                })?,
        };
        match (
            value("NUMBERING_GRANT_KEY_ID"),
            value("NUMBERING_GRANT_PRIVATE_KEY_FILE"),
        ) {
            (None, None) => Ok(None),
            (Some(kid), Some(file)) => Ok(Some(Self {
                key_id: KeyId::parse(&kid)
                    .map_err(|e| invalid("NUMBERING_GRANT_KEY_ID", e.to_string()))?,
                private_key_file: PathBuf::from(file),
                ttl_seconds,
            })),
            (None, Some(_)) => Err(ConfigError::Missing {
                var: "NUMBERING_GRANT_KEY_ID",
            }),
            (Some(_), None) => Err(ConfigError::Missing {
                var: "NUMBERING_GRANT_PRIVATE_KEY_FILE",
            }),
        }
    }

    /// Loads the grant key and refuses one shared with service authentication.
    pub fn load(
        &self,
        service_auth: &ServiceAuthSettings,
        clock: SharedClock,
    ) -> Result<NumberGrantSigner, ConfigError> {
        let key =
            ServiceSigningKey::from_pkcs8_pem_file(self.key_id.clone(), &self.private_key_file)
                .map_err(|e| invalid("NUMBERING_GRANT_PRIVATE_KEY_FILE", e.to_string()))?;
        let service_key = ServiceSigningKey::from_pkcs8_pem_file(
            service_auth.key_id.clone(),
            &service_auth.private_key_file,
        )
        .map_err(|e| invalid("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", e.to_string()))?;
        if self.key_id == service_auth.key_id || key.public_jwk().x == service_key.public_jwk().x {
            return Err(invalid(
                "NUMBERING_GRANT_PRIVATE_KEY_FILE",
                "the grant key must be dedicated (never the service-auth key)",
            ));
        }
        signer(
            key,
            service_auth.runtime_environment,
            self.ttl_seconds,
            clock,
        )
    }
}

/// A grant signer for `key` (the dedicated grant key).
pub fn signer(
    key: ServiceSigningKey,
    runtime_environment: RuntimeEnvironment,
    ttl_seconds: u64,
    clock: SharedClock,
) -> Result<NumberGrantSigner, ConfigError> {
    NumberGrantSigner::new(
        numbering_service_id(),
        runtime_environment,
        key,
        ttl_seconds,
        clock,
    )
    .map_err(|e| invalid("NUMBERING_GRANT_TTL_SECONDS", e.to_string()))
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

    #[test]
    fn grants_are_optional_complete_or_refused() {
        assert!(GrantSettings::from_lookup(lookup(&[])).unwrap().is_none());
        let full = GrantSettings::from_lookup(lookup(&[
            ("NUMBERING_GRANT_KEY_ID", "numbering-grant-1"),
            ("NUMBERING_GRANT_PRIVATE_KEY_FILE", "/run/secrets/grant.pem"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(full.ttl_seconds, 60);
        for bad in [
            vec![("NUMBERING_GRANT_KEY_ID", "numbering-grant-1")],
            vec![("NUMBERING_GRANT_PRIVATE_KEY_FILE", "/run/secrets/grant.pem")],
            vec![("NUMBERING_GRANT_PRIVATE_KEY", "-----BEGIN PRIVATE KEY-----")],
            vec![
                ("NUMBERING_GRANT_KEY_ID", "numbering-grant-1"),
                ("NUMBERING_GRANT_PRIVATE_KEY_FILE", "/run/secrets/grant.pem"),
                ("NUMBERING_GRANT_TTL_SECONDS", "3600"),
            ],
        ] {
            assert!(GrantSettings::from_lookup(lookup(&bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_service_auth_key_is_never_a_grant_key() {
        let dir = std::env::temp_dir().join(format!("nb-grant-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let service = ServiceSigningKey::generate(KeyId::parse("numbering-1").unwrap());
        let grant = ServiceSigningKey::generate(KeyId::parse("numbering-grant-1").unwrap());
        std::fs::write(dir.join("service.pem"), service.to_pkcs8_pem().as_bytes()).unwrap();
        std::fs::write(dir.join("grant.pem"), grant.to_pkcs8_pem().as_bytes()).unwrap();
        let auth = ServiceAuthSettings::from_lookup(&lookup(&[
            ("CF_RUNTIME_ENVIRONMENT", "staging"),
            ("CF_SERVICE_AUTH_KEY_ID", "numbering-1"),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                dir.join("service.pem").to_str().unwrap(),
            ),
            ("CF_SERVICE_TRUST_BUNDLE_FILE", "/etc/trust.json"),
        ]))
        .unwrap();
        let clock: SharedClock = std::sync::Arc::new(cf_service_auth::SystemClock);
        let ok = GrantSettings {
            key_id: KeyId::parse("numbering-grant-1").unwrap(),
            private_key_file: dir.join("grant.pem"),
            ttl_seconds: 60,
        };
        assert!(ok.load(&auth, clock.clone()).is_ok());
        for (kid, file) in [
            ("numbering-grant-1", "service.pem"),
            ("numbering-1", "grant.pem"),
        ] {
            let shared = GrantSettings {
                key_id: KeyId::parse(kid).unwrap(),
                private_key_file: dir.join(file),
                ttl_seconds: 60,
            };
            assert!(shared.load(&auth, clock.clone()).is_err(), "{kid} {file}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
