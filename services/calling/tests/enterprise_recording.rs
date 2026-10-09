//! Part F §61 call recording on ONE canonical Call (application / RTC path through the
//! neutral call media capture interface; the SIP path is certified against real Asterisk,
//! SIPp and an S3-compatible store in `enterprise_sip_certification`): a separate typed
//! policy, immutable server-authoritative decisions, notice issued by the server,
//! each participant's own consent bound to ONE Call, idempotent start, stops (explicit,
//! Call end, revocation, withdrawal, maximum length), protected artifacts, audited
//! access including denials, AUTO_WHEN_AUTHORIZED only with evidence, failures.

mod common;

use std::sync::{atomic::Ordering, Arc};

use axum::http::{Method, StatusCode};
use calling::{
    calls::{app::enterprise::CapturedMedia, CallState},
    enterprise::{policy::AdminCapability, EnterpriseFlags},
    platform::scopes::names,
};
use cf_object_storage::MemoryObjectStore;
use common::{
    app::Device,
    enterprise::{conforms, credential, id_of, strings, EnterpriseWorld, RecordingCapture},
    TestResponse,
};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

type Principal = calling::platform::principals::ResolvedPrincipal;

fn tag() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .map(|c| {
            if c.is_ascii_digit() {
                (b'g' + (c as u8 - b'0')) as char
            } else {
                c
            }
        })
        .collect()
}

fn wav(ms: u32) -> Vec<u8> {
    let data_len = ms * 16;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8000u32.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend((0..data_len).map(|i| (i % 13) as u8));
    out
}

struct Desk {
    principal: Principal,
    phone: Device,
}

/// A recording world: memory storage, a recording RTC capture adapter, the operator
/// grant of MANAGE_RECORDING, a recording administrator and a recording reader.
struct Rec {
    e: EnterpriseWorld,
    store: Arc<MemoryObjectStore>,
    capture: Arc<RecordingCapture>,
    admin: calling::platform::credentials::IssuedCredential,
    reader: calling::platform::credentials::IssuedCredential,
}

impl Rec {
    async fn new() -> Option<Self> {
        Self::with_flags(EnterpriseFlags::ALL_ON).await
    }

    async fn with_flags(flags: EnterpriseFlags) -> Option<Self> {
        let store = Arc::new(MemoryObjectStore::new());
        let capture = Arc::new(RecordingCapture::default());
        let (s, c) = (store.clone(), capture.clone());
        let e = EnterpriseWorld::configured(move |runtime| {
            runtime.enterprise_flags = flags;
            runtime.artifacts = s;
            runtime.capture = c;
        })
        .await?;
        e.h()
            .state
            .enterprise
            .operator_set_capabilities(
                e.a.w.scope(),
                vec![
                    AdminCapability::ManageRouting,
                    AdminCapability::ManageRecording,
                ],
            )
            .await
            .unwrap();
        let admin = credential(
            e.h(),
            &e.a.w.consumer,
            e.a.w.kind,
            &[
                names::ENTERPRISE_CONFIG_READ,
                names::ENTERPRISE_RECORDING_READ,
                names::ENTERPRISE_RECORDING_MANAGE,
            ],
        )
        .await;
        let reader = credential(
            e.h(),
            &e.a.w.consumer,
            e.a.w.kind,
            &[names::ENTERPRISE_RECORDING_READ],
        )
        .await;
        Some(Self {
            e,
            store,
            capture,
            admin,
            reader,
        })
    }

    async fn manage(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.e
            .send(self.admin.secret.expose_once(), method, path, body)
            .await
    }

    async fn read(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.e
            .send(self.reader.secret.expose_once(), method, path, body)
            .await
    }

