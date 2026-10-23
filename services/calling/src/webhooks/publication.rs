//! Part VI §53 — direct publication of Calling's public events to the Developer Platform.
//!
//! Every domain service publishes its normalized platform envelopes directly to the
//! Developer Platform's `POST /internal/v1/platform/events` (`developer-event.publish`; the
//! SAME path and envelope Calling's own ingress serves, de-duplicated there on
//! (producer, `event_id`)). Calling's outbox publisher ([`super::worker`]) therefore has up
//! to two receivers, chosen by [`PublicEventsDestination`]
//! (`CALLING_PUBLIC_EVENTS_DESTINATION`):
//!
//! * `local` (default) — the in-process webhook pipeline only (projection into the public
//!   catalog, customer subscriptions): today's behaviour, unchanged;
//! * `developer-platform` — the Developer Platform only;
//! * `both` — both (shadow comparison before the cutover; the Developer Platform ingests in
//!   shadow mode until then, so customers see no change).
//!
//! Only Calling's own committed `call.*`, `recording.*` and `voicemail.*` public events
//! reach the Developer Platform ([`is_published_to_developer_platform`]): the stored
//! envelope, unchanged (producer `cfmeet.calling`, the stable outbox `event_id`, the
//! committed correlation id, the call's tenant / application / environment ownership).
//! Internal-only facts and `usage.*` are acknowledged without leaving Calling. Each
//! receiver's outcome is recorded on the outbox row (`local_outcome` /
//! `developer_platform_outcome`) with the attempt, so a retry re-sends only to the receiver
//! that has not accepted. A Developer Platform 4xx other than `RATE_LIMITED` is a permanent
//! refusal ([`DEVELOPER_PLATFORM_REJECTED`]); anything else is retried with the outbox's
//! bounded backoff and dead-lettered at its attempt cap. Customer delivery through
//! Calling stays governed by Calling's own webhook settings.

use std::sync::Arc;

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, OutboundServiceRequest, ServiceId,
};
use serde_json::Value;

use super::{
    catalog,
    worker::{EventTransport, PRODUCER_CALLING},
};

/// The Developer Platform's event ingestion path.
pub const PLATFORM_EVENTS_PATH: &str = "/internal/v1/platform/events";

/// The Developer Platform refused the envelope for good (dead-letters once every receiver
/// settled; never re-sent there).
pub const DEVELOPER_PLATFORM_REJECTED: &str = "DEVELOPER_PLATFORM_REJECTED";
/// Transient failure of the Developer Platform (retried).
pub const DEVELOPER_PLATFORM_UNAVAILABLE: &str = "DEVELOPER_PLATFORM_UNAVAILABLE";
/// No `CF_DEVELOPER_PLATFORM_BASE_URL` (retried; startup refuses this configuration).
pub const DEVELOPER_PLATFORM_NOT_CONFIGURED: &str = "DEVELOPER_PLATFORM_NOT_CONFIGURED";

/// Whether a failure code is a permanent refusal (never retried at that receiver).
pub fn is_permanent(code: &str) -> bool {
    code == DEVELOPER_PLATFORM_REJECTED
}

/// `CALLING_PUBLIC_EVENTS_DESTINATION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublicEventsDestination {
    /// The in-process webhook pipeline only (default: today's behaviour, unchanged).
    #[default]
    Local,
    /// The Developer Platform only.
    DeveloperPlatform,
    /// Both (shadow comparison before the cutover).
    Both,
}

