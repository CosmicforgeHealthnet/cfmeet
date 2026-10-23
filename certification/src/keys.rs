//! Service signing keys and receiver trust bundles derived from the permission registry.

use std::collections::{BTreeMap, BTreeSet};

use cf_service_auth::{
    permissions::REGISTRY, services, KeyId, PublicJwk, RuntimeEnvironment, ServiceId,
    ServiceSigningKey, ServiceTokenSigner, SystemClock, TokenTtl,
};
use serde_json::{json, Value};

/// The runtime environment of every certification stack.
pub const RUNTIME: RuntimeEnvironment = RuntimeEnvironment::Test;

/// `cfmeet.platform-access` -> `platform-access`.
pub fn short(service: &str) -> &str {
    service.strip_prefix("cfmeet.").unwrap_or(service)
}

/// The key id of `service`'s certification key.
pub fn kid(service: &str) -> String {
    format!("{}-cert-1", short(service))
}

/// One freshly generated Ed25519 key per service identity (plus the operator console).
pub struct ServiceKeys {
    keys: BTreeMap<String, ServiceSigningKey>,
}

impl ServiceKeys {
    pub fn generate(services: &[&str]) -> Self {
        let mut keys = BTreeMap::new();
        for s in services
            .iter()
            .copied()
            .chain(std::iter::once(services::OPERATOR_CONSOLE))
        {
            keys.insert(
                s.to_string(),
                ServiceSigningKey::generate(KeyId::parse(&kid(s)).expect("valid kid")),
            );
        }
        Self { keys }
    }

    pub fn key(&self, service: &str) -> &ServiceSigningKey {
        self.keys
            .get(service)
            .unwrap_or_else(|| panic!("no key for {service}"))
    }

    pub fn pem(&self, service: &str) -> String {
        self.key(service).to_pkcs8_pem().to_string()
    }

    pub fn public_jwk(&self, service: &str) -> PublicJwk {
        self.key(service).public_jwk()
    }

    /// A signer for `service` with its REAL key (operator console, forged-hop tests).
    pub fn signer(&self, service: &str) -> ServiceTokenSigner {
        let key = ServiceSigningKey::from_pkcs8_pem(
            KeyId::parse(&kid(service)).expect("valid kid"),
            &self.pem(service),
        )
        .expect("own key parses");
        ServiceTokenSigner::new(
            ServiceId::parse(service).expect("canonical service id"),
            RUNTIME,
            key,
            TokenTtl::default(),
            std::sync::Arc::new(SystemClock),
        )
    }
}

/// The permissions `receiver` grants `issuer` according to the registry, restricted to
/// started grantees (the operator console is always "started": the harness holds it).
pub fn granted(receiver: &str, started: &[&str]) -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut grants: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for d in REGISTRY.iter().filter(|d| d.receiver == receiver) {
        for g in d.grantable_to {
            let present = *g == services::OPERATOR_CONSOLE || started.contains(g);
            if present && *g != receiver {
                grants.entry(g.to_string()).or_default().insert(d.name);
            }
        }
    }
    grants
}

/// The trust bundle of `receiver` (contract `cfmeet.platform.service-trust-bundle` v1).
pub fn trust_bundle(receiver: &str, started: &[&str], keys: &ServiceKeys) -> Value {
    let issuers: Vec<Value> = granted(receiver, started)
        .into_iter()
        .map(|(issuer, permissions)| {
            json!({
                "issuer": issuer,
                "audience": receiver,
                "runtime_environment": RUNTIME.as_str(),
                "keys": [keys.public_jwk(&issuer)],
                "permissions": permissions,
            })
        })
        .collect();
    json!({
        "bundle_version": 1,
        "receiver": receiver,
        "runtime_environment": RUNTIME.as_str(),
        "trusted_issuers": issuers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[&str] = &[
        services::PLATFORM_ACCESS,
        services::DEVELOPER_PLATFORM,
        services::API_GATEWAY,
        services::MEETING_SERVICE,
        services::CALLING,
        services::MESSAGING,
        services::NUMBERING,
        services::SMS_GATEWAY,
        services::PRIVACY_RELAY,
    ];

    #[test]
    fn bundles_follow_the_registry_and_load() {
        let keys = ServiceKeys::generate(ALL);
        for receiver in ALL.iter().filter(|s| **s != services::API_GATEWAY) {
            let bundle = trust_bundle(receiver, ALL, &keys);
            let dir = std::env::temp_dir().join(format!("cf-cert-keys-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join("bundle.json");
            std::fs::write(&p, bundle.to_string()).unwrap();
            let loaded = cf_service_auth::TrustPolicy::from_file(
                &p,
                &ServiceId::parse(receiver).unwrap(),
                RUNTIME,
            );
            std::fs::remove_dir_all(&dir).ok();
            assert!(loaded.is_ok(), "{receiver}: {:?}", loaded.err());
        }
        // Spot checks: the gateway forwards to each customer upstream; the Developer
        // Platform verifies sessions at the Meeting Service; operators administer.
        let pa = granted(services::PLATFORM_ACCESS, ALL);
        assert!(pa[services::API_GATEWAY].contains("platform-credential.authenticate"));
        assert!(pa[services::OPERATOR_CONSOLE].contains("platform-identity.administer"));
        assert!(
            granted(services::MEETING_SERVICE, ALL)[services::DEVELOPER_PLATFORM]
                .contains("user-session.verify")
        );
        assert!(granted(services::CALLING, ALL)[services::API_GATEWAY]
            .contains("calling-request.forward"));
        // Telephony is not started: nothing is granted to it.
        assert!(!granted(services::PRIVACY_RELAY, ALL).contains_key(services::TELEPHONY));
    }
}
