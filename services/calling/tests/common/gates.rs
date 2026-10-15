//! Deterministic barriers around the scripted Telephony and Privacy Relay ports
//! (Calling-side tests only). A test arms a gate; the next call through the gated port
//! announces itself ("entered") and waits until the test releases it, so a race is
//! reproduced at an exact point instead of by timing. Every call through a port is also
//! logged with a global sequence number, so a test can prove what happened AFTER a
//! marker it recorded (e.g. "no originate was issued after the revocation").

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use calling::{
    privacy_relay::{
        CallAuthorization, GrantRequest, PrivacyDecision, PrivacyRelayAuthority, PrivacyRelayError,
        ProxyRequest, ProxyResponse, ResolutionGrant,
    },
    routing::engine::{CandidateRoute, Evidence, RouteCapability},
    telephony::{LegControl, TelephonyError, TelephonyPort},
};
use cf_service_auth::{CorrelationId, DelegatedPlatformContext};
use serde_json::Value;
use tokio::sync::Notify;
use uuid::Uuid;

use super::{privacy::FakePrivacyRelay, telephony::FakeTelephony};

/// One armed barrier: the gated call notifies `entered` and waits for `release`.
#[derive(Clone, Default)]
pub struct Gate {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

impl Gate {
    /// Waits until the gated call is inside the gate.
    pub async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    /// Lets the gated call continue.
    pub fn open(&self) {
        self.release.notify_one();
    }

    async fn pass(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

/// A global, totally ordered log of port calls and test markers.
#[derive(Default)]
pub struct Timeline {
    seq: AtomicU64,
    entries: Mutex<Vec<(u64, String)>>,
}

impl Timeline {
    pub fn record(&self, what: impl Into<String>) -> u64 {
        let mut entries = self.entries.lock().unwrap();
        let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        entries.push((n, what.into()));
        n
    }

    pub fn entries(&self) -> Vec<(u64, String)> {
        self.entries.lock().unwrap().clone()
    }

    /// Entries whose label starts with `prefix`, after sequence `after`.
    pub fn after(&self, after: u64, prefix: &str) -> Vec<(u64, String)> {
        self.entries()
            .into_iter()
            .filter(|(n, what)| *n > after && what.starts_with(prefix))
            .collect()
    }
}

/// Where an armed originate gate holds the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginatePoint {
    /// Calling has issued the command; Telephony has not received it yet.
    BeforeTelephony,
    /// Telephony accepted the command (a paid leg exists); the answer is not back yet.
    AfterAccepted,
}

/// [`FakeTelephony`] behind outage switches and an originate barrier.
pub struct GatedTelephony {
    pub inner: Arc<FakeTelephony>,
    pub timeline: Arc<Timeline>,
    /// Every originate / control answers Unavailable without reaching Telephony.
    pub down: AtomicBool,
    /// Only leg controls answer Unavailable.
    pub controls_down: AtomicBool,
    gate: Mutex<Option<(OriginatePoint, Gate)>>,
}

impl GatedTelephony {
    pub fn new(inner: Arc<FakeTelephony>, timeline: Arc<Timeline>) -> Self {
        Self {
            inner,
            timeline,
            down: AtomicBool::new(false),
            controls_down: AtomicBool::new(false),
            gate: Mutex::new(None),
        }
    }

    /// Arms a one-shot barrier for the next originate.
    pub fn arm(&self, point: OriginatePoint) -> Gate {
        let gate = Gate::default();
        *self.gate.lock().unwrap() = Some((point, gate.clone()));
        gate
    }

    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    pub fn set_controls_down(&self, down: bool) {
        self.controls_down.store(down, Ordering::SeqCst);
    }
}

#[async_trait]
impl TelephonyPort for GatedTelephony {
    async fn availability(
        &self,
        country: &str,
        capability: RouteCapability,
        correlation_id: &CorrelationId,
    ) -> Evidence<Vec<CandidateRoute>> {
        self.inner
            .availability(country, capability, correlation_id)
            .await
    }

