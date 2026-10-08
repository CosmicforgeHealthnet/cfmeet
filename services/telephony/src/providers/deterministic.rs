//! Deterministic, scriptable interconnect (`DETERMINISTIC_TEST` adapter).
//!
//! It is NOT a carrier and never claims to be one: the service refuses to register it in
//! the `staging` or `production` runtime environments. It exists so the generic provider
//! contract suite, the unknown-outcome / failover / reconciliation races and the business
//! flows can run against a provider whose behaviour is exactly controlled — including
//! outcomes a real network produces only by accident (a timeout after the provider
//! already created the call, a late success, a lost callback).
//!
//! Its HTTP callbacks are authenticated with HMAC-SHA-256 over the raw body
//! (`x-cfmeet-provider-signature: v1=<hex>`), verified in constant time before parsing.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use super::{
    CapacityReport, ControlOutcome, EventRejection, OriginateOutcome, OriginateRequest,
    ProviderEvent, ProviderIngress, QueryKey, QueryOutcome, TelecomInterconnectProvider,
    UnknownReason,
};
use crate::domain::{
    EndReason, FailureCategory, Health, LegStatus, NormalizedEvent, ProviderCapability,
};

pub const SIGNATURE_HEADER: &str = "x-cfmeet-provider-signature";

/// What the next origination does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    /// Accept and create the call.
    Accept,
    /// Create the call but answer as if the response was lost (late success scenario).
    AcceptButLoseResponse(UnknownReason),
    /// Never create the call, and fail definitively.
    Fail {
        transmitted: bool,
        reason: EndReason,
    },
    /// Never create the call, but the outcome is unknown to the caller.
    LoseRequest(UnknownReason),
    /// Optionally create the call, then never answer within the request deadline (the
    /// executor's own deadline decides: a timeout AFTER the request left).
    Stall { create: bool },
    /// Create the call, then the worker process dies before it can record anything
    /// (the executing task panics).
    AcceptThenCrash,
    /// A provider-side error (the interconnect, not the callee): definite, no call.
    ProviderError { transmitted: bool },
}

#[derive(Debug, Clone)]
pub struct FakeCall {
    pub provider_call_ref: String,
    pub correlation_key: String,
    pub status: LegStatus,
    pub end_reason: Option<EndReason>,
}

#[derive(Default)]
struct State {
    script: VecDeque<Script>,
    calls: HashMap<String, FakeCall>,
    /// Correlation keys that reached the provider (whether or not a call was created).
    seen: Vec<String>,
    /// Correlation keys of every created call (provider effects).
    effects: Vec<String>,
    terminated: Vec<String>,
    health: Option<Health>,
    max_concurrent: Option<u32>,
    query_unknown: bool,
    /// Native idempotency: a repeated idempotency key returns the existing call.
    native_idempotency: bool,
    by_idempotency_key: HashMap<String, String>,
    terminate_outcome: Option<ControlOutcome>,
    next_event: u64,
}

/// The deterministic provider.
#[derive(Clone)]
pub struct DeterministicProvider {
    id: String,
    capabilities: Vec<ProviderCapability>,
    callback_secret: Arc<Vec<u8>>,
    state: Arc<Mutex<State>>,
}

