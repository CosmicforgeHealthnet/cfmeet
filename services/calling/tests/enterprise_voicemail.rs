//! Part F §60 voicemail on ONE canonical Call (application / RTC caller path through the
//! neutral call media capture interface; the SIP path is certified against real Asterisk,
//! SIPp and an S3-compatible store in `enterprise_sip_certification`): mailbox
//! administration, routing to a mailbox (no answer, queue overflow, after hours, IVR),
//! capture delivery into protected object storage, READY, authorized retrieval, read
//! state, deletion and retention, storage failure recovery, the kill switch and the
//! internal capture endpoint's authorization.
mod common;

use std::sync::{atomic::Ordering, Arc};

use axum::http::{Method, StatusCode};
use calling::{
    calls::{
        app::enterprise::{CaptureDelivery, CapturedMedia},
        errors::CallError,
        CallState,
    },
    enterprise::EnterpriseFlags,
    platform::scopes::names,
};
use cf_object_storage::MemoryObjectStore;
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use chrono::{DateTime, Utc};
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

fn dest(kind: &str, id: impl std::fmt::Display) -> Value {
    json!({ "type": kind, "id": id.to_string() })
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// A mono 16-bit 8 kHz WAV of `ms` milliseconds (a quiet tone, not silence).
fn wav(ms: u32) -> Vec<u8> {
    let samples = 8 * ms;
    let data_len = samples * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
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
    for i in 0..samples {
        let v = if (i / 10) % 2 == 0 { 800i16 } else { -800i16 };
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn media(bytes: &[u8], ms: i32) -> CapturedMedia {
    CapturedMedia {
        content_type: "audio/wav".into(),
        duration_ms: ms,
        sha256: cf_object_storage::sha256_hex(bytes),
        bytes: bytes.to_vec(),
    }
}

/// A voicemail world: protected storage in memory, a recording RTC capture adapter, an
/// administrator, a voicemail administrator and a voicemail reader.
struct Vm {
    e: EnterpriseWorld,
    store: Arc<MemoryObjectStore>,
    capture: Arc<RecordingCapture>,
    vm_admin: crate::common::IssuedCredential,
    vm_reader: crate::common::IssuedCredential,
}

impl Vm {
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
        let vm_admin = credential(
            e.h(),
            &e.a.w.consumer,
            e.a.w.kind,
            &[
                names::ENTERPRISE_CONFIG_READ,
                names::ENTERPRISE_VOICEMAIL_READ,
                names::ENTERPRISE_VOICEMAIL_MANAGE,
            ],
        )
        .await;
        let vm_reader = credential(
            e.h(),
            &e.a.w.consumer,
            e.a.w.kind,
            &[names::ENTERPRISE_VOICEMAIL_READ],
        )
        .await;
        Some(Self {
            e,
            store,
            capture,
            vm_admin,
            vm_reader,
        })
    }

    async fn manage(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.e
            .send(self.vm_admin.secret.expose_once(), method, path, body)
            .await
    }

    async fn read(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.e
            .send(self.vm_reader.secret.expose_once(), method, path, body)
            .await
    }

    async fn mailbox(&self, owner_type: &str, owner: impl std::fmt::Display) -> Uuid {
        let r = self
            .manage(
                Method::POST,
                "/v1/enterprise/voicemail/mailboxes",
                Some(json!({ "owner": dest(owner_type, owner), "displayLabel": format!("Box {}", tag()),
                             "maxMessageSeconds": 120, "retentionDays": 30 })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        conforms("VoicemailMailbox", &r.json);
        id_of(&r.json, "mailboxId")
    }

    async fn message_of(&self, call: Uuid) -> Option<Uuid> {
        strings(
            self.e.h().conn(),
            &format!("SELECT id::text FROM voicemail_messages WHERE call_id = '{call}'"),
        )
        .await
        .pop()
        .map(|s| s.parse().unwrap())
    }

    async fn message_state(&self, message: Uuid) -> String {
        strings(
            self.e.h().conn(),
            &format!(
                "SELECT state || ':' || COALESCE(failure_code, '') FROM voicemail_messages WHERE id = '{message}'"
            ),
        )
        .await
        .pop()
        .unwrap()
    }

    async fn deliver(
        &self,
        message: Uuid,
        call: Uuid,
        bytes: &[u8],
    ) -> Result<CaptureDelivery, CallError> {
        self.e
            .h()
            .state
            .app
            .deliver_capture(message, call, media(bytes, 1500))
            .await
    }

    async fn steps(&self, call: Uuid) -> Vec<String> {
        strings(
            self.e.h().conn(),
            &format!(
                "SELECT decision || ':' || reason FROM enterprise_routing_steps WHERE call_id = '{call}' \
                  ORDER BY step_number"
            ),
        )
        .await
    }

    async fn outcome(&self, call: Uuid) -> String {
        strings(
            self.e.h().conn(),
            &format!(
                "SELECT phase || ':' || COALESCE(outcome, '') FROM enterprise_call_contexts WHERE call_id = '{call}'"
            ),
        )
        .await
        .pop()
        .unwrap_or_default()
    }

    /// The Call's voicemail events; their outbox envelopes are validated against the
    /// shared event contracts and never carry an object key.
    async fn events(&self, call: Uuid) -> Vec<String> {
        for text in strings(
            self.e.h().conn(),
            &format!(
                "SELECT envelope::text FROM calling_outbox_events \
                  WHERE event_type LIKE 'calling.voicemail-%' AND envelope::text LIKE '%{call}%'"
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
            assert!(!text.contains("voicemail/"), "{text}");
        }
        strings(
            self.e.h().conn(),
            &format!(
                "SELECT event_type FROM call_events WHERE call_id = '{call}' \
                   AND event_type LIKE 'VOICEMAIL_%' ORDER BY sequence"
            ),
        )
        .await
    }

    async fn tick(&self) {
        self.e.h().state.app.expire_due(100).await.unwrap();
    }
}

struct Desk {
    principal: Principal,
    phone: Device,
}

async fn desk(e: &EnterpriseWorld, name: &str) -> Desk {
    let principal = e.member(&format!("{name}-{}", tag())).await;
    let phone = e.a.device(&principal, &format!("{name}-desk")).await;
    Desk { principal, phone }
}

async fn dial(e: &EnterpriseWorld, from: &Desk, number: &str) -> (Uuid, Value) {
    let r = e
        .call_extension(
            "VOICE",
            &from.principal,
            &from.phone,
            number,
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    (
        r.json["call"]["callId"].as_str().unwrap().parse().unwrap(),
        r.json,
    )
}

/// A member whose unanswered calls go to their own mailbox, reachable at `number`.
async fn member_with_mailbox(vm: &Vm, number: &str) -> (Desk, Uuid) {
    let e = &vm.e;
    let member = desk(e, "member").await;
    let mailbox = vm.mailbox("PRINCIPAL", member.principal.id()).await;
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/forwarding-rules",
            Some(
                json!({ "principalId": member.principal.id(), "condition": "NO_ANSWER",
                         "destination": dest("VOICEMAIL", mailbox) }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    e.extension(number, "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    (member, mailbox)
}

/// Dials `number`, lets the member's offer expire and returns (call, message).
async fn unanswered(vm: &Vm, caller: &Desk, number: &str) -> (Uuid, Uuid) {
    let (call, _) = dial(&vm.e, caller, number).await;
    vm.e.h().clock.advance(chrono::Duration::seconds(46));
    vm.tick().await;
    let Some(message) = vm.message_of(call).await else {
        panic!("no message: {:?}", vm.steps(call).await);
    };
    (call, message)
}

#[tokio::test]
async fn mailbox_administration() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let member = desk(e, "owner").await;
    let ext = e
        .extension("4100", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    // Validation: typed owners of the scope, bounded limits, no unknown fields.
    for (body, status) in [
        (
            json!({ "owner": dest("PRINCIPAL", member.principal.id()), "displayLabel": "x",
                    "maxMessageSeconds": 2, "retentionDays": 30 }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "owner": dest("PRINCIPAL", member.principal.id()), "displayLabel": "x",
                    "maxMessageSeconds": 60, "retentionDays": 0 }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "owner": dest("PUBLIC_CONTACT", Uuid::new_v4()), "displayLabel": "x",
                    "maxMessageSeconds": 60, "retentionDays": 30 }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "owner": { "type": "PHONE", "number": "+2348012345678" }, "displayLabel": "x",
                    "maxMessageSeconds": 60, "retentionDays": 30 }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "owner": dest("PRINCIPAL", Uuid::new_v4()), "displayLabel": "x",
                    "maxMessageSeconds": 60, "retentionDays": 30 }),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({ "owner": dest("PRINCIPAL", member.principal.id()), "displayLabel": "x",
                    "maxMessageSeconds": 60, "retentionDays": 30, "storageUrl": "s3://x" }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let r = vm
            .manage(
                Method::POST,
                "/v1/enterprise/voicemail/mailboxes",
                Some(body.clone()),
            )
            .await;
        assert_eq!(r.status, status, "{body}: {}", r.text);
    }
    // Scope separation: configuration administrators cannot manage voicemail.
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/voicemail/mailboxes",
            Some(
                json!({ "owner": dest("PRINCIPAL", member.principal.id()), "displayLabel": "x",
                         "maxMessageSeconds": 60, "retentionDays": 30 }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);

    let personal = vm.mailbox("PRINCIPAL", member.principal.id()).await;
    let by_ext = vm.mailbox("EXTENSION", id_of(&ext, "extensionId")).await;
    let r = e
        .admin(Method::GET, "/v1/enterprise/voicemail/mailboxes", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("VoicemailMailboxList", &r.json);
    assert_eq!(r.json["mailboxes"].as_array().unwrap().len(), 2);
    // Versioned update; stale versions conflict.
    let path = format!("/v1/enterprise/voicemail/mailboxes/{personal}");
    let r = vm
        .manage(
            Method::PATCH,
            &path,
            Some(json!({ "expectedVersion": 1, "retentionDays": 7 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["retentionDays"], 7);
    assert_eq!(r.json["version"], 2);
    let r = vm
        .manage(
            Method::PATCH,
            &path,
            Some(json!({ "expectedVersion": 1, "retentionDays": 8 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    // Another tenant cannot see the mailbox (404, never 403).
    let (_, other) = e.other_tenant("globex").await;
    let r = e
        .send(other.secret.expose_once(), Method::GET, &path, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // The policy can forbid voicemail.
    let mut rules = common::enterprise::default_rules();
    rules["voicemailAllowed"] = json!(false);
    e.create(
        "/v1/enterprise/policies",
        json!({ "name": format!("No voicemail {}", tag()), "isDefault": true, "rules": rules }),
    )
    .await;
    let r = vm
        .manage(
            Method::POST,
            "/v1/enterprise/voicemail/mailboxes",
            Some(
                json!({ "owner": dest("PRINCIPAL", member.principal.id()), "displayLabel": "x",
                         "maxMessageSeconds": 60, "retentionDays": 30 }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // Disable (audited) — never blocked.
    let r = vm
        .manage(
            Method::DELETE,
            &format!("/v1/enterprise/voicemail/mailboxes/{by_ext}?expectedVersion=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DISABLED");
    assert_eq!(
        strings(
            e.h().conn(),
            &format!(
                "SELECT operation FROM enterprise_admin_audit WHERE resource_type = 'VOICEMAIL_MAILBOX' \
                   AND resource_id IN ('{personal}', '{by_ext}') ORDER BY seq"
            ),
        )
        .await,
        ["CREATE", "CREATE", "UPDATE", "DISABLE"]
    );
}

/// No answer: the member's NO_ANSWER rule sends the SAME Call to their mailbox; the RTC
/// capture adapter records it; the delivered audio is stored privately; READY; the Call
/// ends NO_ANSWER with the enterprise outcome VOICEMAIL; events and evidence link it.
#[tokio::test]
async fn no_answer_records_a_message_on_the_same_call() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    let (member, mailbox) = member_with_mailbox(&vm, "4200").await;
    let (call, message) = unanswered(&vm, &caller, "4200").await;
    assert!(
        vm.steps(call)
            .await
            .contains(&"VOICEMAIL:RECORDING".to_string()),
        "{:?}",
        vm.steps(call).await
    );
    assert_eq!(vm.message_state(message).await, "RECORDING:");
    assert_eq!(vm.outcome(call).await, "VOICEMAIL:");
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    let started = vm.capture.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].capture_id, message);
    assert_eq!(started[0].call_id, call);
    assert_eq!(started[0].max_duration_seconds, 120);
    // The authorized media scope: exactly the caller's own live sessions.
    let caller_sessions = strings(
        e.h().conn(),
        &format!(
            "SELECT s.media_identity FROM call_sessions s JOIN call_participants p \
               ON p.id = s.participant_id WHERE s.call_id = '{call}' \
                AND p.principal_id = '{}' AND s.ended_at IS NULL ORDER BY s.created_at, s.id",
            caller.principal.id()
        ),
    )
    .await;
    assert!(!caller_sessions.is_empty());
    assert_eq!(started[0].sources, caller_sessions);
    // Media access before READY is refused.
    let r = vm
        .manage(
            Method::POST,
            &format!("/v1/enterprise/voicemail/messages/{message}/media-access"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    // A tampered capture is refused; nothing is stored.
    let audio = wav(1500);
    let mut bad = media(&audio, 1500);
    bad.sha256 = cf_object_storage::sha256_hex(b"other");
    let err = e
        .h()
        .state
        .app
        .deliver_capture(message, call, bad)
        .await
        .unwrap_err();
    assert!(matches!(err, CallError::Validation(_)), "{err:?}");
    // Another Call's capture id is not this Call's.
    assert!(matches!(
        vm.deliver(message, Uuid::new_v4(), &audio).await,
        Err(CallError::NotFound)
    ));
    assert!(vm.store.is_empty());
    // Delivery: stored, READY, the Call ends NO_ANSWER / VOICEMAIL.
    assert_eq!(
        vm.deliver(message, call, &audio).await.unwrap(),
        CaptureDelivery::Stored
    );
    assert_eq!(vm.message_state(message).await, "READY:");
    assert_eq!(e.a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(vm.outcome(call).await, "ENDED:VOICEMAIL");
    let keys = vm.store.keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(vm.store.get_raw(&keys[0]).unwrap(), audio);
    // Idempotent re-delivery.
    assert_eq!(
        vm.deliver(message, call, &audio).await.unwrap(),
        CaptureDelivery::AlreadyStored
    );
    assert_eq!(vm.store.len(), 1);
    // Evidence: ONE Call, the voicemail events and their outbox envelopes.
    assert_eq!(
        e.count(&format!("SELECT count(*) FROM calls WHERE id = '{call}'"))
            .await,
        1
    );
    assert_eq!(
        vm.events(call).await,
        ["VOICEMAIL_RECEIVED", "VOICEMAIL_READY"]
    );
    let envelopes = strings(
        e.h().conn(),
        &format!(
            "SELECT envelope::text FROM calling_outbox_events WHERE event_type IN \
               ('calling.voicemail-received', 'calling.voicemail-ready') AND envelope::text LIKE '%{call}%'"
        ),
    )
    .await;
    assert_eq!(envelopes.len(), 2);
    for text in envelopes {
        let envelope: Value = serde_json::from_str(&text).unwrap();
        common::contracts::check("platform/events/envelope/v1.schema.json", &envelope);
        common::contracts::check(
            "calling/events/call-lifecycle/v1.schema.json",
            &envelope["payload"],
        );
        assert!(!text.contains("voicemail/"), "{text}");
    }
    // The owner reads it: integrity metadata, never an object key or a stored URL.
    let r = vm
        .read(
            Method::GET,
            &format!(
                "/v1/enterprise/voicemail/messages?mailboxId={mailbox}&principalId={}",
                member.principal.id()
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("VoicemailMessageList", &r.json);
    let m = &r.json["messages"][0];
    assert_eq!(m["messageId"], message.to_string());
    assert_eq!(m["callId"], call.to_string());
    assert_eq!(m["state"], "READY");
    assert_eq!(m["readState"], "UNREAD");
    assert_eq!(m["sizeBytes"], audio.len());
    assert_eq!(m["sha256"], cf_object_storage::sha256_hex(&audio));
    assert!(!r.text.contains("voicemail/"), "{}", r.text);
    assert!(!r.text.to_lowercase().contains("objectkey"), "{}", r.text);
    // A Call reaches voicemail once: a second message is impossible.
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM voicemail_messages WHERE call_id = '{call}'"
        ))
        .await,
        1
    );
}

/// The caller hangs up while leaving a message: the Call is CANCELLED, its outcome is
/// VOICEMAIL, and the audio that still arrives makes the message READY.
#[tokio::test]
async fn caller_hangup_during_recording_still_keeps_the_message() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    member_with_mailbox(&vm, "4250").await;
    let (call, message) = unanswered(&vm, &caller, "4250").await;
    let (_, v) = e.a.state(call).await;
    let r = e.a.command(call, "cancel", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Cancelled);
    assert_eq!(vm.outcome(call).await, "ENDED:VOICEMAIL");
    vm.deliver(message, call, &wav(800)).await.unwrap();
    assert_eq!(vm.message_state(message).await, "READY:");
    assert_eq!(e.a.state(call).await.0, CallState::Cancelled);
}

/// Every route kind reaches a mailbox on the SAME Call: queue overflow, after hours and an
/// IVR key.
#[tokio::test]
async fn queue_overflow_after_hours_and_ivr_route_to_voicemail() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    let other = desk(e, "other").await;
    let agent = desk(e, "agent").await;

    // Queue overflow (capacity 1): the second caller reaches the queue's mailbox.
    let q = e
        .create(
            "/v1/enterprise/queues",
            json!({ "name": format!("Support {}", tag()), "maxWaitSeconds": 60,
                    "offerTimeoutSeconds": 10, "capacity": 1 }),
        )
        .await;
    let qid = id_of(&q, "queueId");
    let queue_box = vm.mailbox("QUEUE", qid).await;
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/queues/{qid}"),
            Some(json!({ "expectedVersion": 1, "overflow": dest("VOICEMAIL", queue_box) })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.extension("4300", "QUEUE", qid).await;
    dial(e, &other, "4300").await;
    let (call, created) = dial(e, &caller, "4300").await;
    assert_eq!(created["enterprise"]["phase"], "VOICEMAIL", "{created}");
    let steps = vm.steps(call).await;
    assert!(
        steps.iter().any(|s| s.starts_with("QUEUE_FULL:")),
        "{steps:?}"
    );
    assert!(
        steps.contains(&"VOICEMAIL:RECORDING".to_string()),
        "{steps:?}"
    );
    let message = vm.message_of(call).await.unwrap();
    vm.deliver(message, call, &wav(500)).await.unwrap();
    assert_eq!(vm.message_state(message).await, "READY:");

    // After hours: the schedule's closed route is a mailbox.
    let agent_box = vm.mailbox("PRINCIPAL", agent.principal.id()).await;
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/business-hours",
            Some(json!({ "name": format!("Office {}", tag()), "timezone": "Africa/Lagos",
                         "openDestination": dest("ENTERPRISE_MEMBER", agent.principal.id()),
                         "closedDestination": dest("VOICEMAIL", agent_box),
                         "weeklyWindows": [{ "dayOfWeek": "MONDAY", "start": "09:00", "end": "17:00" }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    e.extension("4400", "SCHEDULE", id_of(&r.json, "businessHoursId"))
        .await;
    e.h().clock.set(at("2031-03-08T22:00:00Z"));
    let (call, _) = dial(e, &caller, "4400").await;
    let steps = vm.steps(call).await;
    assert!(steps.contains(&"SCHEDULE:CLOSED".to_string()), "{steps:?}");
    assert!(
        steps.contains(&"VOICEMAIL:RECORDING".to_string()),
        "{steps:?}"
    );
    assert!(e
        .a
        .invitation_for(call, agent.phone.endpoint)
        .await
        .is_none());
    assert!(vm.message_of(call).await.is_some());

    // IVR: key 9 leaves a message.
    let f = e
        .create(
            "/v1/enterprise/ivr-flows",
            json!({ "name": format!("Menu {}", tag()) }),
        )
        .await;
    let fid = id_of(&f, "ivrFlowId");
    let r = e
        .admin(
            Method::POST,
            &format!("/v1/enterprise/ivr-flows/{fid}/versions"),
            Some(json!({ "graph": { "startNode": "main", "nodes": [
                { "id": "main", "kind": "MENU", "prompt": { "builtin": "MENU" }, "inputTimeoutSeconds": 5,
                  "maxRetries": 1, "options": [{ "digit": "9", "next": "vm" }],
                  "onInvalid": "bye", "onTimeout": "bye" },
                { "id": "vm", "kind": "ROUTE_VOICEMAIL", "mailboxId": agent_box },
                { "id": "bye", "kind": "HANGUP", "prompt": { "builtin": "GOODBYE" } },
            ]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let flow_version = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/ivr-flows/{fid}"),
            None,
        )
        .await
        .json["version"]
        .clone();
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ivr-flows/{fid}/activation"),
            Some(json!({ "expectedVersion": flow_version, "versionNumber": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.extension("4500", "IVR", fid).await;
    let (call, _) = dial(e, &caller, "4500").await;
    let r = e
        .send(
            e.caller_token(),
            Method::POST,
            &format!("/v1/calls/{call}/ivr/input"),
            Some(json!({ "endpointId": caller.phone.endpoint, "digit": "9" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["accepted"], true);
    let steps = vm.steps(call).await;
    assert!(
        steps.contains(&"VOICEMAIL:RECORDING".to_string()),
        "{steps:?}"
    );
    let message = vm.message_of(call).await.unwrap();
    assert!(vm.capture.started().iter().any(|c| c.capture_id == message));
    vm.deliver(message, call, &wav(700)).await.unwrap();
    assert_eq!(e.a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(vm.outcome(call).await, "ENDED:VOICEMAIL");
}

/// Storage failure never fakes READY: the message stays PROCESSING (the deliverer
/// retries) and recovers when storage returns. A lost capture ends FAILED; a refused
/// capture start fails the message and ends the Call safely.
#[tokio::test]
async fn storage_failure_is_recoverable_and_captures_can_fail() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    member_with_mailbox(&vm, "4600").await;
    let (call, message) = unanswered(&vm, &caller, "4600").await;
    let audio = wav(1000);
    vm.store.set_available(false);
    assert!(matches!(
        vm.deliver(message, call, &audio).await,
        Err(CallError::ArtifactStorageUnavailable)
    ));
    assert_eq!(
        vm.message_state(message).await,
        "PROCESSING:STORAGE_UNAVAILABLE"
    );
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    vm.store.set_available(true);
    assert_eq!(
        vm.deliver(message, call, &audio).await.unwrap(),
        CaptureDelivery::Stored
    );
    assert_eq!(vm.message_state(message).await, "READY:");

    // Lost: nothing arrives — the Call ends at its deadline, the message later FAILED.
    let (call, message) = unanswered(&vm, &caller, "4600").await;
    e.h().clock.advance(chrono::Duration::seconds(151));
    vm.tick().await;
    assert_eq!(e.a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(vm.message_state(message).await, "RECORDING:");
    e.h().clock.advance(chrono::Duration::minutes(31));
    vm.tick().await;
    assert_eq!(vm.message_state(message).await, "FAILED:CAPTURE_LOST");
    // A late delivery still recovers the audio (FAILED -> READY).
    vm.deliver(message, call, &audio).await.unwrap();
    assert_eq!(vm.message_state(message).await, "READY:");

    // The adapter refuses to start: FAILED at once, the Call ends NO_ANSWER.
    vm.capture.refuse.store(true, Ordering::SeqCst);
    let (call, message) = unanswered(&vm, &caller, "4600").await;
    assert_eq!(
        vm.message_state(message).await,
        "FAILED:CAPTURE_START_FAILED"
    );
    assert_eq!(e.a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(vm.outcome(call).await, "ENDED:NO_ANSWER");
    // Each failed message is an operator incident (one per message, restricted evidence).
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM calling_incidents WHERE category = 'VOICEMAIL_FINALIZATION_FAILURE' \
             AND resource_type = 'VOICEMAIL_MESSAGE' AND resource_id = '{message}' AND status = 'OPEN' \
             AND restricted_evidence->>'failureCode' = 'CAPTURE_START_FAILED'"
        ))
        .await,
        1
    );
    assert!(
        e.count(
            "SELECT count(*) FROM calling_incidents WHERE category = 'VOICEMAIL_FINALIZATION_FAILURE' \
             AND restricted_evidence->>'failureCode' = 'CAPTURE_LOST'"
        )
        .await
            >= 1
    );
}

/// Retrieval is authorized FIRST: the owner (on whose behalf the application asks) or a
/// voicemail administrator; anyone else — another principal, another tenant — gets 404.
/// Media access is a short-lived signed read of one object, audited, never stored.
#[tokio::test]
async fn retrieval_is_authorized_and_audited() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    let (member, mailbox) = member_with_mailbox(&vm, "4700").await;
    let stranger = desk(e, "stranger").await;
    let (call, message) = unanswered(&vm, &caller, "4700").await;
    vm.deliver(message, call, &wav(900)).await.unwrap();
    let path = format!("/v1/enterprise/voicemail/messages/{message}");
    let owner = member.principal.id();

    // The owner.
    let r = vm
        .read(Method::GET, &format!("{path}?principalId={owner}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("VoicemailMessage", &r.json);
    let r = vm
        .read(
            Method::POST,
            &format!("{path}/media-access"),
            Some(json!({ "principalId": owner })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("VoicemailMediaAccess", &r.json);
    let url = r.json["url"].as_str().unwrap();
    assert!(!url.contains("voicemail/"), "{url}");
    assert!(!url.contains(&message.to_string()), "{url}");
    let expires = DateTime::parse_from_rfc3339(r.json["expiresAt"].as_str().unwrap()).unwrap();
    assert!(
        expires.with_timezone(&Utc)
            <= calling::domain::Clock::now(&e.h().clock) + chrono::Duration::seconds(300)
    );
    // Nothing about the URL is stored.
    assert_eq!(
        e.count(
            "SELECT count(*) FROM voicemail_messages t WHERE row_to_json(t)::text LIKE '%memory://%'"
        )
        .await,
        0
    );

    // Not the owner, no principal without the manage scope, another tenant: refused.
    for (method, p, body) in [
        (
            Method::GET,
            format!("{path}?principalId={}", stranger.principal.id()),
            None,
        ),
        (
            Method::POST,
            format!("{path}/media-access"),
            Some(json!({ "principalId": stranger.principal.id() })),
        ),
        (
            Method::GET,
            format!(
                "/v1/enterprise/voicemail/messages?mailboxId={mailbox}&principalId={}",
                stranger.principal.id()
            ),
            None,
        ),
        (
            Method::GET,
            format!("{path}?principalId={}", Uuid::new_v4()),
            None,
        ),
    ] {
        let r = vm.read(method, &p, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}: {}", r.text);
    }
    let r = vm.read(Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = vm
        .read(
            Method::POST,
            &format!("{path}/media-access"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // A configuration administrator holds no voicemail scope.
    let r = e
        .admin(Method::GET, &format!("{path}?principalId={owner}"), None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let (other_consumer, _) = e.other_tenant("initech").await;
    let other = credential(
        e.h(),
        &other_consumer,
        e.a.w.kind,
        &[
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ],
    )
    .await;
    for (method, p, body) in [
        (Method::GET, path.clone(), None),
        (
            Method::POST,
            format!("{path}/media-access"),
            Some(json!({})),
        ),
        (
            Method::PUT,
            format!("{path}/read-state"),
            Some(json!({ "expectedVersion": 1, "readState": "READ" })),
        ),
        (Method::DELETE, format!("{path}?expectedVersion=1"), None),
    ] {
        let r = e.send(other.secret.expose_once(), method, &p, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}: {}", r.text);
    }
    // The voicemail administrator.
    let r = vm
        .manage(
            Method::POST,
            &format!("{path}/media-access"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Every media access is audited.
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_admin_audit WHERE resource_type = 'VOICEMAIL_MESSAGE' \
               AND resource_id = '{message}' AND operation = 'MEDIA_ACCESS' \
               AND details->>'outcome' = 'GRANTED'"
        ))
        .await,
        2
    );

    // Read state: versioned and audited.
    let current = vm.manage(Method::GET, &path, None).await.json["version"]
        .as_i64()
        .unwrap();
    let r = vm
        .manage(
            Method::PUT,
            &format!("{path}/read-state"),
            Some(json!({ "expectedVersion": current, "readState": "READ", "principalId": owner })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["readState"], "READ");
    assert!(r.json["readAt"].is_string());
    let r = vm
        .manage(
            Method::PUT,
            &format!("{path}/read-state"),
            Some(
                json!({ "expectedVersion": current, "readState": "UNREAD", "principalId": owner }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = vm
        .manage(
            Method::PUT,
            &format!("{path}/read-state"),
            Some(json!({ "expectedVersion": current + 1, "readState": "UNREAD", "principalId": owner })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["readState"], "UNREAD");
    assert!(r.json["readAt"].is_null());
    // The reader (voicemail.read only) cannot change state.
    let r = vm
        .read(
            Method::PUT,
            &format!("{path}/read-state"),
            Some(json!({ "expectedVersion": current + 2, "readState": "READ", "principalId": owner })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(
        strings(
            e.h().conn(),
            &format!(
                "SELECT operation FROM enterprise_admin_audit WHERE resource_id = '{message}' \
                   AND operation LIKE 'MARK_%' AND details->>'outcome' = 'GRANTED' ORDER BY seq"
            ),
        )
        .await,
        ["MARK_READ", "MARK_UNREAD"]
    );
    // Keyset pagination, newest first.
    let (call2, message2) = unanswered(&vm, &caller, "4700").await;
    vm.deliver(message2, call2, &wav(400)).await.unwrap();
    let list = |cursor: Option<String>| {
        let mut p = format!(
            "/v1/enterprise/voicemail/messages?mailboxId={mailbox}&principalId={owner}&limit=1"
        );
        if let Some(c) = cursor {
            p.push_str(&format!("&cursor={c}"));
        }
        p
    };
    let first = vm.read(Method::GET, &list(None), None).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert_eq!(first.json["messages"][0]["messageId"], message2.to_string());
    let cursor = first.json["nextCursor"].as_str().unwrap().to_string();
    let second = vm.read(Method::GET, &list(Some(cursor)), None).await;
    assert_eq!(second.json["messages"][0]["messageId"], message.to_string());
    assert!(second.json["nextCursor"].is_null());
    let unread = vm
        .read(
            Method::GET,
            &format!(
                "/v1/enterprise/voicemail/messages?mailboxId={mailbox}&principalId={owner}&readState=UNREAD"
            ),
            None,
        )
        .await;
    assert_eq!(unread.json["messages"].as_array().unwrap().len(), 2);
}

/// Deletion is asynchronous and idempotent: DELETING (audited) -> the object is removed
/// -> DELETED (event). A storage outage delays it, never loses it. Retention deletes as
/// the system. The originating Call's history is untouched.
#[tokio::test]
async fn deletion_and_retention() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    let (member, mailbox) = member_with_mailbox(&vm, "4800").await;
    let owner = member.principal.id();
    let (call, message) = unanswered(&vm, &caller, "4800").await;
    let path = format!("/v1/enterprise/voicemail/messages/{message}");
    // Still recording: cannot be deleted.
    let r = vm
        .manage(
            Method::DELETE,
            &format!("{path}?expectedVersion=1&principalId={owner}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    vm.deliver(message, call, &wav(600)).await.unwrap();
    assert_eq!(vm.store.len(), 1);
    let version = vm.manage(Method::GET, &path, None).await.json["version"]
        .as_i64()
        .unwrap();
    let r = vm
        .manage(
            Method::DELETE,
            &format!("{path}?expectedVersion={version}&principalId={owner}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "DELETING");
    // Storage down: the deletion waits (attempt counted), the object survives.
    vm.store.set_available(false);
    vm.tick().await;
    assert_eq!(vm.message_state(message).await, "DELETING:");
    assert_eq!(vm.store.len(), 1);
    vm.store.set_available(true);
    e.h().clock.advance(chrono::Duration::seconds(5));
    vm.tick().await;
    assert_eq!(vm.message_state(message).await, "DELETED:");
    assert!(vm.store.is_empty());
    assert_eq!(
        vm.events(call).await,
        ["VOICEMAIL_RECEIVED", "VOICEMAIL_READY", "VOICEMAIL_DELETED"]
    );
    // A second delete conflicts; deleted messages leave the mailbox listing.
    let r = vm
        .manage(
            Method::DELETE,
            &format!("{path}?expectedVersion=99&principalId={owner}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = vm
        .read(
            Method::GET,
            &format!("/v1/enterprise/voicemail/messages?mailboxId={mailbox}&principalId={owner}"),
            None,
        )
        .await;
    assert_eq!(r.json["messages"].as_array().unwrap().len(), 0);
    let r = vm
        .read(
            Method::GET,
            &format!(
                "/v1/enterprise/voicemail/messages?mailboxId={mailbox}&principalId={owner}&state=DELETED"
            ),
            None,
        )
        .await;
    assert_eq!(r.json["messages"].as_array().unwrap().len(), 1);
    // The Call's history is untouched.
    assert_eq!(e.a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(vm.outcome(call).await, "ENDED:VOICEMAIL");

    // Retention (30 days): READY -> DELETING -> DELETED as the system.
    let (call, message) = unanswered(&vm, &caller, "4800").await;
    vm.deliver(message, call, &wav(600)).await.unwrap();
    e.h().clock.advance(chrono::Duration::days(29));
    vm.tick().await;
    assert_eq!(vm.message_state(message).await, "READY:");
    e.h()
        .clock
        .advance(chrono::Duration::days(1) + chrono::Duration::seconds(1));
    vm.tick().await;
    assert_eq!(vm.message_state(message).await, "DELETED:");
    assert!(vm.store.is_empty());
    assert_eq!(
        strings(
            e.h().conn(),
            &format!(
                "SELECT actor_kind || ':' || operation FROM enterprise_admin_audit \
                   WHERE resource_id = '{message}' ORDER BY seq"
            ),
        )
        .await,
        ["SYSTEM:RETENTION_DELETE"]
    );
    assert_eq!(
        strings(
            e.h().conn(),
            &format!("SELECT delete_reason FROM voicemail_messages WHERE id = '{message}'"),
        )
        .await,
        ["RETENTION"]
    );
    // Rows are never hard-deleted.
    let err = e
        .h()
        .conn()
        .execute_unprepared(&format!(
            "DELETE FROM voicemail_messages WHERE id = '{message}'"
        ))
        .await;
    assert!(err.is_err());
}

/// The kill switch stops NEW voicemail (the route ends NO_ROUTE) but never cleanup.
#[tokio::test]
async fn kill_switch_blocks_new_voicemail_only() {
    let mut flags = EnterpriseFlags::ALL_ON;
    flags.voicemail = false;
    let Some(vm) = Vm::with_flags(flags).await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    member_with_mailbox(&vm, "4900").await;
    let (call, _) = dial(e, &caller, "4900").await;
    e.h().clock.advance(chrono::Duration::seconds(46));
    vm.tick().await;
    assert!(vm.message_of(call).await.is_none());
    assert!(
        vm.steps(call)
            .await
            .contains(&"NO_ROUTE:VOICEMAIL_DISABLED".to_string()),
        "{:?}",
        vm.steps(call).await
    );
    assert!(e.a.state(call).await.0.is_terminal());
    assert!(vm.capture.started().is_empty());
}

/// The internal capture endpoint: Telephony only (`telephony-capture.deliver`), only a
/// capture it was asked for (a SIP capture on that leg), integrity-checked; storage
/// outages are 503 so the edge retries.
#[tokio::test]
async fn capture_endpoint_authorization() {
    use base64::Engine;
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    member_with_mailbox(&vm, "4950").await;
    let (call, message) = unanswered(&vm, &caller, "4950").await;
    let audio = wav(300);
    let body = json!({
        "schemaVersion": "1",
        "captureId": message,
        "telephonyLegId": Uuid::new_v4(),
        "callId": call,
        "callLegId": Uuid::new_v4(),
        "outcome": "COMPLETED",
        "media": {
            "contentType": "audio/wav",
            "durationMs": 300,
            "sizeBytes": audio.len(),
            "sha256": cf_object_storage::sha256_hex(&audio),
            "base64": base64::engine::general_purpose::STANDARD.encode(&audio),
        },
    });
    let send = |signer: Arc<ServiceTokenSigner>, body: Value| {
        let h = e.h();
        async move {
            let signed = signer
                .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
                .unwrap();
            let headers: Vec<(String, String)> = signed
                .headers()
                .iter()
                .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap().to_string()))
                .collect();
            let refs: Vec<(&str, &str)> = headers
                .iter()
                .map(|(n, v)| (n.as_str(), v.as_str()))
                .collect();
            h.request_with(
                Method::POST,
                "/internal/v1/telephony/captures",
                None,
                Some(body),
                &refs,
            )
            .await
        }
    };
    // Unauthenticated, another service, an untrusted signer: refused.
    let r = e
        .h()
        .request(
            Method::POST,
            "/internal/v1/telephony/captures",
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    let r = send(e.h().peers.privacy_relay.clone(), body.clone()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = send(e.h().peers.untrusted.clone(), body.clone()).await;
    assert!(
        matches!(r.status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
        "{}",
        r.text
    );
    // Telephony cannot deliver an application (RTC) capture: 404, nothing stored.
    let r = send(e.h().peers.telephony.clone(), body.clone()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let mut unknown = body.clone();
    unknown["captureId"] = json!(Uuid::new_v4());
    let r = send(e.h().peers.telephony.clone(), unknown).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let mut extra = body.clone();
    extra["objectKey"] = json!("voicemail/x");
    let r = send(e.h().peers.telephony.clone(), extra).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    assert!(vm.store.is_empty());
    assert_eq!(vm.message_state(message).await, "RECORDING:");

    // A SIP capture of this leg (as Telephony would see it): delivered; a size mismatch
    // or an outage is refused / 503.
    let leg: Uuid = strings(
        e.h().conn(),
        &format!(
            "SELECT id::text FROM call_legs WHERE call_id = '{call}' ORDER BY created_at LIMIT 1"
        ),
    )
    .await
    .pop()
    .unwrap()
    .parse()
    .unwrap();
    e.h()
        .conn()
        .execute_unprepared(&format!(
            "ALTER TABLE voicemail_messages DISABLE TRIGGER trg_voicemail_messages_immutable; \
             UPDATE voicemail_messages SET capture_source = 'SIP', call_leg_id = '{leg}', version = version + 1 WHERE id = '{message}'; \
             ALTER TABLE voicemail_messages ENABLE TRIGGER trg_voicemail_messages_immutable;"
        ))
        .await
        .unwrap();
    let mut sip = body.clone();
    sip["callLegId"] = json!(leg);
    let mut short = sip.clone();
    short["media"]["sizeBytes"] = json!(audio.len() + 1);
    let r = send(e.h().peers.telephony.clone(), short).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    vm.store.set_available(false);
    let r = send(e.h().peers.telephony.clone(), sip.clone()).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "ARTIFACT_STORAGE_UNAVAILABLE");
    vm.store.set_available(true);
    let r = send(e.h().peers.telephony.clone(), sip.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "STORED");
    let r = send(e.h().peers.telephony.clone(), sip).await;
    assert_eq!(r.json["status"], "ALREADY_STORED");
    assert_eq!(vm.message_state(message).await, "READY:");
    // A FAILED report for an already stored capture changes nothing.
    let mut failed = body.clone();
    failed["callLegId"] = json!(leg);
    failed["outcome"] = json!("FAILED");
    failed["failureCode"] = json!("EDGE_RECORDING_FAILED");
    failed.as_object_mut().unwrap().remove("media");
    let r = send(e.h().peers.telephony.clone(), failed).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(vm.message_state(message).await, "READY:");
}

/// §189: the same capture delivered twice concurrently — one stored object, READY once,
/// one VOICEMAIL_READY event.
#[tokio::test]
async fn concurrent_finalization_stores_once() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    let (_member, _mailbox) = member_with_mailbox(&vm, "4900").await;
    for round in 0..3 {
        let (call, message) = unanswered(&vm, &caller, "4900").await;
        let audio = wav(800);
        let (a, b) = tokio::join!(
            vm.deliver(message, call, &audio),
            vm.deliver(message, call, &audio)
        );
        let outcomes = [a.unwrap(), b.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == CaptureDelivery::Stored)
                .count(),
            1,
            "round {round}: {outcomes:?}"
        );
        assert_eq!(vm.message_state(message).await, "READY:");
        assert_eq!(
            vm.events(call)
                .await
                .iter()
                .filter(|t| *t == "VOICEMAIL_READY")
                .count(),
            1
        );
    }
    assert_eq!(vm.store.len(), 3, "one object per message");
}

/// §189: an owner's delete racing the retention worker — one deletion, one object
/// removal, the message ends DELETED with one reason.
#[tokio::test]
async fn delete_racing_retention_deletes_once() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    let (member, _mailbox) = member_with_mailbox(&vm, "4950").await;
    let owner = member.principal.id();
    let (call, message) = unanswered(&vm, &caller, "4950").await;
    vm.deliver(message, call, &wav(600)).await.unwrap();
    assert_eq!(vm.store.len(), 1);
    let path = format!("/v1/enterprise/voicemail/messages/{message}");
    let version = vm.manage(Method::GET, &path, None).await.json["version"]
        .as_i64()
        .unwrap();
    e.h()
        .clock
        .advance(chrono::Duration::days(30) + chrono::Duration::seconds(1));
    let delete_path = format!("{path}?expectedVersion={version}&principalId={owner}");
    let (r, _) = tokio::join!(vm.manage(Method::DELETE, &delete_path, None), vm.tick());
    // Either the owner won (200 DELETING) or retention did first (409 / version moved).
    assert!(
        r.status == StatusCode::OK || r.status == StatusCode::CONFLICT,
        "{}",
        r.text
    );
    e.h().clock.advance(chrono::Duration::seconds(5));
    vm.tick().await;
    vm.tick().await;
    assert_eq!(vm.message_state(message).await, "DELETED:");
    assert!(vm.store.is_empty());
    assert_eq!(
        vm.events(call)
            .await
            .iter()
            .filter(|t| *t == "VOICEMAIL_DELETED")
            .count(),
        1
    );
    let reasons = strings(
        e.h().conn(),
        &format!("SELECT delete_reason FROM voicemail_messages WHERE id = '{message}'"),
    )
    .await;
    assert!(
        reasons == ["USER"] || reasons == ["RETENTION"] || reasons == ["ADMIN"],
        "{reasons:?}"
    );
}

/// Part H §79.29: every voicemail access decision on a message of the caller's own scope
/// is audited — GRANTED reads / media accesses / deletes, and DENIED attempts by a
/// non-owner (answered 404, byte-identical to an unknown id) or by a credential missing
/// the voicemail scope (403). Another tenant gets the same 404 as for an unknown id and
/// leaves NO audit row anywhere. No audit row carries an object key.
#[tokio::test]
async fn voicemail_access_decisions_are_audited_without_revealing_existence() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    let (member, _) = member_with_mailbox(&vm, "4900").await;
    let stranger = desk(e, "stranger").await;
    let (call, message) = unanswered(&vm, &caller, "4900").await;
    vm.deliver(message, call, &wav(700)).await.unwrap();
    let path = format!("/v1/enterprise/voicemail/messages/{message}");
    let owner = member.principal.id();
    let audit = |filter: &str| {
        format!(
            "SELECT operation || ':' || COALESCE(details->>'outcome', '') || ':' || \
                    COALESCE(details->>'reason', '') FROM enterprise_admin_audit \
              WHERE resource_id = '{message}' {filter} ORDER BY seq"
        )
    };
    let trail = || async { strings(e.h().conn(), &audit("")).await };

    // Granted read.
    let r = vm
        .read(Method::GET, &format!("{path}?principalId={owner}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(trail().await, ["READ_METADATA:GRANTED:"]);

    // A non-owner: 404 exactly like an unknown id — and audited DENIED.
    let denied = vm
        .read(
            Method::GET,
            &format!("{path}?principalId={}", stranger.principal.id()),
            None,
        )
        .await;
    let unknown = vm
        .read(
            Method::GET,
            &format!(
                "/v1/enterprise/voicemail/messages/{}?principalId={}",
                Uuid::new_v4(),
                stranger.principal.id()
            ),
            None,
        )
        .await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(denied.normalized_error(), unknown.normalized_error());
    let r = vm
        .read(
            Method::POST,
            &format!("{path}/media-access"),
            Some(json!({ "principalId": stranger.principal.id() })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert!(r.json.get("url").is_none());
    let r = vm
        .manage(
            Method::DELETE,
            &format!(
                "{path}?expectedVersion=1&principalId={}",
                stranger.principal.id()
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert_eq!(vm.message_state(message).await, "READY:", "no mutation");
    // Missing scopes: 403, audited DENIED.
    let r = vm
        .read(
            Method::DELETE,
            &format!("{path}?expectedVersion=1&principalId={owner}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = e
        .admin(Method::GET, &format!("{path}?principalId={owner}"), None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(
        trail().await,
        [
            "READ_METADATA:GRANTED:",
            "READ_METADATA:DENIED:NOT_OWNER",
            "MEDIA_ACCESS:DENIED:NOT_OWNER",
            "DELETE:DENIED:NOT_OWNER",
            "DELETE:DENIED:MISSING_SCOPE",
            "READ_METADATA:DENIED:MISSING_SCOPE",
        ]
    );
    // An unknown id leaves no trace.
    let unknown_id = Uuid::new_v4();
    let r = vm
        .read(
            Method::DELETE,
            &format!("/v1/enterprise/voicemail/messages/{unknown_id}?expectedVersion=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_admin_audit WHERE resource_id = '{unknown_id}'"
        ))
        .await,
        0
    );

    // Another tenant (with every voicemail scope): 404 like an unknown id, no audit row
    // anywhere, nothing changes.
    let (other_consumer, _) = e.other_tenant("initech").await;
    let other = credential(
        e.h(),
        &other_consumer,
        e.a.w.kind,
        &[
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ],
    )
    .await;
    let before = trail().await;
    for (method, p, body) in [
        (Method::GET, path.clone(), None),
        (
            Method::POST,
            format!("{path}/media-access"),
            Some(json!({})),
        ),
        (
            Method::PUT,
            format!("{path}/read-state"),
            Some(json!({ "expectedVersion": 1, "readState": "READ" })),
        ),
        (Method::DELETE, format!("{path}?expectedVersion=1"), None),
    ] {
        let foreign = e
            .send(other.secret.expose_once(), method.clone(), &p, body.clone())
            .await;
        let missing = e
            .send(
                other.secret.expose_once(),
                method,
                &p.replace(&message.to_string(), &Uuid::new_v4().to_string()),
                body,
            )
            .await;
        assert_eq!(
            foreign.status,
            StatusCode::NOT_FOUND,
            "{p}: {}",
            foreign.text
        );
        assert_eq!(
            foreign.normalized_error(),
            missing.normalized_error(),
            "{p}"
        );
    }
    assert_eq!(trail().await, before);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_admin_audit WHERE tenant_id = '{}'",
            other_consumer.tenant.id
        ))
        .await,
        0
    );
    assert_eq!(vm.message_state(message).await, "READY:");

    // Granted media access and delete.
    let r = vm
        .read(
            Method::POST,
            &format!("{path}/media-access"),
            Some(json!({ "principalId": owner })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let version = vm.manage(Method::GET, &path, None).await.json["version"]
        .as_i64()
        .unwrap();
    let r = vm
        .manage(
            Method::DELETE,
            &format!("{path}?expectedVersion={version}&principalId={owner}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let granted = strings(e.h().conn(), &audit("AND details->>'outcome' = 'GRANTED'")).await;
    assert_eq!(
        granted,
        [
            "READ_METADATA:GRANTED:",
            "MEDIA_ACCESS:GRANTED:",
            "READ_METADATA:GRANTED:",
            "DELETE:GRANTED:USER",
        ]
    );
    // Denied rows name the credential that tried (who), never a key.
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_admin_audit WHERE resource_id = '{message}' \
               AND details->>'outcome' = 'DENIED' AND actor_credential_id IS NULL"
        ))
        .await,
        0
    );
    assert_eq!(
        e.count(
            "SELECT count(*) FROM enterprise_admin_audit WHERE details::text LIKE '%voicemail/%' \
               OR details::text LIKE '%memory://%'"
        )
        .await,
        0
    );
}

/// Whether a media-access request for `message` on behalf of `owner` hands out a URL.
async fn media_url(vm: &Vm, message: Uuid, owner: impl std::fmt::Display) -> bool {
    let r = vm
        .read(
            Method::POST,
            &format!("/v1/enterprise/voicemail/messages/{message}/media-access"),
            Some(json!({ "principalId": owner.to_string() })),
        )
        .await;
    r.json.get("url").is_some()
}

/// Part H §79.26 for voicemail: an integrity mismatch after the write, or a failed READY
/// transaction, never leaves the object behind — deleted at once, or queued (key only in
/// `calling_orphan_objects`) and deleted by the worker once the message can no longer be
/// finalized. No media access exists for it.
#[tokio::test]
async fn voicemail_capture_objects_are_never_left_behind() {
    let Some(vm) = Vm::new().await else {
        return;
    };
    let e = &vm.e;
    let caller = desk(e, "caller").await;
    let (member, _) = member_with_mailbox(&vm, "4950").await;
    let owner = member.principal.id();
    let orphans = || async { e.count("SELECT count(*) FROM calling_orphan_objects").await };

    // Integrity mismatch: removed at once.
    let (call, message) = unanswered(&vm, &caller, "4950").await;
    vm.store.set_corrupt_writes(true);
    assert!(matches!(
        vm.deliver(message, call, &wav(500)).await,
        Err(CallError::ArtifactStorageUnavailable)
    ));
    assert!(vm.store.is_empty(), "{:?}", vm.store.keys());
    assert_eq!(orphans().await, 0);
    assert!(!media_url(&vm, message, owner).await);
    vm.store.set_corrupt_writes(false);

    // The READY transaction fails after the write: removed at once ...
    common::exec(
        e.h().conn(),
        "CREATE FUNCTION test_fail_voicemail_ready() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN IF NEW.state = 'READY' THEN RAISE EXCEPTION 'injected finalization failure'; END IF; \
         RETURN NEW; END $$; \
         CREATE TRIGGER trg_test_fail_voicemail_ready BEFORE UPDATE ON voicemail_messages \
         FOR EACH ROW EXECUTE FUNCTION test_fail_voicemail_ready();",
    )
    .await
    .unwrap();
    assert!(vm.deliver(message, call, &wav(500)).await.is_err());
    assert!(vm.store.is_empty());
    assert_eq!(orphans().await, 0);
    // ... or, when storage refuses the delete, queued with an incident (no key in it).
    vm.store.set_failing_deletes(true);
    assert!(vm.deliver(message, call, &wav(500)).await.is_err());
    assert_eq!((vm.store.len(), orphans().await), (1, 1));
    let key = vm.store.keys().pop().unwrap();
    assert!(!media_url(&vm, message, owner).await);
    let evidence = strings(
        e.h().conn(),
        &format!(
            "SELECT restricted_evidence::text FROM calling_incidents WHERE resource_id = '{message}'"
        ),
    )
    .await
    .join(" ");
    assert!(
        !evidence.is_empty() && !evidence.contains("voicemail/"),
        "{evidence}"
    );
    common::exec(
        e.h().conn(),
        "DROP TRIGGER trg_test_fail_voicemail_ready ON voicemail_messages; \
         DROP FUNCTION test_fail_voicemail_ready();",
    )
    .await
    .unwrap();
    // Storage recovers; the message is lost (never finalized): the worker deletes it.
    vm.store.set_failing_deletes(false);
    e.h().clock.advance(chrono::Duration::hours(2));
    vm.tick().await;
    assert!(vm.message_state(message).await.starts_with("FAILED:"));
    assert_eq!(orphans().await, 0);
    assert!(vm.store.get_raw(&key).is_none());
    assert!(vm.store.is_empty());
}
