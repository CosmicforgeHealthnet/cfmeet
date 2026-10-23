//! Service identities and runtime environments.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Canonical identities of CF Meet backend services. Stable, generic and
/// product-independent (contract `cfmeet.platform.common` `ServiceId`).
pub mod services {
    pub const CALLING: &str = "cfmeet.calling";
    pub const PRIVACY_RELAY: &str = "cfmeet.privacy-relay";
    pub const TELEPHONY: &str = "cfmeet.telephony";
    pub const MEETING_SERVICE: &str = "cfmeet.meeting-service";
    /// The shared platform identity authority (Part IV Workstream B).
    pub const PLATFORM_ACCESS: &str = "cfmeet.platform-access";
    /// Digital Communications Numbers (Part IV Workstreams C–D).
    pub const NUMBERING: &str = "cfmeet.numbering";
    /// SMS transport (Part IV Workstreams M–O). Numbering already authorizes it (route
    /// readiness, inbound destinations, sender grants); the service itself is later.
    pub const SMS_GATEWAY: &str = "cfmeet.sms-gateway";
    /// Unified Messaging (Part V): conversations, messages, Messaging devices and their
    /// public key material. Called internally only by the Meeting Service (delegated
    /// first-party backend, meeting sync), Privacy Relay (context authority) and the SMS
    /// Gateway (bridge delivery), each with its own narrow permission.
    pub const MESSAGING: &str = "cfmeet.messaging";
    /// Developer Platform (Part VI): the developer / enterprise control plane. References
    /// Platform Access's canonical tenant / application / environment ids; never an
    /// identity or credential authority of its own.
    pub const DEVELOPER_PLATFORM: &str = "cfmeet.developer-platform";
    /// Public API Gateway (Part VI): customer ingress. Authenticates customer credentials
    /// through Platform Access and forwards a signed delegated context; owns no data.
    pub const API_GATEWAY: &str = "cfmeet.api-gateway";
    /// Operator administration tooling. Not a deployable service: an issuer identity
    /// that only Platform Access trusts, for identity administration.
    pub const OPERATOR_CONSOLE: &str = "cfmeet.operator-console";
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("`{0}` is not a valid service identity (expected cfmeet.<name>)")]
    InvalidServiceId(String),
    #[error("`{0}` is not a valid runtime environment (local, test, staging, production)")]
    InvalidRuntimeEnvironment(String),
}

/// Identity of a deployable CF Meet backend service: `cfmeet.<name>` where `<name>` is
/// lower-case words joined by `-`. Wildcards are impossible by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServiceId(String);

impl ServiceId {
    pub const MAX_LEN: usize = 64;
    const PREFIX: &'static str = "cfmeet.";

    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        let invalid = || IdentityError::InvalidServiceId(bounded(value));
        if value.len() > Self::MAX_LEN {
            return Err(invalid());
        }
        let name = value.strip_prefix(Self::PREFIX).ok_or_else(invalid)?;
        let mut words = name.split('-');
        let first = words.next().unwrap_or_default();
        let first_ok = first.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && first
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        let rest_ok = words.all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
        if first_ok && rest_ok {
            Ok(Self(value.to_string()))
        } else {
            Err(invalid())
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn calling() -> Self {
        Self(services::CALLING.to_string())
    }

    pub fn privacy_relay() -> Self {
        Self(services::PRIVACY_RELAY.to_string())
    }

    pub fn telephony() -> Self {
        Self(services::TELEPHONY.to_string())
    }

    pub fn platform_access() -> Self {
        Self(services::PLATFORM_ACCESS.to_string())
    }

    pub fn numbering() -> Self {
        Self(services::NUMBERING.to_string())
    }

    pub fn sms_gateway() -> Self {
        Self(services::SMS_GATEWAY.to_string())
    }

    pub fn messaging() -> Self {
        Self(services::MESSAGING.to_string())
    }

    pub fn meeting_service() -> Self {
        Self(services::MEETING_SERVICE.to_string())
    }

    pub fn developer_platform() -> Self {
        Self(services::DEVELOPER_PLATFORM.to_string())
    }

    pub fn api_gateway() -> Self {
        Self(services::API_GATEWAY.to_string())
    }

    pub fn operator_console() -> Self {
        Self(services::OPERATOR_CONSOLE.to_string())
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ServiceId {
    type Error = IdentityError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ServiceId> for String {
    fn from(id: ServiceId) -> String {
        id.0
    }
}

/// Runtime / deployment environment of a service identity. This is NOT a customer's
/// application environment (SANDBOX / TEST / PRODUCTION): a staging deployment can serve
/// a SANDBOX application, and both dimensions are checked independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeEnvironment {
    Local,
    Test,
    Staging,
    Production,
}

impl RuntimeEnvironment {
    pub const ALL: &'static [RuntimeEnvironment] = &[
        RuntimeEnvironment::Local,
        RuntimeEnvironment::Test,
        RuntimeEnvironment::Staging,
        RuntimeEnvironment::Production,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeEnvironment::Local => "local",
            RuntimeEnvironment::Test => "test",
            RuntimeEnvironment::Staging => "staging",
            RuntimeEnvironment::Production => "production",
        }
    }

    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        Self::ALL
            .iter()
            .copied()
            .find(|e| e.as_str() == value)
            .ok_or_else(|| IdentityError::InvalidRuntimeEnvironment(bounded(value)))
    }

    /// Whether plaintext-HTTP internal targets may be used (developer machines and test
    /// harnesses only). Staging and production require HTTPS.
    pub fn allows_insecure_transport(&self) -> bool {
        matches!(self, RuntimeEnvironment::Local | RuntimeEnvironment::Test)
    }
}

impl fmt::Display for RuntimeEnvironment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Bounded, control-character-free rendering of untrusted input for error messages.
pub(crate) fn bounded(value: &str) -> String {
    value
        .chars()
        .take(80)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_identities_parse() {
        for id in [
            services::CALLING,
            services::PRIVACY_RELAY,
            services::TELEPHONY,
            services::MEETING_SERVICE,
            services::PLATFORM_ACCESS,
            services::NUMBERING,
            services::SMS_GATEWAY,
            services::MESSAGING,
            services::DEVELOPER_PLATFORM,
            services::API_GATEWAY,
            "cfmeet.a1",
        ] {
            assert_eq!(ServiceId::parse(id).unwrap().as_str(), id);
        }
    }

    #[test]
    fn wildcards_and_malformed_identities_are_rejected() {
        for bad in [
            "",
            "*",
            "cfmeet.*",
            "cfmeet.",
            "cfmeet",
            "calling",
            "cfmeet.Calling",
            "cfmeet.calling-",
            "cfmeet.-calling",
            "cfmeet.calling--x",
            "cfmeet.1calling",
            "cfmeet.calling.extra",
            "cfmeet.calling ",
            "other.calling",
            &format!("cfmeet.{}", "a".repeat(64)),
        ] {
            assert!(ServiceId::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn runtime_environments_round_trip() {
        for env in RuntimeEnvironment::ALL {
            assert_eq!(RuntimeEnvironment::parse(env.as_str()).unwrap(), *env);
        }
        assert!(RuntimeEnvironment::parse("prod").is_err());
        assert!(RuntimeEnvironment::parse("PRODUCTION").is_err());
        assert!(!RuntimeEnvironment::Production.allows_insecure_transport());
        assert!(!RuntimeEnvironment::Staging.allows_insecure_transport());
    }
}
