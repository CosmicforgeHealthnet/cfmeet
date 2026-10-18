//! A scripted Telephony (Calling-side tests only; the real service is exercised by the
//! cross-service end-to-end test). It receives exactly what Calling's port sends and
//! answers like Telephony's internal API: it never holds a number, and it lets a test
//! decide outages, refusals and lost responses.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use calling::{
    media::MediaProviderError,
    routing::engine::{CandidateRoute, Evidence, RouteCapability, RouteHealth},
    telephony::{media::PublicMediaGateway, LegControl, TelephonyError, TelephonyPort},
};
use cf_service_auth::{CorrelationId, DelegatedPlatformContext};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Accept,
    /// Persisted and accepted, but the response is lost (Calling sees Unavailable).
    AcceptButLoseResponse,
    Refuse(&'static str),
    Unavailable,
}

/// Holds one control request inside the fake until the test releases it (a worker that
/// is "still sending" while its lease expires).
#[derive(Default)]
pub struct ControlGate {
    pub entered: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}

#[derive(Default)]
struct State {
    /// Answers of the next control requests (default: Accept), each optionally gated.
    control_answers: Vec<(Option<Arc<ControlGate>>, Answer)>,
    answers: Vec<Answer>,
    routes: Option<Evidence<Vec<CandidateRoute>>>,
    /// Accepted originate commands by idempotency key -> telephony leg id.
    accepted: Vec<(String, Uuid, Value)>,
    /// Every originate body received (grant secret included, as sent).
    originates: Vec<(DelegatedPlatformContext, Value)>,
    controls: Vec<(Uuid, LegControl, Value)>,
    bridges: Vec<Value>,
    ended_bridges: Vec<Uuid>,
}

#[derive(Default)]
pub struct FakeTelephony {
    state: Mutex<State>,
}

/// One healthy NG route.
pub fn healthy_routes() -> Evidence<Vec<CandidateRoute>> {
    Evidence::Known(vec![CandidateRoute {
        route_ref: "ng-a".into(),
        health: RouteHealth::Healthy,
        capacity_available: Evidence::Known(true),
        estimated_leg_cost_minor: Evidence::Known(100),
        quality_permille: Evidence::Unknown,
        interconnect_class: Default::default(),
    }])
}

impl FakeTelephony {
    pub fn healthy() -> Self {
        let t = Self::default();
        t.set_routes(healthy_routes());
        t
    }

    pub fn set_routes(&self, routes: Evidence<Vec<CandidateRoute>>) {
        self.state.lock().unwrap().routes = Some(routes);
    }

    /// Answers of the next originate calls (default: Accept).
    pub fn script(&self, answers: &[Answer]) {
        self.state
            .lock()
            .unwrap()
            .answers
            .extend_from_slice(answers);
    }

    /// Answers of the next control (TERMINATE / HOLD / …) requests (default: Accept).
    pub fn script_controls(&self, answers: &[Answer]) {
        self.state
            .lock()
            .unwrap()
            .control_answers
            .extend(answers.iter().cloned().map(|a| (None, a)));
    }

    /// The next control request blocks inside the fake (after it was recorded) until
    /// the returned gate is released, then answers `answer`.
    pub fn gate_next_control(&self, answer: Answer) -> Arc<ControlGate> {
        let gate = Arc::new(ControlGate::default());
        self.state
            .lock()
            .unwrap()
            .control_answers
            .push((Some(gate.clone()), answer));
        gate
    }

    pub fn originates(&self) -> Vec<(DelegatedPlatformContext, Value)> {
        self.state.lock().unwrap().originates.clone()
    }

    /// Distinct accepted executions (paid effects).
    pub fn executions(&self) -> Vec<(String, Uuid, Value)> {
        self.state.lock().unwrap().accepted.clone()
    }

    pub fn controls(&self) -> Vec<(Uuid, LegControl, Value)> {
        self.state.lock().unwrap().controls.clone()
    }

    pub fn bridges(&self) -> Vec<Value> {
        self.state.lock().unwrap().bridges.clone()
    }

    pub fn ended_bridges(&self) -> Vec<Uuid> {
        self.state.lock().unwrap().ended_bridges.clone()
    }

    /// The Telephony leg id of the accepted originate for `call_leg_id`.
    pub fn telephony_leg_of(&self, call_leg_id: Uuid) -> Option<Uuid> {
        self.state
            .lock()
            .unwrap()
            .accepted
            .iter()
            .find(|(_, _, v)| v["call"]["callLegId"] == json!(call_leg_id))
            .map(|(_, t, _)| *t)
    }
}

