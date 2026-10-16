//! The deterministic SMS provider: a local, scriptable stand-in for an SMS aggregator
//! (never a carrier, never a network call). Test-only: [`super::guard_runtime`] refuses it
//! in staging and production.
//!
//! Fault modes ([`Script`]): accept, reject, not-sent (throttled / connection refused),
//! timeout AFTER the provider created the message (unknown outcome), timeout BEFORE it
//! did, malformed answer, and an unknown status query. Test helpers produce SIGNED
//! callbacks exactly as a provider would (inbound messages, delivery receipts —
//! duplicated, out of order, late, for unknown references) and deliberately broken ones
//! (bad signature, stale timestamp, tampered body).
//!
//! Callback wire format: JSON body; headers `X-Provider-Event-Id`,
//! `X-Provider-Timestamp` (unix seconds) and `X-Provider-Signature`
//! (`v1=<hex HMAC-SHA256(secret, "v1:" + timestamp + ":" + event id + ":" + body)>`).

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::http::{HeaderMap, HeaderValue};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    error_class, valid_reference, Callback, CallbackError, DeliveryReceipt, InboundMessage,
    QueryOutcome, ReceiptStatus, SmsProvider, SubmitOutcome, SubmitRequest,
};

pub const HEADER_EVENT_ID: &str = "x-provider-event-id";
pub const HEADER_TIMESTAMP: &str = "x-provider-timestamp";
pub const HEADER_SIGNATURE: &str = "x-provider-signature";
pub const TOLERANCE_SECONDS: i64 = 300;

