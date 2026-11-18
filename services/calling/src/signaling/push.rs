//! Push-ready ring offers (Part C §88) and the Notification Service call fast lane (I3,
//! roadmap N9).
//!
//! When an offer starts ringing an endpoint that registered a `pushTargetRef`, Calling
//! builds the transport-neutral payload of contract `cfmeet.calling.push-invitation` (call
//! id, offer id, call kind, optional caller display name, issue and expiry times) and hands
//! it — after the authoritative transaction committed, like every realtime signal — to the
//! configured [`PushDispatcher`]. The payload never carries a media credential, provider
//! token, API credential or telephone number: the device acts on it with its own signaling
//! credential (recover, then accept or decline).
//!
//! Transports (`CALLING_PUSH_TRANSPORT`):
//! * `none` (default) — [`NoPushTransport`]: offers reach endpoints through signaling and
//!   recovery only (today's behaviour).
//! * `notifications` — [`NotificationsPushTransport`]: the offer of a CF Meet first-party
//!   recipient (a NATIVE_CF_USER `cfmeet-user:` principal of a FIRST_PARTY tenant; the
//!   `pushTargetRef` of any other endpoint is customer-supplied and never sent) goes to
//!   the Notification Service's I3 (`POST /internal/v1/push/call-invitations`,
//!   `notifications-call-push.dispatch`) in a background task bounded by
//!   `CALLING_PUSH_TIMEOUT_MS`: ringing is never delayed and never fails because of push.
//!   One alert per (call, push target): repeats are dropped until the invitation expires.
//!
//! Partner push (ADR 0063, `CALLING_PARTNER_PUSH_ENABLED`, default off): a ring offer to
//! an ACTIVE EXTERNAL_APPLICATION_PRINCIPAL of an EXTERNAL tenant is also handed to the
//! transport as a [`PartnerPushOffer`] — the Notification Service's `platform-principal`
//! recipient (§6 N-P3), whose push registrations the partner's app made through Messaging
//! and which are sent with the partner's own APNs VoIP / FCM keys. The `notifications`
//! transport sends it to the same I3 with the same rules (background, bounded, no retry),
//! one alert per (call, principal). The first-party path above is unchanged.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

use async_trait::async_trait;
use cf_service_auth::{client::InternalServiceClient, OutboundServiceRequest, ServiceId};
use serde_json::{json, Value};
use uuid::Uuid;

/// One push-ready offer.
#[derive(Debug, Clone, PartialEq)]
pub struct PushOffer {
    pub push_target_ref: Uuid,
    /// The `cfmeet.calling.push-invitation` payload.
    pub payload: Value,
    pub call_id: Uuid,
    /// `cfmeet-user:<uuid>` when the ringing endpoint belongs to a CF Meet first-party
    /// recipient (NATIVE_CF_USER principal of a FIRST_PARTY tenant); `None` otherwise.
    pub first_party_account: Option<String>,
}

/// A push transport. Dispatch is best effort: a lost push never loses state (the offer is
/// recoverable through `GET /v1/calling/invitations`).
#[async_trait]
pub trait PushDispatcher: Send + Sync + 'static {
    /// Delivers `payload` to the application's push target `push_target_ref`.
    async fn dispatch(&self, push_target_ref: Uuid, payload: Value);

    /// Delivers one offer with its recipient facts (default: [`dispatch`](Self::dispatch)).
    async fn dispatch_offer(&self, offer: PushOffer) {
        self.dispatch(offer.push_target_ref, offer.payload).await
    }

    /// Delivers one partner offer (ADR 0063; only produced while
    /// `CALLING_PARTNER_PUSH_ENABLED`). Default: not sent.
    async fn dispatch_partner_offer(&self, _offer: PartnerPushOffer) {
        tracing::debug!("partner push offer not dispatched: the transport has no partner lane");
    }
}

/// The Notification Service `platform-principal` recipient of a partner's user
/// (contract §6 N-P3): the scope triple and the principal, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformPrincipalRecipient {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Uuid,
}

impl PlatformPrincipalRecipient {
    /// The wire shape `{kind: "platform-principal", tenantId, applicationId,
    /// environmentId, principalId}`.
    pub fn to_json(&self) -> Value {
        json!({
            "kind": "platform-principal",
            "tenantId": self.tenant_id,
            "applicationId": self.application_id,
            "environmentId": self.environment_id,
            "principalId": self.principal_id,
        })
    }
}