#[async_trait]
impl TelephonyPort for FakeTelephony {
    async fn availability(
        &self,
        _: &str,
        _: RouteCapability,
        _: &CorrelationId,
    ) -> Evidence<Vec<CandidateRoute>> {
        self.state
            .lock()
            .unwrap()
            .routes
            .clone()
            .unwrap_or(Evidence::Unknown)
    }

    async fn originate(
        &self,
        delegated: &DelegatedPlatformContext,
        _: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError> {
        let mut s = self.state.lock().unwrap();
        s.originates.push((delegated.clone(), command.clone()));
        let key = command["idempotencyKey"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // Telephony's idempotency: a replay answers with the same leg, never a new one.
        if let Some((_, leg, _)) = s.accepted.iter().find(|(k, _, _)| *k == key) {
            return Ok(
                json!({ "replayed": true, "leg": { "telephonyLegId": leg, "status": "ORIGINATING" } }),
            );
        }
        let answer = if s.answers.is_empty() {
            Answer::Accept
        } else {
            s.answers.remove(0)
        };
        match answer {
            Answer::Accept | Answer::AcceptButLoseResponse => {
                let leg = Uuid::new_v4();
                let mut stored = command.clone();
                stored["destination"]
                    .as_object_mut()
                    .map(|d| d.remove("grantSecret"));
                s.accepted.push((key, leg, stored));
                if answer == Answer::AcceptButLoseResponse {
                    return Err(TelephonyError::Unavailable);
                }
                Ok(
                    json!({ "commandId": command["commandId"], "leg": { "telephonyLegId": leg, "status": "PLANNED" } }),
                )
            }
            Answer::Refuse(code) => Err(TelephonyError::Refused {
                status: 409,
                code: code.into(),
            }),
            Answer::Unavailable => Err(TelephonyError::Unavailable),
        }
    }

    async fn control(
        &self,
        telephony_leg_id: Uuid,
        action: LegControl,
        _: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError> {
        let (gate, answer) = {
            let mut s = self.state.lock().unwrap();
            s.controls.push((telephony_leg_id, action, command.clone()));
            if s.control_answers.is_empty() {
                (None, Answer::Accept)
            } else {
                s.control_answers.remove(0)
            }
        };
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        match answer {
            Answer::Accept => Ok(json!({ "leg": { "telephonyLegId": telephony_leg_id } })),
            Answer::Refuse(code) => Err(TelephonyError::Refused {
                status: 409,
                code: code.into(),
            }),
            Answer::Unavailable | Answer::AcceptButLoseResponse => Err(TelephonyError::Unavailable),
        }
    }

    async fn create_bridge(
        &self,
        _: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError> {
        self.state.lock().unwrap().bridges.push(command.clone());
        let id = Uuid::new_v4();
        let code = format!("cfmj{}", Uuid::new_v4().simple());
        Ok(json!({ "bridge": { "bridgeId": id, "status": "PENDING", "mediaJoinCode": code } }))
    }

    async fn end_bridge(&self, bridge_id: Uuid, _: &CorrelationId) -> Result<(), TelephonyError> {
        self.state.lock().unwrap().ended_bridges.push(bridge_id);
        Ok(())
    }
}

/// Records media attachments (room, identity, join code).
#[derive(Default)]
pub struct FakePublicMedia {
    pub attached: Mutex<Vec<(String, String, String)>>,
    pub fail: Mutex<bool>,
}

#[async_trait]
impl PublicMediaGateway for FakePublicMedia {
    async fn attach(
        &self,
        room: &str,
        identity: &str,
        join_code: &str,
    ) -> Result<(), MediaProviderError> {
        if *self.fail.lock().unwrap() {
            return Err(MediaProviderError::Unavailable);
        }
        self.attached
            .lock()
            .unwrap()
            .push((room.into(), identity.into(), join_code.into()));
        Ok(())
    }
}

/// A Telephony event as Telephony's outbox would deliver it.
pub fn leg_event(
    event_type: &str,
    call: Uuid,
    call_leg: Uuid,
    telephony_leg: Uuid,
    attempt: Option<Uuid>,
    sequence: i64,
) -> Value {
    let mut v = json!({
        "schemaVersion": "1",
        "eventId": Uuid::new_v4(),
        "eventType": event_type,
        "occurredAt": chrono::Utc::now(),
        "telephonyLegId": telephony_leg,
        "callId": call,
        "callLegId": call_leg,
        "sequence": sequence,
        "direction": "OUTBOUND",
        "routeClass": "APP_TO_PSTN",
    });
    if let Some(a) = attempt {
        v["attemptId"] = json!(a);
    }
    v
}