    async fn originate(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError> {
        let leg = command["call"]["callLegId"].as_str().unwrap_or_default();
        self.timeline.record(format!("originate:{leg}"));
        if self.down.load(Ordering::SeqCst) {
            return Err(TelephonyError::Unavailable);
        }
        let gate = self.gate.lock().unwrap().take();
        if let Some((OriginatePoint::BeforeTelephony, g)) = &gate {
            g.pass().await;
        }
        let result = self
            .inner
            .originate(delegated, correlation_id, command)
            .await;
        if result.is_ok() {
            self.timeline.record(format!("accepted:{leg}"));
        }
        if let Some((OriginatePoint::AfterAccepted, g)) = &gate {
            g.pass().await;
        }
        result
    }

    async fn control(
        &self,
        telephony_leg_id: Uuid,
        action: LegControl,
        correlation_id: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError> {
        if self.down.load(Ordering::SeqCst) || self.controls_down.load(Ordering::SeqCst) {
            return Err(TelephonyError::Unavailable);
        }
        self.timeline
            .record(format!("control:{action:?}:{telephony_leg_id}"));
        self.inner
            .control(telephony_leg_id, action, correlation_id, command)
            .await
    }

    async fn create_bridge(
        &self,
        correlation_id: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError> {
        self.inner.create_bridge(correlation_id, command).await
    }

    async fn end_bridge(
        &self,
        bridge_id: Uuid,
        correlation_id: &CorrelationId,
    ) -> Result<(), TelephonyError> {
        self.inner.end_bridge(bridge_id, correlation_id).await
    }
}

/// [`FakePrivacyRelay`] with a barrier on Contact Resolution Grants and an access log.
pub struct GatedPrivacy {
    pub inner: Arc<FakePrivacyRelay>,
    pub timeline: Arc<Timeline>,
    /// Armed: the next grant is ISSUED by Privacy Relay, then held before Calling gets it.
    grant_gate: Mutex<Option<Gate>>,
    pub authorizations: AtomicU64,
    pub proxied: AtomicU64,
}

impl GatedPrivacy {
    pub fn new(inner: Arc<FakePrivacyRelay>, timeline: Arc<Timeline>) -> Self {
        Self {
            inner,
            timeline,
            grant_gate: Mutex::new(None),
            authorizations: AtomicU64::new(0),
            proxied: AtomicU64::new(0),
        }
    }

    pub fn arm_grant(&self) -> Gate {
        let gate = Gate::default();
        *self.grant_gate.lock().unwrap() = Some(gate.clone());
        gate
    }
}

#[async_trait]
impl PrivacyRelayAuthority for GatedPrivacy {
    async fn authorize_call(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        request: &CallAuthorization,
    ) -> Result<PrivacyDecision, PrivacyRelayError> {
        self.authorizations.fetch_add(1, Ordering::SeqCst);
        self.timeline.record("authorize");
        self.inner
            .authorize_call(delegated, correlation_id, request)
            .await
    }

    async fn proxy(&self, request: ProxyRequest) -> Result<ProxyResponse, PrivacyRelayError> {
        self.proxied.fetch_add(1, Ordering::SeqCst);
        self.inner.proxy(request).await
    }

    async fn grant_for_execution(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        request: &GrantRequest,
    ) -> Result<ResolutionGrant, PrivacyRelayError> {
        let result = self
            .inner
            .grant_for_execution(delegated, correlation_id, request)
            .await;
        self.timeline.record(format!(
            "grant:{}:{}",
            request.call_leg_id,
            if result.is_ok() { "issued" } else { "refused" }
        ));
        let gate = self.grant_gate.lock().unwrap().take();
        if let Some(g) = gate {
            g.pass().await;
        }
        result
    }

    async fn ensure_relay_identity(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        relay_session_id: Uuid,
        country: &str,
    ) -> Result<(), PrivacyRelayError> {
        self.inner
            .ensure_relay_identity(delegated, correlation_id, relay_session_id, country)
            .await
    }
}