/// One ring offer to a partner's user (EXTERNAL tenant).
#[derive(Debug, Clone, PartialEq)]
pub struct PartnerPushOffer {
    pub recipient: PlatformPrincipalRecipient,
    /// The `cfmeet.calling.push-invitation` payload.
    pub payload: Value,
    pub call_id: Uuid,
}

/// No push transport configured: offers reach endpoints through signaling and recovery.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPushTransport;

#[async_trait]
impl PushDispatcher for NoPushTransport {
    async fn dispatch(&self, _push_target_ref: Uuid, _payload: Value) {
        tracing::debug!("push-ready offer not dispatched: no push transport is configured");
    }
}

/// `CALLING_PUSH_TRANSPORT` and `CALLING_PUSH_TIMEOUT_MS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushTransportSetting {
    None,
    Notifications { timeout: StdDuration },
}

impl PushTransportSetting {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        match get("CALLING_PUSH_TRANSPORT").as_deref().map(str::trim) {
            None | Some("") | Some("none") => Ok(Self::None),
            Some("notifications") => {
                let ms = match get("CALLING_PUSH_TIMEOUT_MS") {
                    None => 1000,
                    Some(v) => v
                        .trim()
                        .parse::<u64>()
                        .ok()
                        .filter(|ms| (100..=2000).contains(ms))
                        .ok_or("CALLING_PUSH_TIMEOUT_MS must be 100-2000")?,
                };
                Ok(Self::Notifications {
                    timeout: StdDuration::from_millis(ms),
                })
            }
            Some(_) => Err("CALLING_PUSH_TRANSPORT must be none or notifications".into()),
        }
    }
}

/// Where I3 requests go (the service in production; a fake in tests).
#[async_trait]
pub trait CallPushSink: Send + Sync + 'static {
    async fn call_push(&self, body: Value) -> Result<Value, String>;
}

/// The service-authenticated I3 of the Notification Service.
pub struct NotificationServiceCallPush {
    client: Arc<InternalServiceClient>,
}

impl NotificationServiceCallPush {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

/// I3, nested under the Notification Service's internal base URL.
pub const CALL_PUSH_PATH: &str = "/internal/v1/push/call-invitations";

#[async_trait]
impl CallPushSink for NotificationServiceCallPush {
    async fn call_push(&self, body: Value) -> Result<Value, String> {
        let request = OutboundServiceRequest::new_operation(ServiceId::notifications());
        self.client
            .post_json(&request, CALL_PUSH_PATH, &body)
            .await
            .map_err(|e| e.to_string())?
            .json::<Value>()
            .map_err(|e| e.to_string())
    }
}

/// Bound of the de-duplication memory (one replica, ringing calls only).
const DEDUPE_MAX: usize = 50_000;

/// The Notification Service call fast lane (I3), first-party recipients only.
pub struct NotificationsPushTransport {
    sink: Arc<dyn CallPushSink>,
    timeout: StdDuration,
    seen: Mutex<HashMap<(Uuid, Uuid), Instant>>,
}

impl NotificationsPushTransport {
    pub fn new(sink: Arc<dyn CallPushSink>, timeout: StdDuration) -> Self {
        Self {
            sink,
            timeout,
            seen: Mutex::default(),
        }
    }

    /// Records (call, target); `false` when it was already alerted and not yet expired.
    fn first_alert(&self, call: Uuid, target: Uuid, until: Instant) -> bool {
        let now = Instant::now();
        let mut seen = self.seen.lock().expect("dedupe");
        if seen.len() > DEDUPE_MAX {
            seen.retain(|_, t| *t > now);
        }
        match seen.get(&(call, target)) {
            Some(t) if *t > now => false,
            _ => {
                seen.insert((call, target), until);
                true
            }
        }
    }

    /// The I3 body of an offer (contract `dispatchCallPush` 1.2.0).
    pub fn body(offer: &PushOffer, account: &str) -> Value {
        json!({
            "pushTargetRefs": [offer.push_target_ref],
            "payload": offer.payload,
            "expiresAt": offer.payload["expiresAt"],
            "recipientAccountRef": account,
        })
    }

    /// The I3 body of a partner offer (§6 N-P3): the `platform-principal` recipient
    /// instead of push target references (the Notification Service wakes the principal's
    /// own registrations in that scope).
    pub fn partner_body(offer: &PartnerPushOffer) -> Value {
        json!({
            "recipient": offer.recipient.to_json(),
            "payload": offer.payload,
            "expiresAt": offer.payload["expiresAt"],
        })
    }