impl DeterministicProvider {
    pub fn new(id: &str, callback_secret: &[u8]) -> Self {
        Self {
            id: id.to_string(),
            capabilities: vec![
                ProviderCapability::OutboundVoice,
                ProviderCapability::InboundVoice,
                ProviderCapability::StatusQuery,
                ProviderCapability::Events,
                ProviderCapability::Health,
                ProviderCapability::Capacity,
                ProviderCapability::CallerIdSelection,
                ProviderCapability::Dtmf,
            ],
            callback_secret: Arc::new(callback_secret.to_vec()),
            state: Arc::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Queues the behaviour of the next originations (default: Accept).
    pub fn script(&self, steps: &[Script]) {
        self.lock().script.extend(steps.iter().copied());
    }

    pub fn set_health(&self, health: Health) {
        self.lock().health = Some(health);
    }

    pub fn set_capacity(&self, max: Option<u32>) {
        self.lock().max_concurrent = max;
    }

    /// Makes status read-back answer Unknown.
    pub fn set_query_unknown(&self, unknown: bool) {
        self.lock().query_unknown = unknown;
    }

    /// Native idempotency (`IDEMPOTENT_ORIGINATION`): the provider deduplicates by the
    /// request's idempotency key and returns its own idempotency reference.
    pub fn set_native_idempotency(&self, on: bool) {
        self.lock().native_idempotency = on;
    }

    /// Makes terminate answer this outcome without ending the call (None: normal).
    pub fn set_terminate_outcome(&self, outcome: Option<ControlOutcome>) {
        self.lock().terminate_outcome = outcome;
    }

    /// The provider "loses" a call entirely (read-back answers Absent from now on).
    pub fn forget(&self, correlation_key: &str) {
        self.lock()
            .calls
            .retain(|_, c| c.correlation_key != correlation_key);
    }

    /// Correlation keys that produced a (paid) provider call.
    pub fn effects(&self) -> Vec<String> {
        self.lock().effects.clone()
    }

    /// Requests that reached the provider (with or without a call).
    pub fn requests(&self) -> Vec<String> {
        self.lock().seen.clone()
    }

    pub fn terminated(&self) -> Vec<String> {
        self.lock().terminated.clone()
    }

    pub fn call(&self, correlation_key: &str) -> Option<FakeCall> {
        self.lock()
            .calls
            .values()
            .find(|c| c.correlation_key == correlation_key)
            .cloned()
    }

    pub fn active_calls(&self) -> usize {
        self.lock()
            .calls
            .values()
            .filter(|c| !c.status.is_terminal())
            .count()
    }

    /// Moves a provider call and returns the normalized event the provider would emit.
    pub fn progress(
        &self,
        correlation_key: &str,
        event: NormalizedEvent,
        at: DateTime<Utc>,
    ) -> ProviderEvent {
        let mut s = self.lock();
        s.next_event += 1;
        let n = s.next_event;
        let call = s
            .calls
            .values_mut()
            .find(|c| c.correlation_key == correlation_key)
            .expect("progress on an existing fake call");
        if let Some(status) = event.target_status() {
            call.status = status;
        }
        if let Some(reason) = event.end_reason() {
            call.end_reason = Some(reason);
        }
        ProviderEvent {
            provider_event_id: format!("ev-{n}"),
            provider_call_ref: Some(call.provider_call_ref.clone()),
            correlation_key: Some(call.correlation_key.clone()),
            event,
            occurred_at: at,
            sequence: Some(n as i64),
            restricted_detail: BTreeMap::new(),
            inbound: None,
            dtmf: None,
            provider_cost_minor: None,
        }
    }

    /// Ends a provider call silently (the callback is "lost"); only read-back sees it.
    pub fn end_silently(&self, correlation_key: &str, reason: EndReason) {
        let mut s = self.lock();
        if let Some(call) = s
            .calls
            .values_mut()
            .find(|c| c.correlation_key == correlation_key)
        {
            call.status = if reason == EndReason::Completed {
                LegStatus::Ended
            } else {
                LegStatus::Failed
            };
            call.end_reason = Some(reason);
        }
    }

    /// Creates a provider-side call nobody asked for (orphan / unknown object).
    pub fn create_unsolicited(&self, provider_call_ref: &str) {
        let mut s = self.lock();
        s.calls.insert(
            provider_call_ref.to_string(),
            FakeCall {
                provider_call_ref: provider_call_ref.to_string(),
                correlation_key: format!("unsolicited-{provider_call_ref}"),
                status: LegStatus::Connected,
                end_reason: None,
            },
        );
    }

    fn create_call(s: &mut State, request: &OriginateRequest) -> String {
        let provider_call_ref = format!("dp-{}", request.correlation_key);
        s.calls.insert(
            provider_call_ref.clone(),
            FakeCall {
                provider_call_ref: provider_call_ref.clone(),
                correlation_key: request.correlation_key.clone(),
                status: LegStatus::Originating,
                end_reason: None,
            },
        );
        s.effects.push(request.correlation_key.clone());
        if s.native_idempotency {
            s.by_idempotency_key
                .insert(request.idempotency_key.clone(), provider_call_ref.clone());
        }
        provider_call_ref
    }

    /// Signs a callback body the way the provider would.
    pub fn sign(&self, body: &[u8]) -> String {
        format!(
            "v1={}",
            hex::encode(hmac_sha256(&self.callback_secret, body))
        )
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

/// Strict callback body of the deterministic provider.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CallbackBody {
    event_id: String,
    call_ref: Option<String>,
    correlation_key: Option<String>,
    event: String,
    occurred_at: DateTime<Utc>,
    sequence: Option<i64>,
    #[serde(default)]
    cost_minor: Option<i64>,
    #[serde(default)]
    currency: Option<String>,
}

#[async_trait]
impl TelecomInterconnectProvider for DeterministicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &[ProviderCapability] {
        &self.capabilities
    }

    async fn originate(&self, request: OriginateRequest) -> OriginateOutcome {
        let step = {
            let s = self.lock();
            if s.native_idempotency {
                if let Some(existing) = s.by_idempotency_key.get(&request.idempotency_key) {
                    // Same key: the same provider call, no second effect.
                    return OriginateOutcome::Accepted {
                        provider_call_ref: existing.clone(),
                        provider_idempotency_ref: Some(format!("idem-{}", request.idempotency_key)),
                    };
                }
            }
            s.script.front().copied().unwrap_or(Script::Accept)
        };
        if let Script::Stall { create } = step {
            {
                let mut s = self.lock();
                s.script.pop_front();
                s.seen.push(request.correlation_key.clone());
                if create {
                    Self::create_call(&mut s, &request);
                }
            }
            tokio::time::sleep(request.request_timeout * 4).await;
            return OriginateOutcome::Accepted {
                provider_call_ref: format!("dp-{}", request.correlation_key),
                provider_idempotency_ref: None,
            };
        }
        if step == Script::AcceptThenCrash {
            {
                let mut s = self.lock();
                s.script.pop_front();
                s.seen.push(request.correlation_key.clone());
                Self::create_call(&mut s, &request);
            }
            panic!("simulated worker crash after the provider accepted");
        }
        let mut s = self.lock();
        let step = s.script.pop_front().unwrap_or(Script::Accept);
        if !matches!(
            step,
            Script::Fail {
                transmitted: false,
                ..
            } | Script::ProviderError { transmitted: false }
        ) {
            s.seen.push(request.correlation_key.clone());
        }
        let create = |s: &mut State| Self::create_call(s, &request);
        match step {
            Script::Accept => {
                if let Some(max) = s.max_concurrent {
                    let active = s.calls.values().filter(|c| !c.status.is_terminal()).count();
                    if active as u32 >= max {
                        return OriginateOutcome::DefiniteFailure {
                            category: FailureCategory::Routing,
                            reason: EndReason::Failed,
                            transmitted: true,
                        };
                    }
                }
                let provider_call_ref = create(&mut s);
                let provider_idempotency_ref = s
                    .native_idempotency
                    .then(|| format!("idem-{}", request.idempotency_key));
                OriginateOutcome::Accepted {
                    provider_call_ref,
                    provider_idempotency_ref,
                }
            }
            Script::AcceptButLoseResponse(reason) => {
                create(&mut s);
                OriginateOutcome::UnknownOutcome { reason }
            }
            Script::Fail {
                transmitted,
                reason,
            } => OriginateOutcome::DefiniteFailure {
                category: FailureCategory::TargetUnavailable,
                reason,
                transmitted,
            },
            Script::LoseRequest(reason) => OriginateOutcome::UnknownOutcome { reason },
            Script::ProviderError { transmitted } => OriginateOutcome::DefiniteFailure {
                category: FailureCategory::Network,
                reason: EndReason::Failed,
                transmitted,
            },
            Script::Stall { .. } | Script::AcceptThenCrash => unreachable!("handled above"),
        }
    }

    async fn terminate(&self, provider_call_ref: &str) -> ControlOutcome {
        let mut s = self.lock();
        s.terminated.push(provider_call_ref.to_string());
        if let Some(outcome) = s.terminate_outcome {
            return outcome;
        }
        match s.calls.get_mut(provider_call_ref) {
            Some(call) if !call.status.is_terminal() => {
                call.status = LegStatus::Ended;
                call.end_reason = Some(EndReason::Completed);
                ControlOutcome::Done
            }
            _ => ControlOutcome::AlreadyGone,
        }
    }

    async fn hold(&self, provider_call_ref: &str) -> ControlOutcome {
        let mut s = self.lock();
        match s.calls.get_mut(provider_call_ref) {
            Some(call) if call.status == LegStatus::Connected => {
                call.status = LegStatus::Held;
                ControlOutcome::Done
            }
            Some(_) => ControlOutcome::DefiniteFailure,
            None => ControlOutcome::AlreadyGone,
        }
    }

    async fn resume(&self, provider_call_ref: &str) -> ControlOutcome {
        let mut s = self.lock();
        match s.calls.get_mut(provider_call_ref) {
            Some(call) if call.status == LegStatus::Held => {
                call.status = LegStatus::Connected;
                ControlOutcome::Done
            }
            Some(_) => ControlOutcome::DefiniteFailure,
            None => ControlOutcome::AlreadyGone,
        }
    }

    async fn query_call(&self, key: &QueryKey) -> QueryOutcome {
        let s = self.lock();
        if s.query_unknown {
            return QueryOutcome::Unknown;
        }
        let call = match &key.provider_call_ref {
            Some(r) => s.calls.get(r),
            None => s
                .calls
                .values()
                .find(|c| c.correlation_key == key.correlation_key),
        };
        match call {
            None => QueryOutcome::Absent,
            Some(c) if c.status.is_terminal() => QueryOutcome::Ended {
                provider_call_ref: c.provider_call_ref.clone(),
                reason: c.end_reason.unwrap_or(EndReason::Completed),
            },
            Some(c) => QueryOutcome::Active {
                provider_call_ref: c.provider_call_ref.clone(),
                status: c.status,
            },
        }
    }

    async fn list_active(&self) -> Option<Vec<super::ActiveCall>> {
        Some(
            self.lock()
                .calls
                .values()
                .filter(|c| !c.status.is_terminal())
                .map(|c| super::ActiveCall {
                    provider_call_ref: c.provider_call_ref.clone(),
                    correlation_key: Some(c.correlation_key.clone()),
                    status: c.status,
                })
                .collect(),
        )
    }

    async fn health(&self) -> Health {
        self.lock().health.unwrap_or(Health::Healthy)
    }

    async fn capacity(&self) -> CapacityReport {
        let s = self.lock();
        CapacityReport {
            max_concurrent: s.max_concurrent,
            in_use: Some(s.calls.values().filter(|c| !c.status.is_terminal()).count() as u32),
        }
    }

    fn verify_event(&self, ingress: &ProviderIngress) -> Result<(), EventRejection> {
        let Some(header) = ingress.headers.get(SIGNATURE_HEADER) else {
            return Err(EventRejection::Unauthenticated);
        };
        let Some(hex_sig) = header.strip_prefix("v1=") else {
            return Err(EventRejection::Unauthenticated);
        };
        let Ok(given) = hex::decode(hex_sig) else {
            return Err(EventRejection::Unauthenticated);
        };
        let expected = hmac_sha256(&self.callback_secret, &ingress.body);
        if given.len() == expected.len() && bool::from(given.ct_eq(&expected)) {
            Ok(())
        } else {
            Err(EventRejection::Unauthenticated)
        }
    }

    fn normalize_event(
        &self,
        ingress: &ProviderIngress,
    ) -> Result<Vec<ProviderEvent>, EventRejection> {
        let body: CallbackBody =
            serde_json::from_slice(&ingress.body).map_err(|_| EventRejection::Malformed)?;
        let event = match body.event.as_str() {
            "accepted" => NormalizedEvent::LegAccepted,
            "ringing" => NormalizedEvent::LegRinging,
            "answered" => NormalizedEvent::LegAnswered,
            "connected" => NormalizedEvent::LegConnected,
            "held" => NormalizedEvent::LegHeld,
            "resumed" => NormalizedEvent::LegResumed,
            "completed" => NormalizedEvent::LegEnded,
            "busy" => NormalizedEvent::LegBusy,
            "no-answer" => NormalizedEvent::LegNoAnswer,
            "rejected" => NormalizedEvent::LegDeclined,
            "failed" => NormalizedEvent::LegFailed,
            _ => return Err(EventRejection::Malformed),
        };
        let mut detail = BTreeMap::new();
        detail.insert("provider_status".to_string(), body.event.clone());
        let cost = match (body.cost_minor, body.currency) {
            (Some(c), Some(cur)) if c >= 0 && cur.len() == 3 => Some((c, cur)),
            (None, None) => None,
            _ => return Err(EventRejection::Malformed),
        };
        Ok(vec![ProviderEvent {
            provider_event_id: body.event_id,
            provider_call_ref: body.call_ref,
            correlation_key: body.correlation_key,
            event,
            occurred_at: body.occurred_at,
            sequence: body.sequence,
            restricted_detail: detail,
            inbound: None,
            dtmf: None,
            provider_cost_minor: cost,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