/// What the next submission does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Accept,
    Reject(&'static str),
    NotSent(&'static str),
    /// The provider creates the message, then the answer never arrives in time.
    TimeoutAfterCreate,
    /// The answer never arrives and nothing was created.
    TimeoutBeforeCreate,
    /// The provider answers something unparseable (the message WAS created).
    Malformed,
}

/// One message the simulated provider holds.
#[derive(Debug, Clone)]
pub struct Held {
    pub provider_message_ref: String,
    pub from: String,
    pub to: String,
    pub body: String,
    pub status: ReceiptStatus,
}

#[derive(Default)]
struct State {
    script: VecDeque<Script>,
    by_key: HashMap<String, Held>,
    /// Every submission that reached the provider (idempotency keys, in order).
    submissions: Vec<String>,
    query_unknown: bool,
}

#[derive(Clone)]
pub struct DeterministicSmsProvider {
    id: String,
    secret: Arc<Zeroizing<Vec<u8>>>,
    state: Arc<Mutex<State>>,
    /// How long a "timeout" stalls (the gateway's submit timeout must be shorter).
    stall: Duration,
    cost_per_segment_minor: i64,
}

impl DeterministicSmsProvider {
    pub fn new(id: &str, callback_secret: &[u8]) -> Self {
        Self {
            id: id.to_string(),
            secret: Arc::new(Zeroizing::new(callback_secret.to_vec())),
            state: Arc::default(),
            stall: Duration::from_secs(30),
            cost_per_segment_minor: 4,
        }
    }

    pub fn with_stall(mut self, stall: Duration) -> Self {
        self.stall = stall;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn script(&self, steps: &[Script]) {
        self.lock().script.extend(steps.iter().copied());
    }

    pub fn set_query_unknown(&self, unknown: bool) {
        self.lock().query_unknown = unknown;
    }

    /// Idempotency keys of every submission that reached the provider.
    pub fn submissions(&self) -> Vec<String> {
        self.lock().submissions.clone()
    }

    /// The messages the provider holds (what a carrier would send).
    pub fn held(&self) -> Vec<Held> {
        self.lock().by_key.values().cloned().collect()
    }

    pub fn held_by_key(&self, key: &str) -> Option<Held> {
        self.lock().by_key.get(key).cloned()
    }

    fn create(&self, request: &SubmitRequest) -> Held {
        let mut s = self.lock();
        let held = Held {
            provider_message_ref: format!("dsms-{}", Uuid::new_v4().simple()),
            from: request.from.to_string(),
            to: request.to.to_string(),
            body: request.body.to_string(),
            status: ReceiptStatus::Accepted,
        };
        s.by_key
            .insert(request.idempotency_key.clone(), held.clone());
        held
    }

    /// Signs `body` as this provider's callback with event id `event_id` at `timestamp`.
    pub fn sign(&self, event_id: &str, timestamp: i64, body: &[u8]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let signature = cf_webhooks::signing::sign(&self.secret, timestamp, event_id, body);
        headers.insert(HEADER_EVENT_ID, HeaderValue::from_str(event_id).unwrap());
        headers.insert(
            HEADER_TIMESTAMP,
            HeaderValue::from_str(&timestamp.to_string()).unwrap(),
        );
        headers.insert(HEADER_SIGNATURE, HeaderValue::from_str(&signature).unwrap());
        headers
    }

    /// A signed inbound-message callback.
    pub fn inbound_callback(
        &self,
        from: &str,
        to: &str,
        text: &str,
        at: DateTime<Utc>,
    ) -> (HeaderMap, Vec<u8>) {
        let event = format!("din-{}", Uuid::new_v4().simple());
        let body = json!({
            "kind": "INBOUND", "eventId": event, "from": from, "to": to, "text": text,
            "receivedAt": at.to_rfc3339(),
        })
        .to_string()
        .into_bytes();
        (self.sign(&event, at.timestamp(), &body), body)
    }

    /// A signed delivery receipt for `provider_message_ref` (the provider's own state is
    /// updated as a carrier report would).
    pub fn receipt_callback(
        &self,
        provider_message_ref: &str,
        status: ReceiptStatus,
        error_code: Option<&str>,
        at: DateTime<Utc>,
    ) -> (HeaderMap, Vec<u8>) {
        {
            let mut s = self.lock();
            if let Some(h) = s
                .by_key
                .values_mut()
                .find(|h| h.provider_message_ref == provider_message_ref)
            {
                h.status = status;
            }
        }
        let event = format!("dlr-{}", Uuid::new_v4().simple());
        let body = json!({
            "kind": "RECEIPT", "eventId": event, "messageRef": provider_message_ref,
            "status": status.as_str(), "errorCode": error_code,
            "costMinor": (status == ReceiptStatus::Delivered).then_some(self.cost_per_segment_minor),
            "occurredAt": at.to_rfc3339(),
        })
        .to_string()
        .into_bytes();
        (self.sign(&event, at.timestamp(), &body), body)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireCallback {
    kind: String,
    event_id: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    received_at: Option<DateTime<Utc>>,
    #[serde(default)]
    message_ref: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    cost_minor: Option<i64>,
    #[serde(default)]
    occurred_at: Option<DateTime<Utc>>,
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let v = values.next()?;
    if values.next().is_some() {
        return None;
    }
    v.to_str().ok()
}

#[async_trait]
impl SmsProvider for DeterministicSmsProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn test_only(&self) -> bool {
        true
    }

    fn native_idempotency(&self) -> bool {
        true
    }

    async fn submit(&self, request: SubmitRequest) -> SubmitOutcome {
        // Native idempotency: the same key is the same message, never a second one.
        if let Some(held) = self.lock().by_key.get(&request.idempotency_key).cloned() {
            return SubmitOutcome::Accepted {
                provider_message_ref: held.provider_message_ref,
                cost_minor: Some(self.cost_per_segment_minor * request.segments as i64),
            };
        }
        let step = {
            let mut s = self.lock();
            let step = s.script.pop_front().unwrap_or(Script::Accept);
            if !matches!(step, Script::NotSent(_) | Script::TimeoutBeforeCreate) {
                s.submissions.push(request.idempotency_key.clone());
            }
            step
        };
        match step {
            Script::Accept => {
                let held = self.create(&request);
                SubmitOutcome::Accepted {
                    provider_message_ref: held.provider_message_ref,
                    cost_minor: Some(self.cost_per_segment_minor * request.segments as i64),
                }
            }
            Script::Reject(code) => SubmitOutcome::Rejected { code },
            Script::NotSent(code) => SubmitOutcome::NotSent { code },
            Script::TimeoutAfterCreate => {
                self.create(&request);
                tokio::time::sleep(self.stall).await;
                SubmitOutcome::Unknown {
                    reason: "TIMEOUT_AFTER_SEND",
                }
            }
            Script::TimeoutBeforeCreate => {
                tokio::time::sleep(self.stall).await;
                SubmitOutcome::Unknown {
                    reason: "TIMEOUT_AFTER_SEND",
                }
            }
            Script::Malformed => {
                self.create(&request);
                SubmitOutcome::Unknown {
                    reason: "MALFORMED_RESPONSE",
                }
            }
        }
    }

    async fn query(&self, idempotency_key: &str) -> QueryOutcome {
        let s = self.lock();
        if s.query_unknown {
            return QueryOutcome::Unknown;
        }
        match s.by_key.get(idempotency_key) {
            Some(h) => QueryOutcome::Found {
                provider_message_ref: h.provider_message_ref.clone(),
                status: h.status,
            },
            None => QueryOutcome::NotFound,
        }
    }

    fn authenticate_callback(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now_unix: i64,
    ) -> Result<(), CallbackError> {
        let event = header(headers, HEADER_EVENT_ID)
            .filter(|e| valid_reference(e))
            .ok_or(CallbackError::Unauthenticated("MISSING_EVENT_ID"))?;
        let timestamp = header(headers, HEADER_TIMESTAMP)
            .ok_or(CallbackError::Unauthenticated("MISSING_TIMESTAMP"))?;
        let signature = header(headers, HEADER_SIGNATURE)
            .ok_or(CallbackError::Unauthenticated("MISSING_SIGNATURE"))?;
        cf_webhooks::signing::verify(
            &self.secret,
            signature,
            timestamp,
            event,
            body,
            now_unix,
            TOLERANCE_SECONDS,
        )
        .map_err(|e| match e {
            cf_webhooks::signing::VerifyError::StaleTimestamp => {
                CallbackError::Unauthenticated("STALE_TIMESTAMP")
            }
            _ => CallbackError::Unauthenticated("BAD_SIGNATURE"),
        })
    }

    fn parse_callback(&self, body: &[u8]) -> Result<Callback, CallbackError> {
        let wire: WireCallback =
            serde_json::from_slice(body).map_err(|_| CallbackError::Malformed("BODY"))?;
        if !valid_reference(&wire.event_id) {
            return Err(CallbackError::Malformed("EVENT_ID"));
        }
        match wire.kind.as_str() {
            "INBOUND" => {
                let (Some(from), Some(to), Some(text), Some(received_at)) =
                    (wire.from, wire.to, wire.text, wire.received_at)
                else {
                    return Err(CallbackError::Malformed("INBOUND_FIELDS"));
                };
                if text.len() > 4_096 {
                    return Err(CallbackError::Malformed("TEXT_TOO_LONG"));
                }
                Ok(Callback::Inbound(InboundMessage {
                    event_ref: wire.event_id,
                    from: Zeroizing::new(from),
                    to: Zeroizing::new(to),
                    text: Zeroizing::new(text),
                    received_at,
                }))
            }
            "RECEIPT" => {
                let (Some(message_ref), Some(status), Some(occurred_at)) =
                    (wire.message_ref, wire.status, wire.occurred_at)
                else {
                    return Err(CallbackError::Malformed("RECEIPT_FIELDS"));
                };
                if !valid_reference(&message_ref) {
                    return Err(CallbackError::Malformed("MESSAGE_REF"));
                }
                let status =
                    ReceiptStatus::parse(&status).ok_or(CallbackError::Malformed("STATUS"))?;
                Ok(Callback::Receipt(DeliveryReceipt {
                    event_ref: wire.event_id,
                    provider_message_ref: message_ref,
                    status,
                    error_code: error_class(wire.error_code.as_deref()),
                    cost_minor: wire.cost_minor.filter(|c| *c >= 0),
                    occurred_at,
                }))
            }
            _ => Err(CallbackError::Malformed("KIND")),
        }
    }
}

impl super::contract::ContractFixture for DeterministicSmsProvider {
    fn signed_inbound(
        &self,
        from: &str,
        to: &str,
        text: &str,
        at: DateTime<Utc>,
    ) -> (HeaderMap, Vec<u8>) {
        self.inbound_callback(from, to, text, at)
    }

    fn signed_receipt(
        &self,
        provider_message_ref: &str,
        status: ReceiptStatus,
        at: DateTime<Utc>,
    ) -> (HeaderMap, Vec<u8>) {
        self.receipt_callback(provider_message_ref, status, None, at)
    }
}

/// A valid-looking body that is not a callback (tests).
pub fn malformed_body() -> Value {
    json!({ "kind": "SOMETHING", "eventId": "x-1" })
}