    /// When the offer's invitation expires (`None`: invalid or already expired), with
    /// the bounded outcome to count otherwise.
    fn until(payload: &Value) -> Result<Instant, &'static str> {
        let expires = payload["expiresAt"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
            .ok_or("invalid")?;
        let left = (expires - chrono::Utc::now())
            .to_std()
            .unwrap_or(StdDuration::ZERO);
        if left.is_zero() {
            return Err("expired");
        }
        Ok(Instant::now() + left)
    }
}

#[async_trait]
impl PushDispatcher for NotificationsPushTransport {
    async fn dispatch(&self, _push_target_ref: Uuid, _payload: Value) {
        // Without recipient facts nothing is sent (the target may be customer-supplied).
        crate::metrics::inc(
            "calling_push_offers_total",
            &[("outcome", "not_first_party")],
        );
    }

    async fn dispatch_offer(&self, offer: PushOffer) {
        let Some(account) = offer.first_party_account.clone() else {
            crate::metrics::inc(
                "calling_push_offers_total",
                &[("outcome", "not_first_party")],
            );
            return;
        };
        let expires = offer.payload["expiresAt"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        let Some(expires) = expires else {
            crate::metrics::inc("calling_push_offers_total", &[("outcome", "invalid")]);
            return;
        };
        let left = (expires - chrono::Utc::now())
            .to_std()
            .unwrap_or(StdDuration::ZERO);
        if left.is_zero() {
            crate::metrics::inc("calling_push_offers_total", &[("outcome", "expired")]);
            return;
        }
        let until = Instant::now() + left;
        if !self.first_alert(offer.call_id, offer.push_target_ref, until) {
            crate::metrics::inc("calling_push_offers_total", &[("outcome", "duplicate")]);
            return;
        }
        let body = Self::body(&offer, &account);
        let sink = self.sink.clone();
        let timeout = self.timeout;
        // Fire and forget: the caller (signal publication after commit) never waits.
        tokio::spawn(async move {
            let outcome = match tokio::time::timeout(timeout, sink.call_push(body)).await {
                Ok(Ok(_)) => "dispatched",
                Ok(Err(_)) => "failed",
                Err(_) => "timeout",
            };
            crate::metrics::inc("calling_push_offers_total", &[("outcome", outcome)]);
            if outcome != "dispatched" {
                tracing::debug!(
                    outcome,
                    "call push not dispatched (recoverable via signaling)"
                );
            }
        });
    }

    async fn dispatch_partner_offer(&self, offer: PartnerPushOffer) {
        let until = match Self::until(&offer.payload) {
            Ok(until) => until,
            Err(outcome) => {
                crate::metrics::inc("calling_partner_push_offers_total", &[("outcome", outcome)]);
                return;
            }
        };
        // One alert per (call, principal): every ringing endpoint of the user shares it.
        if !self.first_alert(offer.call_id, offer.recipient.principal_id, until) {
            crate::metrics::inc(
                "calling_partner_push_offers_total",
                &[("outcome", "duplicate")],
            );
            return;
        }
        let body = Self::partner_body(&offer);
        let sink = self.sink.clone();
        let timeout = self.timeout;
        tokio::spawn(async move {
            let outcome = match tokio::time::timeout(timeout, sink.call_push(body)).await {
                Ok(Ok(_)) => "dispatched",
                Ok(Err(_)) => "failed",
                Err(_) => "timeout",
            };
            crate::metrics::inc("calling_partner_push_offers_total", &[("outcome", outcome)]);
            if outcome != "dispatched" {
                tracing::debug!(
                    outcome,
                    "partner call push not dispatched (recoverable via signaling)"
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transport_switch_defaults_to_none() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            PushTransportSetting::from_lookup(env(&[])),
            Ok(PushTransportSetting::None)
        );
        assert_eq!(
            PushTransportSetting::from_lookup(env(&[("CALLING_PUSH_TRANSPORT", "notifications")])),
            Ok(PushTransportSetting::Notifications {
                timeout: StdDuration::from_millis(1000)
            })
        );
        assert!(
            PushTransportSetting::from_lookup(env(&[("CALLING_PUSH_TRANSPORT", "fcm")])).is_err()
        );
        assert!(PushTransportSetting::from_lookup(env(&[
            ("CALLING_PUSH_TRANSPORT", "notifications"),
            ("CALLING_PUSH_TIMEOUT_MS", "5000")
        ]))
        .is_err());
    }
}
