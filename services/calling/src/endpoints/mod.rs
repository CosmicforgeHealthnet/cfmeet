//! Application endpoints (Part C §20): the Calling-owned registry of the devices /
//! application instances a principal can be rung on.
//!
//! * An endpoint belongs to exactly one principal of one tenant / application /
//!   environment (database-pinned). It is registered by the application's BACKEND with
//!   its API credential; browsers and mobile apps never hold that credential.
//! * The endpoint key is an opaque, bounded, non-secret application identifier, unique
//!   among ACTIVE endpoints of an application environment. Re-registering the same key
//!   for the same principal is idempotent; for another principal it is a conflict.
//! * Presence (`ONLINE` / `OFFLINE` + `last_seen_at`) is a ROUTING HINT: it orders which
//!   of a principal's endpoints are rung first when a cap applies. It never authorizes
//!   anything and never adds an endpoint of another principal.
//! * Endpoint kinds are neutral (`WEB`, `MOBILE`, `DESKTOP`, `ENTERPRISE_APP`); device or
//!   vendor names are never authorization concepts.

pub mod http;
pub mod service;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{
    calls::kinds::EndpointCategory,
    domain::{ApplicationId, EnvironmentId, PrincipalId, TenantId},
    models::call_endpoints,
};

pub use service::{EndpointService, NewEndpoint, PresenceWindow};

text_enum!(
    /// Neutral kind of an application endpoint.
    EndpointKind {
        Web = "WEB",
        Mobile = "MOBILE",
        Desktop = "DESKTOP",
        EnterpriseApp = "ENTERPRISE_APP",
    }
);

text_enum!(EndpointStatus {
    Active = "ACTIVE",
    Revoked = "REVOKED",
});

text_enum!(
    /// Routing hint only.
    PresenceState {
        Online = "ONLINE",
        Offline = "OFFLINE",
    }
);

typed_id!(
    /// A registered application endpoint.
    CallEndpointId
);

impl EndpointKind {
    /// The session category a session on this endpoint is recorded with.
    pub fn session_category(&self) -> EndpointCategory {
        match self {
            EndpointKind::Web => EndpointCategory::Web,
            EndpointKind::Mobile => EndpointCategory::Mobile,
            EndpointKind::Desktop | EndpointKind::EnterpriseApp => EndpointCategory::Desktop,
        }
    }
}

/// A validated endpoint key: opaque, 1–128 characters of `[A-Za-z0-9._:-]` starting with
/// an alphanumeric, never shaped like a telephone number or a `tel:` / `sip:` URI (mirrors
/// the database CHECK).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointKey(String);

impl EndpointKey {
    pub fn parse(value: &str) -> Option<Self> {
        let bytes = value.as_bytes();
        let shaped = !bytes.is_empty()
            && bytes.len() <= 128
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
        let phone_like = (7..=15).contains(&bytes.len()) && bytes.iter().all(u8::is_ascii_digit);
        let lower = value.to_ascii_lowercase();
        let uri_like = ["tel:", "sip:", "sips:"]
            .iter()
            .any(|p| lower.starts_with(p));
        (shaped && !phone_like && !uri_like).then(|| Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Media an endpoint can handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EndpointCapabilities {
    pub audio: bool,
    pub video: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEndpoint {
    pub id: CallEndpointId,
    pub tenant_id: TenantId,
    pub application_id: ApplicationId,
    pub environment_id: EnvironmentId,
    pub principal_id: PrincipalId,
    pub endpoint_key: String,
    pub kind: EndpointKind,
    pub capabilities: EndpointCapabilities,
    pub status: EndpointStatus,
    pub presence: PresenceState,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub push_target_ref: Option<uuid::Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl From<call_endpoints::Model> for CallEndpoint {
    fn from(m: call_endpoints::Model) -> Self {
        Self {
            id: CallEndpointId::from_uuid(m.id),
            tenant_id: TenantId::from_uuid(m.tenant_id),
            application_id: ApplicationId::from_uuid(m.application_id),
            environment_id: EnvironmentId::from_uuid(m.environment_id),
            principal_id: PrincipalId::from_uuid(m.principal_id),
            endpoint_key: m.endpoint_key,
            kind: m.endpoint_kind,
            capabilities: EndpointCapabilities {
                audio: m.supports_audio,
                video: m.supports_video,
            },
            status: m.status,
            presence: m.presence_state,
            last_seen_at: m.last_seen_at,
            push_target_ref: m.push_target_ref,
            created_at: m.created_at,
            updated_at: m.updated_at,
            revoked_at: m.revoked_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_keys_are_opaque_and_bounded() {
        for good in [
            "device-1",
            "a",
            "3f2a9c1e-7b3c-4c55-9f2b-1d2e3f4a5b6c",
            "web:tab.42",
        ] {
            assert!(EndpointKey::parse(good).is_some(), "{good}");
        }
        for bad in [
            "",
            "-leading",
            "has space",
            "+2348012345678",
            "2348012345678",
            "tel:123",
            &"x".repeat(129),
        ] {
            assert!(EndpointKey::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn kinds_are_neutral() {
        for kind in EndpointKind::ALL {
            for vendor in ["IPHONE", "ANDROID", "CHROME", "SAFARI", "IOS"] {
                assert!(!kind.as_str().contains(vendor));
            }
        }
    }
}