impl PublicEventsDestination {
    pub const VARIABLE: &'static str = "CALLING_PUBLIC_EVENTS_DESTINATION";

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "local" => Some(Self::Local),
            "developer-platform" => Some(Self::DeveloperPlatform),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    /// Reads [`Self::VARIABLE`] (unset: `local`).
    pub fn from_lookup(
        get: &impl Fn(&str) -> Option<String>,
    ) -> Result<Self, crate::config::ConfigError> {
        match get(Self::VARIABLE)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            None => Ok(Self::default()),
            Some(v) => Self::parse(&v).ok_or_else(|| {
                crate::config::ConfigError::invalid(
                    Self::VARIABLE,
                    "must be local, developer-platform or both".to_string(),
                )
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::DeveloperPlatform => "developer-platform",
            Self::Both => "both",
        }
    }

    pub fn includes_local(self) -> bool {
        matches!(self, Self::Local | Self::Both)
    }

    pub fn includes_developer_platform(self) -> bool {
        matches!(self, Self::DeveloperPlatform | Self::Both)
    }
}

/// Whether a Calling envelope is one of the public event families Calling publishes to
/// the Developer Platform (`call.*`, `recording.*`, `voicemail.*`).
pub fn is_published_to_developer_platform(envelope: &Value) -> bool {
    catalog::project_calling(envelope).is_some_and(|e| {
        ["call.", "recording.", "voicemail."]
            .iter()
            .any(|family| e.event_type.starts_with(family))
    })
}

/// The Developer Platform's service-authenticated event ingestion.
pub struct DeveloperPlatformTransport {
    client: Arc<InternalServiceClient>,
}

impl DeveloperPlatformTransport {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl EventTransport for DeveloperPlatformTransport {
    async fn publish(&self, producer: &str, envelope: &Value) -> Result<(), &'static str> {
        // Only Calling's own public call / recording / voicemail events leave Calling.
        if producer != PRODUCER_CALLING || !is_published_to_developer_platform(envelope) {
            return Ok(());
        }
        // The hop continues the committed correlation id.
        let request = match envelope
            .get("correlation_id")
            .and_then(Value::as_str)
            .and_then(|c| CorrelationId::parse(c).ok())
        {
            Some(c) => OutboundServiceRequest::continuing(ServiceId::developer_platform(), &c),
            None => OutboundServiceRequest::new_operation(ServiceId::developer_platform()),
        };
        let outcome = match self
            .client
            .post_json(&request, PLATFORM_EVENTS_PATH, envelope)
            .await
        {
            Ok(_) => Ok(()),
            Err(InternalCallError::Status {
                status: 400..=499,
                code,
                ..
            }) if code.as_deref() != Some("RATE_LIMITED") => Err(DEVELOPER_PLATFORM_REJECTED),
            Err(InternalCallError::UnknownTarget(_)) => Err(DEVELOPER_PLATFORM_NOT_CONFIGURED),
            Err(_) => Err(DEVELOPER_PLATFORM_UNAVAILABLE),
        };
        let result: &'static str = match outcome {
            Ok(()) => "published",
            Err(DEVELOPER_PLATFORM_REJECTED) => "rejected",
            Err(_) => "retry",
        };
        crate::metrics::inc(
            "calling_developer_platform_events_total",
            &[("outcome", result)],
        );
        outcome
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_default_is_local_and_values_parse() {
        let unset = |_: &str| None;
        assert_eq!(
            PublicEventsDestination::from_lookup(&unset).unwrap(),
            PublicEventsDestination::Local
        );
        for d in [
            PublicEventsDestination::Local,
            PublicEventsDestination::DeveloperPlatform,
            PublicEventsDestination::Both,
        ] {
            assert_eq!(PublicEventsDestination::parse(d.as_str()), Some(d));
        }
        assert!(PublicEventsDestination::parse("calling").is_none());
        assert!(PublicEventsDestination::Local.includes_local());
        assert!(!PublicEventsDestination::Local.includes_developer_platform());
        assert!(!PublicEventsDestination::DeveloperPlatform.includes_local());
        assert!(PublicEventsDestination::Both.includes_local());
        assert!(PublicEventsDestination::Both.includes_developer_platform());
        let bad = |_: &str| Some("everywhere".to_string());
        assert!(PublicEventsDestination::from_lookup(&bad).is_err());
    }

    #[test]
    fn only_call_recording_and_voicemail_events_reach_the_developer_platform() {
        let env = |event_type: &str, to: Option<&str>| {
            json!({ "payload": { "event_type": event_type, "to_state": to,
                                 "call_id": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a" } })
        };
        assert!(is_published_to_developer_platform(&env(
            "CALL_STATE_CHANGED",
            Some("ENDED")
        )));
        assert!(is_published_to_developer_platform(&env(
            "RECORDING_READY",
            None
        )));
        assert!(is_published_to_developer_platform(&env(
            "VOICEMAIL_RECEIVED",
            None
        )));
        assert!(!is_published_to_developer_platform(&env(
            "CALL_USAGE_FINALIZED",
            None
        )));
        assert!(!is_published_to_developer_platform(&env(
            "CALL_MEDIA_CREDENTIAL_ISSUED",
            None
        )));
    }
}
