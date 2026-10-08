//! A scripted Privacy Relay authority (Calling-side tests only; the real service is
//! exercised by `privacy_relay_e2e.rs`). It never sees a customer credential: it receives
//! exactly what Calling's port passes — the delegated context and CF Meet ids.

use std::sync::Mutex;

use calling::privacy_relay::{
    CallAuthorization, PrivacyDecision, PrivacyRelayAuthority, PrivacyRelayError, ProxyRequest,
    ProxyResponse,
};
use cf_service_auth::{CorrelationId, DelegatedPlatformContext};
use chrono::{Duration, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// AUTHORIZED iff source and every target are in the allowed set.
    Policy,
    /// Always DENIED with this reason.
    Deny(&'static str),
    Unavailable,
    NotFound,
}

pub struct FakePrivacyRelay {
    pub mode: Mutex<Mode>,
    pub allowed: Mutex<Vec<Uuid>>,
    pub context_id: Uuid,
    pub context_version: Mutex<i64>,
    pub session_version: Mutex<i64>,
    pub requests: Mutex<Vec<(DelegatedPlatformContext, CorrelationId, CallAuthorization)>>,
    /// Relay Sessions that exist, and the tenant that owns each (proxy GET answers).
    pub sessions: Mutex<Vec<(Uuid, Uuid)>>,
    /// Paths Calling forwarded through `proxy`.
    pub proxied: Mutex<Vec<String>>,
}

impl FakePrivacyRelay {
    pub fn new(allowed: Vec<Uuid>) -> Self {
        Self {
            mode: Mutex::new(Mode::Policy),
            allowed: Mutex::new(allowed),
            context_id: Uuid::new_v4(),
            context_version: Mutex::new(1),
            session_version: Mutex::new(1),
            requests: Mutex::new(Vec::new()),
            sessions: Mutex::new(Vec::new()),
            proxied: Mutex::new(Vec::new()),
        }
    }

    /// `session` exists and belongs to `tenant`.
    pub fn own_session(&self, session: Uuid, tenant: Uuid) {
        self.sessions.lock().unwrap().push((session, tenant));
    }

    pub fn set_mode(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }

    pub fn allow(&self, principal: Uuid) {
        self.allowed.lock().unwrap().push(principal);
    }

    pub fn set_versions(&self, context: i64, session: i64) {
        *self.context_version.lock().unwrap() = context;
        *self.session_version.lock().unwrap() = session;
    }

    pub fn requests(&self) -> Vec<(DelegatedPlatformContext, CorrelationId, CallAuthorization)> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl PrivacyRelayAuthority for FakePrivacyRelay {
    async fn authorize_call(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        request: &CallAuthorization,
    ) -> Result<PrivacyDecision, PrivacyRelayError> {
        self.requests.lock().unwrap().push((
            delegated.clone(),
            correlation_id.clone(),
            request.clone(),
        ));
        let mode = *self.mode.lock().unwrap();
        let allowed = self.allowed.lock().unwrap().clone();
        let reason = match mode {
            Mode::Unavailable => return Err(PrivacyRelayError::Unavailable),
            Mode::NotFound => return Err(PrivacyRelayError::NotFound),
            Mode::Deny(reason) => reason,
            Mode::Policy => {
                let ok = allowed.contains(&request.source_principal_id)
                    && request
                        .target_principal_ids
                        .iter()
                        .all(|t| allowed.contains(t));
                if ok {
                    "AUTHORIZED"
                } else {
                    "PARTICIPANT_NOT_AUTHORIZED"
                }
            }
        };
        let authorized = reason == "AUTHORIZED";
        Ok(PrivacyDecision {
            decision_id: Uuid::new_v4(),
            decision: if authorized { "AUTHORIZED" } else { "DENIED" }.to_string(),
            reason: reason.to_string(),
            context_id: self.context_id,
            context_version: *self.context_version.lock().unwrap(),
            relay_session_id: request.relay_session_id,
            relay_session_version: *self.session_version.lock().unwrap(),
            strategy: "APP_ONLY".to_string(),
            policy_version: "CONTEXT_POLICY_V1".to_string(),
            valid_until: authorized.then(|| Utc::now() + Duration::hours(1)),
            decided_at: Utc::now(),
        })
    }

    /// Answers `GET /internal/v1/sessions/{id}` in the delegated tenant's scope (a
    /// foreign or unknown session is 404); everything else is unavailable.
    async fn proxy(&self, request: ProxyRequest) -> Result<ProxyResponse, PrivacyRelayError> {
        self.proxied.lock().unwrap().push(request.path.clone());
        if *self.mode.lock().unwrap() == Mode::Unavailable {
            return Err(PrivacyRelayError::Unavailable);
        }
        let Some(id) = request
            .path
            .strip_prefix("/internal/v1/sessions/")
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Err(PrivacyRelayError::Unavailable);
        };
        let owned = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .any(|(s, t)| *s == id && *t == request.delegated.tenant_id());
        Ok(if owned {
            ProxyResponse {
                status: 200,
                body: serde_json::json!({
                    "relaySessionId": id,
                    "contextId": self.context_id,
                    "status": "ACTIVE",
                }),
                replayed: false,
            }
        } else {
            ProxyResponse {
                status: 404,
                body: serde_json::json!({ "error": { "code": "SESSION_NOT_FOUND", "message": "x" } }),
                replayed: false,
            }
        })
    }
}