    async fn policy(&self, mode: &str, notice: bool, consent: &str, max: i64) -> Value {
        let r = self
            .manage(
                Method::POST,
                "/v1/enterprise/recording-policies",
                Some(
                    json!({ "name": format!("Recording {}", tag()), "mode": mode,
                             "noticeRequired": notice, "consentParticipants": consent,
                             "jurisdictionRef": "NG-NDPA-2023/v2", "retentionDays": 30,
                             "maxRecordingSeconds": max, "isDefault": true }),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        conforms("RecordingPolicy", &r.json);
        r.json
    }

    async fn desk(&self, name: &str) -> Desk {
        let principal = self.e.member(&format!("{name}-{}", tag())).await;
        let phone = self.e.a.device(&principal, &format!("{name}-desk")).await;
        Desk { principal, phone }
    }

    /// A CONNECTED enterprise Call: `caller` dials `number` (`callee`'s extension).
    async fn connected(&self, caller: &Desk, number: &str, callee: &Desk) -> Uuid {
        let e = &self.e;
        let r = e
            .call_extension(
                "VOICE",
                &caller.principal,
                &caller.phone,
                number,
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let inv =
            e.a.invitation_for(call, callee.phone.endpoint)
                .await
                .expect("callee rings");
        let (_, v) = e.a.state(call).await;
        let r = e.a.accept(call, inv, &callee.phone, v).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        for d in [&caller.phone, &callee.phone] {
            let s = e.a.session_on(call, d.endpoint).await.unwrap();
            e.a.connect(call, s).await;
        }
        assert_eq!(e.a.state(call).await.0, CallState::Connected);
        call
    }

    async fn start(&self, call: Uuid, principal: Option<Uuid>) -> TestResponse {
        let body = match principal {
            Some(p) => json!({ "principalId": p }),
            None => json!({}),
        };
        self.manage(
            Method::POST,
            &format!("/v1/calls/{call}/recordings"),
            Some(body),
        )
        .await
    }

    async fn consent(&self, call: Uuid, desk: &Desk, decision: &str) -> TestResponse {
        self.e
            .send(
                self.e.caller_token(),
                Method::POST,
                &format!("/v1/calls/{call}/recording-consent"),
                Some(json!({ "endpointId": desk.phone.endpoint, "decision": decision })),
            )
            .await
    }

    async fn decisions(&self, call: Uuid) -> Vec<Value> {
        let r = self
            .manage(
                Method::GET,
                &format!("/v1/calls/{call}/recording-authorizations"),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        conforms("RecordingAuthorizationList", &r.json);
        r.json["authorizations"].as_array().unwrap().clone()
    }

    async fn last_reason(&self, call: Uuid) -> String {
        let d = self.decisions(call).await;
        let last = d.last().expect("a decision");
        format!(
            "{}:{}",
            last["decision"].as_str().unwrap(),
            last["reason"].as_str().unwrap()
        )
    }

    async fn recording_state(&self, id: Uuid) -> String {
        strings(
            self.e.h().conn(),
            &format!(
                "SELECT state || ':' || COALESCE(stop_reason, '') || ':' || COALESCE(failure_code, '') \
                   FROM call_recordings WHERE id = '{id}'"
            ),
        )
        .await
        .pop()
        .unwrap()
    }

    async fn events(&self, call: Uuid) -> Vec<String> {
        for text in strings(
            self.e.h().conn(),
            &format!(
                "SELECT envelope::text FROM calling_outbox_events \
                  WHERE event_type LIKE 'calling.recording-%' AND envelope::text LIKE '%{call}%'"
            ),
        )
        .await
        {
            let envelope: Value = serde_json::from_str(&text).unwrap();
            common::contracts::check("platform/events/envelope/v1.schema.json", &envelope);
            common::contracts::check(
                "calling/events/call-lifecycle/v1.schema.json",
                &envelope["payload"],
            );
            assert!(!text.contains("recordings/"), "{text}");
        }
        strings(
            self.e.h().conn(),
            &format!(
                "SELECT event_type FROM call_events WHERE call_id = '{call}' \
                   AND event_type LIKE 'RECORDING_%' ORDER BY sequence"
            ),
        )
        .await
    }

    async fn tick(&self) {
        self.e.h().state.app.expire_due(100).await.unwrap();
    }

    async fn deliver(&self, recording: Uuid, call: Uuid, bytes: &[u8]) {
        self.e
            .h()
            .state
            .app
            .deliver_capture(
                recording,
                call,
                CapturedMedia {
                    content_type: "audio/wav".into(),
                    duration_ms: 2000,
                    sha256: cf_object_storage::sha256_hex(bytes),
                    bytes: bytes.to_vec(),
                },
            )
            .await
            .unwrap();
    }
}

fn recording_id(r: &TestResponse) -> Uuid {
    id_of(&r.json["recording"], "recordingId")
}

#[tokio::test]
async fn recording_policy_administration() {
    let Some(rec) = Rec::new().await else {
        return;
    };
    let e = &rec.e;
    // Validation: typed modes, bounded limits, a jurisdiction reference that is never a
    // number, no unknown fields (no "everyone consented" shortcut).
    let base = json!({ "name": "Support", "mode": "MANUAL_ALLOWED", "noticeRequired": true,
                       "consentParticipants": "OTHER_PARTICIPANTS", "jurisdictionRef": "NG-NDPA-2023/v2",
                       "retentionDays": 30, "maxRecordingSeconds": 600 });
    for (key, value) in [
        ("mode", json!("ALWAYS")),
        ("jurisdictionRef", json!("+2348012345678")),
        ("jurisdictionRef", json!("08012345678")),
        ("maxRecordingSeconds", json!(5000)),
        ("retentionDays", json!(0)),
        ("consentParticipants", json!("SOME")),
        ("everyoneConsented", json!(true)),
    ] {
        let mut body = base.clone();
        body[key] = value;
        let r = rec
            .manage(
                Method::POST,
                "/v1/enterprise/recording-policies",
                Some(body.clone()),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text);
    }
    // Configuration administrators hold no recording scope.
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/recording-policies",
            Some(base.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let first = rec
        .policy("MANUAL_ALLOWED", true, "OTHER_PARTICIPANTS", 600)
        .await;
    assert_eq!(first["isDefault"], true);
    assert_eq!(first["consentRequired"], true);
    let second = rec.policy("DISABLED", false, "NONE", 600).await;
    assert_eq!(second["consentRequired"], false);
    // One default: the first stopped being it (versioned).
    let r = e
        .admin(
            Method::GET,
            &format!(
                "/v1/enterprise/recording-policies/{}",
                id_of(&first, "recordingPolicyId")
            ),
            None,
        )
        .await;
    assert_eq!(r.json["isDefault"], false);
    assert_eq!(r.json["version"], 2);
    let r = e
        .admin(Method::GET, "/v1/enterprise/recording-policies", None)
        .await;
    conforms("RecordingPolicyList", &r.json);
    assert_eq!(r.json["recordingPolicies"].as_array().unwrap().len(), 2);
    // Stale versions conflict; disabling is audited.
    let path = format!(
        "/v1/enterprise/recording-policies/{}",
        id_of(&second, "recordingPolicyId")
    );
    let r = rec
        .manage(
            Method::PATCH,
            &path,
            Some(json!({ "expectedVersion": 9, "mode": "MANUAL_ALLOWED" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = rec
        .manage(Method::DELETE, &format!("{path}?expectedVersion=1"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DISABLED");
    // Without the operator-granted capability, nobody manages recording policies.
    e.h()
        .state
        .enterprise
        .operator_set_capabilities(e.a.w.scope(), vec![AdminCapability::ManageRouting])
        .await
        .unwrap();
    let r = rec
        .manage(
            Method::POST,
            "/v1/enterprise/recording-policies",
            Some(base.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "ENTERPRISE_POLICY_DENIED");
    assert_eq!(
        e.count(
            "SELECT count(*) FROM enterprise_admin_audit WHERE resource_type = 'RECORDING_POLICY'"
        )
        .await,
        4
    );
}

/// Permission to call is never permission to record: every refusal is an immutable,
/// reasoned decision, and the Call itself is untouched.
#[tokio::test]
async fn permission_to_call_is_not_permission_to_record() {
    let Some(rec) = Rec::new().await else {
        return;
    };
    let e = &rec.e;
    let caller = rec.desk("caller").await;
    let callee = rec.desk("callee").await;
    let stranger = rec.desk("stranger").await;
    e.extension("4210", "ENTERPRISE_MEMBER", callee.principal.id())
        .await;
    let call = rec.connected(&caller, "4210", &callee).await;
    // No policy at all.
    let r = rec.start(call, Some(caller.principal.id().as_uuid())).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "RECORDING_NOT_AUTHORIZED");
    assert_eq!(rec.last_reason(call).await, "DENIED:NO_RECORDING_POLICY");
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
    // A DISABLED policy.
    let p = rec.policy("DISABLED", false, "NONE", 600).await;
    rec.start(call, None).await;
    assert_eq!(rec.last_reason(call).await, "DENIED:POLICY_DISABLED");
    // MANUAL_ALLOWED: a non-participant principal may not start it.
    let r = rec
        .manage(
            Method::PATCH,
            &format!(
                "/v1/enterprise/recording-policies/{}",
                id_of(&p, "recordingPolicyId")
            ),
            Some(json!({ "expectedVersion": 1, "mode": "MANUAL_ALLOWED" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = rec
        .start(call, Some(stranger.principal.id().as_uuid()))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(rec.last_reason(call).await, "DENIED:ACTOR_NOT_PARTICIPANT");
    // A ringing Call is not eligible.
    let ringing = e
        .call_extension(
            "VOICE",
            &caller.principal,
            &caller.phone,
            "4210",
            &Uuid::new_v4().to_string(),
        )
        .await;
    let ringing: Uuid = ringing.json["call"]["callId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    rec.start(ringing, None).await;
    assert_eq!(
        rec.last_reason(ringing).await,
        "DENIED:CALL_STATE_NOT_ELIGIBLE"
    );
    // An ordinary (Part C) Call is not an enterprise Call.
    let (plain, _, _) = e.a.connected_call("VOICE").await;
    rec.start(plain, None).await;
    assert_eq!(
        rec.last_reason(plain).await,
        "DENIED:NOT_AN_ENTERPRISE_CALL"
    );
    // Another tenant's Call does not exist for this credential.
    let (other, _) = e.other_tenant("globex").await;
    let other = credential(
        e.h(),
        &other,
        e.a.w.kind,
        &[names::ENTERPRISE_RECORDING_MANAGE],
    )
    .await;
    let r = e
        .send(
            other.secret.expose_once(),
            Method::POST,
            &format!("/v1/calls/{call}/recordings"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // A reader cannot start; nothing was recorded; decisions are immutable.
    let r = rec
        .read(
            Method::POST,
            &format!("/v1/calls/{call}/recordings"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(e.count("SELECT count(*) FROM call_recordings").await, 0);
    assert!(e
        .h()
        .conn()
        .execute_unprepared(
            "UPDATE call_recording_authorizations SET decision = 'ALLOWED', reason = 'AUTHORIZED'"
        )
        .await
        .is_err());
}

/// The kill switch refuses NEW recordings.
#[tokio::test]
async fn kill_switch_refuses_new_recordings() {
    let mut flags = EnterpriseFlags::ALL_ON;
    flags.call_recording = false;
    let Some(rec) = Rec::with_flags(flags).await else {
        return;
    };
    let caller = rec.desk("caller").await;
    let callee = rec.desk("callee").await;
    rec.e
        .extension("4220", "ENTERPRISE_MEMBER", callee.principal.id())
        .await;
    rec.policy("MANUAL_ALLOWED", false, "NONE", 600).await;
    let call = rec.connected(&caller, "4220", &callee).await;
    let r = rec.start(call, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(rec.last_reason(call).await, "DENIED:RECORDING_DISABLED");
    assert!(rec.capture.started().is_empty());
}

/// Consent is each required participant's OWN decision for THIS Call: missing, declined,
/// replayed, from another Call — then all evidence, an idempotent start, a withdrawal.
#[tokio::test]
async fn consent_is_each_participants_own_and_bound_to_the_call() {
    let Some(rec) = Rec::new().await else {
        return;
    };
    let e = &rec.e;
    let caller = rec.desk("caller").await;
    let callee = rec.desk("callee").await;
    e.extension("4230", "ENTERPRISE_MEMBER", callee.principal.id())
        .await;
    rec.policy("MANUAL_ALLOWED", true, "ALL_PARTICIPANTS", 600)
        .await;
    let call = rec.connected(&caller, "4230", &callee).await;
    let other_call = rec.connected(&caller, "4230", &callee).await;
    let caller_id = caller.principal.id().as_uuid();
    // Nobody consented.
    let r = rec.start(call, Some(caller_id)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let d = rec.decisions(call).await;
    let last = d.last().unwrap();
    assert_eq!(last["reason"], "CONSENT_MISSING");
    assert_eq!(last["requiredParticipantIds"].as_array().unwrap().len(), 2);
    assert_eq!(last["notice"]["required"], true);
    // Consent on ANOTHER Call does not count here (no replay across Calls).
    for d in [&caller, &callee] {
        let r = rec.consent(other_call, d, "GRANTED").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        conforms("RecordingConsent", &r.json);
    }
    rec.start(call, Some(caller_id)).await;
    assert_eq!(rec.last_reason(call).await, "DENIED:CONSENT_MISSING");
    // An endpoint that is not a participant's of this Call cannot consent.
    let outsider = rec.desk("outsider").await;
    let r = rec.consent(call, &outsider, "GRANTED").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // The caller grants, the callee declines.
    let granted = rec.consent(call, &caller, "GRANTED").await;
    assert_eq!(granted.status, StatusCode::OK, "{}", granted.text);
    // Replaying the same decision is idempotent.
    let replay = rec.consent(call, &caller, "GRANTED").await;
    assert_eq!(replay.json["consentId"], granted.json["consentId"]);
    rec.consent(call, &callee, "DECLINED").await;
    rec.start(call, Some(caller_id)).await;
    assert_eq!(rec.last_reason(call).await, "DENIED:CONSENT_DECLINED");
    // A late decision only counts for later decisions: the callee now grants.
    rec.consent(call, &callee, "GRANTED").await;
    let r = rec.start(call, Some(caller_id)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms("CallRecordingEnvelope", &r.json);
    let recording = recording_id(&r);
    assert_eq!(r.json["recording"]["state"], "RECORDING");
    assert_eq!(r.json["recording"]["captureSource"], "APPLICATION");
    let d = rec.decisions(call).await;
    let allowed = d.last().unwrap();
    assert_eq!(allowed["decision"], "ALLOWED");
    assert_eq!(allowed["actor"]["kind"], "PRINCIPAL");
    assert_eq!(
        allowed["notice"]["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["channel"] == "APPLICATION_SIGNAL")
            .count(),
        2
    );
    assert!(allowed["consent"]["participants"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["decision"] == "GRANTED"));
    let started = rec.capture.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].capture_id, recording);
    // Idempotent start: the running recording, no new decision.
    let count = rec.decisions(call).await.len();
    let r = rec.start(call, Some(caller_id)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(recording_id(&r), recording);
    assert_eq!(rec.decisions(call).await.len(), count);
    // The callee withdraws: the recording stops (CONSENT_WITHDRAWN).
    rec.consent(call, &callee, "DECLINED").await;
    assert_eq!(
        rec.recording_state(recording).await,
        "STOPPING:CONSENT_WITHDRAWN:"
    );
    assert_eq!(rec.capture.stopped(), [recording]);
    assert_eq!(
        rec.events(call).await,
        ["RECORDING_STARTED", "RECORDING_STOPPED"]
    );
    // Consent rows are append-only.
    assert!(e
        .h()
        .conn()
        .execute_unprepared("UPDATE call_recording_consents SET decision = 'GRANTED'")
        .await
        .is_err());
}

/// Call end stops the capture; the delivered audio is a protected artifact; access is
/// authorized FIRST and audited (also when refused); deletion and retention.
#[tokio::test]
async fn call_end_stops_and_artifacts_are_protected_and_audited() {
    let Some(rec) = Rec::new().await else {
        return;
    };
    let e = &rec.e;
    let caller = rec.desk("caller").await;
    let callee = rec.desk("callee").await;
    let stranger = rec.desk("stranger").await;
    e.extension("4240", "ENTERPRISE_MEMBER", callee.principal.id())
        .await;
    rec.policy("MANUAL_ALLOWED", false, "NONE", 600).await;
    let call = rec.connected(&caller, "4240", &callee).await;
    // An administrator starts it (no principal).
    let r = rec.start(call, None).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let recording = recording_id(&r);
    assert_eq!(
        rec.decisions(call).await.last().unwrap()["actor"]["kind"],
        "ADMINISTRATOR"
    );
    // The Call ends: the recording stops (CALL_ENDED) and is finalized.
    let (_, v) = e.a.state(call).await;
    let r = e.a.command(call, "end", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    rec.tick().await;
    assert_eq!(rec.recording_state(recording).await, "STOPPING:CALL_ENDED:");
    assert!(rec.capture.stopped().contains(&recording));
    let audio = wav(2000);
    rec.deliver(recording, call, &audio).await;
    assert_eq!(rec.recording_state(recording).await, "READY:CALL_ENDED:");
    assert_eq!(
        rec.events(call).await,
        ["RECORDING_STARTED", "RECORDING_STOPPED", "RECORDING_READY"]
    );
    let keys = rec.store.keys();
    assert_eq!(keys.len(), 1);
    assert!(keys[0].starts_with("recordings/"));
    assert_eq!(rec.store.get_raw(&keys[0]).unwrap(), audio);
    // Metadata: integrity only — no key, URL or number.
    let owner = caller.principal.id().as_uuid();
    let r = rec
        .read(
            Method::GET,
            &format!("/v1/enterprise/recordings/{recording}?principalId={owner}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("CallRecording", &r.json);
    assert_eq!(r.json["sizeBytes"], audio.len());
    assert!(!r.text.contains("recordings/") && !r.text.contains("+234"));
    let list = rec
        .read(
            Method::GET,
            &format!("/v1/calls/{call}/recordings?principalId={owner}"),
            None,
        )
        .await;
    conforms("CallRecordingList", &list.json);
    assert_eq!(list.json["recordings"].as_array().unwrap().len(), 1);
    // Media access: a participant, a short-lived URL that names no key.
    let r = rec
        .read(
            Method::POST,
            &format!("/v1/enterprise/recordings/{recording}/media-access"),
            Some(json!({ "principalId": owner })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("RecordingMediaAccess", &r.json);
    assert!(!r.json["url"].as_str().unwrap().contains("recordings/"));
    // A non-participant: 404, audited DENIED. Another tenant: 404.
    let r = rec
        .read(
            Method::POST,
            &format!("/v1/enterprise/recordings/{recording}/media-access"),
            Some(json!({ "principalId": stranger.principal.id() })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = rec
        .read(
            Method::GET,
            &format!(
                "/v1/calls/{call}/recordings?principalId={}",
                stranger.principal.id()
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let (other, _) = e.other_tenant("initech").await;
    let other = credential(
        e.h(),
        &other,
        e.a.w.kind,
        &[
            names::ENTERPRISE_RECORDING_READ,
            names::ENTERPRISE_RECORDING_MANAGE,
        ],
    )
    .await;
    for (method, path, body) in [
        (
            Method::GET,
            format!("/v1/enterprise/recordings/{recording}"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/enterprise/recordings/{recording}/media-access"),
            Some(json!({})),
        ),
        (Method::GET, format!("/v1/calls/{call}/recordings"), None),
        (
            Method::DELETE,
            format!("/v1/enterprise/recordings/{recording}?expectedVersion=1"),
            None,
        ),
    ] {
        let r = e
            .send(other.secret.expose_once(), method, &path, body)
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {}", r.text);
    }
    // A reader without a principal is not an administrator.
    let r = rec
        .read(
            Method::POST,
            &format!("/v1/enterprise/recordings/{recording}/media-access"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // The audit trail: who, what, outcome.
    let r = rec
        .manage(
            Method::GET,
            &format!("/v1/enterprise/recordings/{recording}/access-audit"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("RecordingAccessList", &r.json);
    let trail: Vec<String> = r.json["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            format!(
                "{}:{}",
                x["operation"].as_str().unwrap(),
                x["outcome"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        trail,
        [
            "READ_METADATA:GRANTED",
            "MEDIA_ACCESS:GRANTED",
            "MEDIA_ACCESS:DENIED"
        ]
    );
    // Deletion: DELETING -> object removed -> DELETED; the Call's history remains.
    let version = rec
        .manage(
            Method::GET,
            &format!("/v1/enterprise/recordings/{recording}"),
            None,
        )
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let r = rec
        .manage(
            Method::DELETE,
            &format!("/v1/enterprise/recordings/{recording}?expectedVersion={version}&principalId={owner}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "DELETING");
    rec.tick().await;
    assert_eq!(rec.recording_state(recording).await, "DELETED:CALL_ENDED:");
    assert!(rec.store.is_empty());
    assert_eq!(rec.events(call).await.last().unwrap(), "RECORDING_DELETED");
    assert!(matches!(
        e.a.state(call).await.0,
        CallState::Ended | CallState::Ending
    ));

    // Retention (30 days) deletes as the system.
    let call = rec.connected(&caller, "4240", &callee).await;
    let recording = recording_id(&rec.start(call, None).await);
    let (_, v) = e.a.state(call).await;
    e.a.command(call, "end", v).await;
    rec.tick().await;
    rec.deliver(recording, call, &wav(500)).await;
    e.h().clock.advance(chrono::Duration::days(31));
    rec.tick().await;
    assert_eq!(rec.recording_state(recording).await, "DELETED:CALL_ENDED:");
    assert_eq!(
        strings(
            e.h().conn(),
            &format!(
                "SELECT actor_kind || ':' || operation FROM call_recording_access_audit \
                  WHERE recording_id = '{recording}' ORDER BY seq"
            ),
        )
        .await,
        ["SYSTEM:RETENTION_DELETE"]
    );
    // Nothing in any recording row looks like a telephone number or holds a URL.
    let dump = strings(
        e.h().conn(),
        "SELECT row_to_json(t)::text FROM call_recordings t UNION ALL \
         SELECT row_to_json(a)::text FROM call_recording_authorizations a",
    )
    .await
    .join("\n");
    assert!(!dump.contains("+234") && !dump.contains("http"), "{dump}");
}

/// Explicit stop (participants / administrators), policy revocation, maximum length,
/// capture failure and storage outage.
#[tokio::test]
async fn stops_revocation_deadlines_and_failures() {
    let Some(rec) = Rec::new().await else {
        return;
    };
    let e = &rec.e;
    let caller = rec.desk("caller").await;
    let callee = rec.desk("callee").await;
    let stranger = rec.desk("stranger").await;
    e.extension("4250", "ENTERPRISE_MEMBER", callee.principal.id())
        .await;
    let policy = rec.policy("MANUAL_ALLOWED", false, "NONE", 60).await;
    // Explicit stop by a participant; a stranger cannot; a second stop conflicts.
    let call = rec.connected(&caller, "4250", &callee).await;
    let recording = recording_id(&rec.start(call, Some(caller.principal.id().as_uuid())).await);
    let stop_path = format!("/v1/calls/{call}/recordings/{recording}/stop");
    let stop = |p: Option<Uuid>| {
        let body = p.map(|p| json!({ "principalId": p })).unwrap_or(json!({}));
        rec.manage(Method::POST, &stop_path, Some(body))
    };
    let r = stop(Some(stranger.principal.id().as_uuid())).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = stop(Some(callee.principal.id().as_uuid())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["recording"]["state"], "STOPPING");
    assert_eq!(r.json["recording"]["stopReason"], "EXPLICIT");
    let r = stop(None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(
        strings(
            e.h().conn(),
            &format!(
                "SELECT operation || ':' || outcome FROM call_recording_access_audit \
                  WHERE recording_id = '{recording}' ORDER BY seq"
            ),
        )
        .await,
        ["STOP:DENIED", "STOP:GRANTED"]
    );
    // The maximum length (60 s) stops a recording.
    let (_, v) = e.a.state(call).await;
    e.a.command(call, "end", v).await;
    let call = rec.connected(&caller, "4250", &callee).await;
    let recording = recording_id(&rec.start(call, None).await);
    e.h().clock.advance(chrono::Duration::seconds(61));
    rec.tick().await;
    assert_eq!(
        rec.recording_state(recording).await,
        "STOPPING:MAX_DURATION:"
    );
    // Storage outage at delivery: PROCESSING, never a fake READY; then recovered.
    rec.store.set_available(false);
    let audio = wav(800);
    let err = e
        .h()
        .state
        .app
        .deliver_capture(
            recording,
            call,
            CapturedMedia {
                content_type: "audio/wav".into(),
                duration_ms: 800,
                sha256: cf_object_storage::sha256_hex(&audio),
                bytes: audio.clone(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            calling::calls::errors::CallError::ArtifactStorageUnavailable
        ),
        "{err:?}"
    );
    assert_eq!(
        rec.recording_state(recording).await,
        "PROCESSING:MAX_DURATION:STORAGE_UNAVAILABLE"
    );
    rec.store.set_available(true);
    rec.deliver(recording, call, &audio).await;
    assert_eq!(rec.recording_state(recording).await, "READY:MAX_DURATION:");
    // Disabling the policy stops running recordings (POLICY_REVOKED).
    let (_, v) = e.a.state(call).await;
    e.a.command(call, "end", v).await;
    let call = rec.connected(&caller, "4250", &callee).await;
    let recording = recording_id(&rec.start(call, None).await);
    let version = e
        .admin(
            Method::GET,
            &format!(
                "/v1/enterprise/recording-policies/{}",
                id_of(&policy, "recordingPolicyId")
            ),
            None,
        )
        .await
        .json["version"]
        .clone();
    let r = rec
        .manage(
            Method::DELETE,
            &format!(
                "/v1/enterprise/recording-policies/{}?expectedVersion={version}",
                id_of(&policy, "recordingPolicyId")
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        rec.recording_state(recording).await,
        "STOPPING:POLICY_REVOKED:"
    );
    // A capture adapter that refuses fails the recording; the Call is unaffected.
    rec.policy("MANUAL_ALLOWED", false, "NONE", 600).await;
    let (_, v) = e.a.state(call).await;
    e.a.command(call, "end", v).await;
    let call = rec.connected(&caller, "4250", &callee).await;
    rec.capture.refuse.store(true, Ordering::SeqCst);
    let r = rec.start(call, None).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["recording"]["state"], "FAILED");
    assert_eq!(r.json["recording"]["failureCode"], "CAPTURE_START_FAILED");
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
    assert!(rec.events(call).await.ends_with(&[
        "RECORDING_STARTED".to_string(),
        "RECORDING_FAILED".to_string()
    ]));
}

/// AUTO_WHEN_AUTHORIZED records by itself only once the evidence is there; MANUAL never
/// does.
#[tokio::test]
async fn auto_when_authorized_records_only_with_evidence() {
    let Some(rec) = Rec::new().await else {
        return;
    };
    let e = &rec.e;
    let caller = rec.desk("caller").await;
    let callee = rec.desk("callee").await;
    e.extension("4260", "ENTERPRISE_MEMBER", callee.principal.id())
        .await;
    let manual = rec.policy("MANUAL_ALLOWED", false, "NONE", 600).await;
    let call = rec.connected(&caller, "4260", &callee).await;
    rec.tick().await;
    assert!(
        rec.decisions(call).await.is_empty(),
        "MANUAL never starts by itself"
    );
    let r = rec
        .manage(
            Method::PATCH,
            &format!(
                "/v1/enterprise/recording-policies/{}",
                id_of(&manual, "recordingPolicyId")
            ),
            Some(
                json!({ "expectedVersion": 1, "mode": "AUTO_WHEN_AUTHORIZED",
                         "consentParticipants": "ALL_PARTICIPANTS" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Evidence missing: one recorded SYSTEM denial, not one per tick.
    rec.tick().await;
    rec.tick().await;
    let d = rec.decisions(call).await;
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["actor"]["kind"], "SYSTEM");
    assert_eq!(d[0]["reason"], "CONSENT_MISSING");
    // Both consent: the platform starts recording.
    rec.consent(call, &caller, "GRANTED").await;
    assert_eq!(rec.last_reason(call).await, "DENIED:CONSENT_MISSING");
    rec.consent(call, &callee, "GRANTED").await;
    assert_eq!(rec.last_reason(call).await, "ALLOWED:AUTHORIZED");
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_recordings WHERE call_id = '{call}' AND state = 'RECORDING'"
        ))
        .await,
        1
    );
    rec.tick().await;
    assert_eq!(rec.decisions(call).await.len(), 3);
}

/// The kill switch stops NEW recordings only: an existing recording can still be stopped,
/// finalized into protected storage and deleted.
#[tokio::test]
async fn kill_switch_never_blocks_stop_finalize_or_delete() {
    let mut flags = EnterpriseFlags::ALL_ON;
    flags.call_recording = false;
    let Some(rec) = Rec::with_flags(flags).await else {
        return;
    };
    let e = &rec.e;
    let caller = rec.desk("caller").await;
    let callee = rec.desk("callee").await;
    e.extension("4270", "ENTERPRISE_MEMBER", callee.principal.id())
        .await;
    let policy = id_of(
        &rec.policy("MANUAL_ALLOWED", false, "NONE", 600).await,
        "recordingPolicyId",
    );
    let call = rec.connected(&caller, "4270", &callee).await;
    // A recording authorized before the switch was turned off.
    let (authorization, recording) = (Uuid::new_v4(), Uuid::new_v4());
    let scope = e.a.w.scope();
    let (t, a, v) = (
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        scope.environment_id().as_uuid(),
    );
    e.h()
        .conn()
        .execute_unprepared(&format!(
            "INSERT INTO call_recording_authorizations (id, tenant_id, application_id, environment_id, call_id, \
               policy_id, policy_version, mode, jurisdiction_ref, actor_kind, required_participant_ids, \
               notice_evidence, consent_evidence, consent_watermark, decision, reason, request_id, \
               correlation_id, created_at) \
             VALUES ('{authorization}', '{t}', '{a}', '{v}', '{call}', '{policy}', 1, 'MANUAL_ALLOWED', \
               'NG-NDPA-2023/v2', 'SYSTEM', '{{}}', '{{\"required\": false}}', \
               '{{\"required\": false, \"participants\": []}}', 0, 'ALLOWED', 'AUTHORIZED', \
               'req_00000000000000000000000000000000', 'corr_0000000000000000000000000000000', now()); \
             INSERT INTO call_recordings (id, tenant_id, application_id, environment_id, call_id, authorization_id, \
               policy_id, capture_source, state, started_at, recording_deadline, delete_attempts, version, \
               created_at, updated_at) \
             VALUES ('{recording}', '{t}', '{a}', '{v}', '{call}', '{authorization}', '{policy}', 'APPLICATION', \
               'RECORDING', now(), now() + interval '10 minutes', 0, 1, now(), now());"
        ))
        .await
        .unwrap();
    // New recordings are refused.
    let r = rec.start(call, None).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "the running recording is returned: {}",
        r.text
    );
    // Stop, finalize, delete all work.
    let r = rec
        .manage(
            Method::POST,
            &format!("/v1/calls/{call}/recordings/{recording}/stop"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(rec.capture.stopped(), [recording]);
    rec.deliver(recording, call, &wav(700)).await;
    assert_eq!(rec.recording_state(recording).await, "READY:EXPLICIT:");
    let r = rec
        .manage(
            Method::DELETE,
            &format!("/v1/enterprise/recordings/{recording}?expectedVersion=4"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    rec.tick().await;
    assert_eq!(rec.recording_state(recording).await, "DELETED:EXPLICIT:");
    assert!(rec.store.is_empty());
    // And a fresh Call cannot start one.
    let (_, version) = e.a.state(call).await;
    e.a.command(call, "end", version).await;
    let call = rec.connected(&caller, "4270", &callee).await;
    let r = rec.start(call, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(rec.last_reason(call).await, "DENIED:RECORDING_DISABLED");
}
