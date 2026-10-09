//! Part F §62 missed calls and call-history continuity: history is derived from Call
//! evidence for each logical participant — never from a push, a realtime signal or a client
//! acknowledgement — so a client that reconnects after every notification was lost reads
//! the canonical missed / answered outcome (`GET /v1/calling/history` with its signaling
//! credential, `GET /v1/calls?principalId=` for the application). One answered device
//! makes the participant ANSWERED; a losing device or a ring-group colleague's answer is
//! never a second missed Call.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use axum::http::{Method, StatusCode};
use calling::{
    calls::{
        app::{
            enterprise::CapturedMedia,
            history::{history_statement, HistoryFilter},
        },
        command::CallScope,
        service::ListCallsQuery,
        CallState,
    },
    domain::{EnvironmentKind, TenantClassification},
    platform::scopes::names,
    signaling::{
        bus::{SignalBusError, SignalSubscription},
        InMemorySignalBus, RealtimeSignalBus, SignalEnvelope,
    },
};
use common::{
    app::{AppWorld, Device},
    enterprise::{id_of, strings, EnterpriseWorld, RecordingCapture},
    Harness, TestResponse,
};
use sea_orm::{ConnectionTrait, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

/// The realtime path is down: every publish fails (signals are lost, state is not).
struct DownBus {
    inner: InMemorySignalBus,
    attempts: AtomicUsize,
}

#[async_trait::async_trait]
impl RealtimeSignalBus for DownBus {
    async fn publish(&self, _envelope: SignalEnvelope) -> Result<(), SignalBusError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(SignalBusError::Unavailable)
    }

    fn subscribe(&self, endpoint_id: Uuid) -> SignalSubscription {
        self.inner.subscribe(endpoint_id)
    }
}

/// An application-calling world whose realtime fanout is down and whose push transport
/// delivers nothing a client could act on.
async fn outage_world() -> Option<(AppWorld, Arc<DownBus>)> {
    let db = Arc::new(common::TestDb::new().await?);
    let media = Arc::new(common::fake_media::FakeMediaProvider::default());
    let bus = Arc::new(InMemorySignalBus::new());
    let down = Arc::new(DownBus {
        inner: InMemorySignalBus::new(),
        attempts: AtomicUsize::new(0),
    });
    let runtime = common::test_runtime(Some(media.clone()), down.clone());
    let h = Harness::with_runtime(
        db,
        common::TEST_PEPPER,
        calling::domain::ManualClock::new(common::t0()),
        media,
        bus,
        runtime,
    );
    let w = common::calls::CallWorld::in_harness(h, "acme", EnvironmentKind::Production).await;
    Some((AppWorld { w }, down))
}

fn call_id(r: &TestResponse) -> Uuid {
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

/// The endpoint's own history after a reconnect (signaling credential).
async fn reconnect_history(a: &AppWorld, device: &Device, query: &str) -> Value {
    let token = a.signaling_token(device).await;
    let r = a
        .as_endpoint(
            &token,
            Method::GET,
            &format!("/v1/calling/history{query}"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["principalId"], device.principal.to_string());
    common::contracts::check_openapi_component("CallHistoryList", &r.json);
    r.json
}

/// The application's view of `principal`'s history (platform credential).
async fn app_history(a: &AppWorld, principal: Uuid, extra: &str) -> Value {
    let r = a
        .get(&format!("/v1/calls?principalId={principal}{extra}"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check_openapi_component("CallHistoryList", &r.json);
    r.json
}

fn entry(list: &Value, call: Uuid) -> &Value {
    let matching: Vec<&Value> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["callId"] == call.to_string())
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "one entry per logical participant: {list}"
    );
    matching[0]
}

fn ids(list: &Value) -> Vec<String> {
    list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["callId"].as_str().unwrap().to_string())
        .collect()
}

fn outcome(e: &Value) -> (&str, &str, bool) {
    (
        e["participation"]["direction"].as_str().unwrap(),
        e["participation"]["outcome"].as_str().unwrap(),
        e["participation"]["missed"].as_bool().unwrap(),
    )
}

/// §138 / §211: push and realtime are down, the Call rings and times out; after a
/// reconnect the recipient fetches the canonical missed Call.
#[tokio::test]
async fn a_missed_call_survives_push_and_realtime_outage() {
    let Some((a, down)) = outage_world().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "caller-phone").await;
    // Two devices of ONE logical recipient.
    let phone = a.device(&a.w.recipient, "callee-phone").await;
    let browser = a.device(&a.w.recipient, "callee-browser").await;
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &[&a.w.recipient], "m-1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call = call_id(&r);
    // While it rings: RINGING, not (yet) missed.
    let live = app_history(&a, phone.principal, "").await;
    assert_eq!(outcome(entry(&live, call)), ("INBOUND", "RINGING", false));
    a.h().clock.advance(chrono::Duration::seconds(46));
    a.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(a.state(call).await.0, CallState::NoAnswer);
    // Every signal was attempted and lost; no push was delivered.
    assert!(down.attempts.load(Ordering::SeqCst) > 0);
    assert!(a.h().push.sent().is_empty());

    // The recipient reconnects on either device: ONE missed Call.
    for device in [&browser, &phone] {
        let history = reconnect_history(&a, device, "").await;
        assert_eq!(ids(&history), [call.to_string()]);
        let e = entry(&history, call);
        assert_eq!(outcome(e), ("INBOUND", "MISSED", true));
        assert!(e["participation"]["answeredAt"].is_null());
        assert_eq!(e["state"], "NO_ANSWER");
        assert!(e["durationMs"].is_null());
        assert_eq!(
            e["counterparts"][0]["principalId"],
            caller.principal.to_string()
        );
        assert_eq!(e["counterparts"][0]["direction"], "OUTBOUND");
        assert_eq!(e["recordingAvailable"], false);
        assert!(e["voicemail"].is_null() && e["enterprise"].is_null());
    }
    let missed = reconnect_history(&a, &phone, "?missed=true").await;
    assert_eq!(ids(&missed), [call.to_string()]);
    assert!(ids(&reconnect_history(&a, &phone, "?missed=false").await).is_empty());
    // The application sees the same canonical answer.
    let app = app_history(&a, phone.principal, "&missed=true").await;
    assert_eq!(ids(&app), [call.to_string()]);
    // The caller's own entry: outbound, not answered, never "missed".
    let mine = app_history(&a, caller.principal, "").await;
    assert_eq!(
        outcome(entry(&mine, call)),
        ("OUTBOUND", "NOT_ANSWERED", false)
    );
    assert!(ids(&app_history(&a, caller.principal, "&missed=true").await).is_empty());
}

/// §139 / §132 / §133: device B never received anything, device A answers; after a
/// reconnect the participant's history says answered — not missed, not twice.
#[tokio::test]
async fn one_answered_device_answers_for_the_logical_participant() {
    let Some((a, _down)) = outage_world().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "caller-phone").await;
    let device_a = a.device(&a.w.recipient, "callee-a").await;
    let device_b = a.device(&a.w.recipient, "callee-b").await;
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &[&a.w.recipient], "a-1")
        .await;
    let call = call_id(&r);
    let offer = a.invitation_for(call, device_a.endpoint).await.unwrap();
    let (_, v) = a.state(call).await;
    let r = a.accept(call, offer, &device_a, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // B lost the race at the offer level …
    let view = a.call_view(call).await;
    let b_status: Vec<&str> = view["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["endpointId"] == device_b.endpoint.to_string())
        .map(|i| i["status"].as_str().unwrap())
        .collect();
    assert_eq!(b_status, ["ANSWERED_ELSEWHERE"]);
    let (_, v) = a.state(call).await;
    assert_eq!(a.command(call, "end", v).await.status, StatusCode::OK);
    // … but B's reconnect reads the participant's canonical answer.
    let history = reconnect_history(&a, &device_b, "").await;
    let e = entry(&history, call);
    assert_eq!(outcome(e), ("INBOUND", "ANSWERED", false));
    assert!(e["participation"]["answeredAt"].is_string());
    assert!(ids(&reconnect_history(&a, &device_b, "?missed=true").await).is_empty());
    assert_eq!(
        ids(&reconnect_history(&a, &device_a, "?outcome=ANSWERED").await),
        [call.to_string()]
    );
    let mine = app_history(&a, caller.principal, "").await;
    assert_eq!(outcome(entry(&mine, call)), ("OUTBOUND", "ANSWERED", false));
}

/// Declined, cancelled by the caller, connected with a duration, in progress.
#[tokio::test]
async fn outcomes_follow_the_call_evidence() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let callee = a.w.recipient.id().as_uuid();
    let caller = a.w.initiator.id().as_uuid();
    // Declined: the recipient's own decision, not a missed Call.
    let (declined, _, phone) = a.ringing_call("VOICE").await;
    let offer = a.invitation_for(declined, phone.endpoint).await.unwrap();
    let (_, v) = a.state(declined).await;
    assert_eq!(
        a.decline(declined, offer, &phone, v).await.status,
        StatusCode::OK
    );
    a.h().clock.advance(chrono::Duration::seconds(1));
    // Cancelled by the caller while ringing: the classic missed Call.
    let (cancelled, _, _) = a.ringing_call("VOICE").await;
    let (_, v) = a.state(cancelled).await;
    assert_eq!(
        a.command(cancelled, "cancel", v).await.status,
        StatusCode::OK
    );
    a.h().clock.advance(chrono::Duration::seconds(1));
    // Connected, then ended: answered with a duration.
    let (talked, _, _) = a.connected_call("VOICE").await;
    a.h().clock.advance(chrono::Duration::seconds(30));
    let (_, v) = a.state(talked).await;
    assert_eq!(a.command(talked, "end", v).await.status, StatusCode::OK);
    // Still ringing.
    let (ringing, _, _) = a.ringing_call("VOICE").await;

    let history = app_history(&a, callee, "").await;
    assert_eq!(
        ids(&history),
        [ringing, talked, cancelled, declined]
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>(),
        "newest first"
    );
    assert_eq!(
        outcome(entry(&history, declined)),
        ("INBOUND", "DECLINED", false)
    );
    assert_eq!(
        outcome(entry(&history, cancelled)),
        ("INBOUND", "MISSED", true)
    );
    assert_eq!(
        outcome(entry(&history, talked)),
        ("INBOUND", "ANSWERED", false)
    );
    assert_eq!(
        outcome(entry(&history, ringing)),
        ("INBOUND", "RINGING", false)
    );
    let talked_entry = entry(&history, talked);
    assert_eq!(talked_entry["state"], "ENDED");
    assert_eq!(talked_entry["durationMs"], 30_000);
    let mine = app_history(&a, caller, "").await;
    assert_eq!(
        outcome(entry(&mine, declined)),
        ("OUTBOUND", "NOT_ANSWERED", false)
    );
    assert_eq!(
        outcome(entry(&mine, ringing)),
        ("OUTBOUND", "IN_PROGRESS", false)
    );
    // Filters: direction, outcome, missed, terminal state.
    let inbound = app_history(&a, callee, "&direction=INBOUND").await;
    assert_eq!(ids(&inbound).len(), 4);
    assert!(ids(&app_history(&a, callee, "&direction=OUTBOUND").await).is_empty());
    assert_eq!(
        ids(&app_history(&a, callee, "&outcome=DECLINED").await),
        [declined.to_string()]
    );
    assert_eq!(
        ids(&app_history(&a, callee, "&missed=true").await),
        [cancelled.to_string()]
    );
    assert_eq!(
        ids(&app_history(&a, callee, "&state=CANCELLED").await),
        [cancelled.to_string()]
    );
}

/// Keyset pagination is stable, bounds and cursors compose with the filters, every query
/// stays in its scope, and invalid combinations are refused.
#[tokio::test]
async fn history_is_paginated_scoped_and_strict() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let callee = a.w.recipient.id().as_uuid();
    let mut calls = Vec::new();
    for _ in 0..5 {
        let (call, _, _) = a.ringing_call("VOICE").await;
        let (_, v) = a.state(call).await;
        assert_eq!(a.command(call, "cancel", v).await.status, StatusCode::OK);
        calls.push(call.to_string());
        a.h().clock.advance(chrono::Duration::seconds(1));
    }
    calls.reverse();
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let extra = match &cursor {
            Some(c) => format!("&limit=2&missed=true&cursor={c}"),
            None => "&limit=2&missed=true".to_string(),
        };
        let page = app_history(&a, callee, &extra).await;
        assert!(page["data"].as_array().unwrap().len() <= 2);
        seen.extend(ids(&page));
        match page["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(seen, calls, "every missed Call exactly once, newest first");
    // Time bounds.
    let created: Vec<String> = strings(
        a.h().conn(),
        "SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
         FROM calls ORDER BY created_at DESC, id DESC",
    )
    .await;
    let bounded = app_history(
        &a,
        callee,
        &format!("&createdBefore={}&createdAfter={}", created[0], created[3]),
    )
    .await;
    assert_eq!(ids(&bounded), calls[1..4].to_vec());

    // Scope: an unknown principal has no history; another tenant sees nothing of ours.
    assert!(ids(&app_history(&a, Uuid::new_v4(), "").await).is_empty());
    let other = a
        .h()
        .consumer("other", TenantClassification::External)
        .await;
    let foreign = a.h().issue(other.sa(a.w.kind), &[names::CALLS_READ]).await;
    let r = a
        .h()
        .request(
            Method::GET,
            &format!("/v1/calls?principalId={callee}"),
            Some(foreign.secret.expose_once()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(ids(&r.json).is_empty());
    // The caller's endpoint reads its own principal's history only.
    let caller_device = a.device(&a.w.initiator, "caller-main").await;
    let own = reconnect_history(&a, &caller_device, "").await;
    assert!(own["data"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["participation"]["direction"] == "OUTBOUND"));
    let token = a.signaling_token(&caller_device).await;
    let r = a
        .as_endpoint(
            &token,
            Method::GET,
            &format!("/v1/calling/history?principalId={callee}"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // A platform credential is not a signaling credential.
    let r = a.get("/v1/calling/history").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // calls.read is required.
    let no_read = a
        .h()
        .issue(a.w.consumer.sa(a.w.kind), &[names::CALLS_WRITE])
        .await;
    let r = a
        .h()
        .request(
            Method::GET,
            &format!("/v1/calls?principalId={callee}"),
            Some(no_read.secret.expose_once()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // Participant filters need a principal; bounded vocabularies only.
    for bad in [
        "/v1/calls?missed=true",
        "/v1/calls?direction=INBOUND",
        "/v1/calls?outcome=MISSED",
        "/v1/calls?principalId=x",
        "/v1/calls?enterpriseDestinationType=QUEUE",
        "/v1/calls?enterpriseDestinationType=PUBLIC_CONTACT&enterpriseDestinationId=00000000-0000-4000-8000-000000000000",
        "/v1/calls?missed=maybe&principalId=00000000-0000-4000-8000-000000000000",
    ] {
        let r = a.get(bad).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
    }
    // The application-wide listing keeps its shape (no participation without a principal).
    let r = a.get("/v1/calls?limit=1").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json["data"][0].get("participation").is_none());
}

/// §133 in an enterprise ring group: a colleague's answer is ANSWERED_ELSEWHERE for the
/// other members (never missed); in an ORDERED hunt a member whose turn expired missed it.
#[tokio::test]
async fn ring_group_members_see_answered_elsewhere_or_missed() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = e.member("caller").await;
    let caller_phone = e.a.device(&caller, "caller-desk").await;
    let mut members = Vec::new();
    for i in 0..3 {
        let m = e.member(&format!("member-{i}")).await;
        let d = e.a.device(&m, &format!("member-{i}-desk")).await;
        members.push(d);
    }
    let group = |strategy: &str, name: &str| {
        json!({ "name": name, "strategy": strategy, "memberRingSeconds": 10,
                "groupTimeoutSeconds": 60 })
    };
    let all = e
        .create(
            "/v1/enterprise/ring-groups",
            group("SIMULTANEOUS", "Front desk"),
        )
        .await;
    let hunt = e
        .create("/v1/enterprise/ring-groups", group("ORDERED", "Hunt line"))
        .await;
    for g in [&all, &hunt] {
        let id = id_of(g, "ringGroupId");
        let r = e
            .admin(
                Method::PUT,
                &format!("/v1/enterprise/ring-groups/{id}/members"),
                Some(json!({ "expectedVersion": g["version"], "members": members
                    .iter()
                    .map(|d| json!({ "type": "ENTERPRISE_MEMBER", "id": d.principal.to_string() }))
                    .collect::<Vec<_>>() })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    e.extension("2000", "RING_GROUP", id_of(&all, "ringGroupId"))
        .await;
    e.extension("3000", "RING_GROUP", id_of(&hunt, "ringGroupId"))
        .await;
    let dial = |number: &'static str| {
        let (e, caller, phone) = (&e, &caller, &caller_phone);
        async move {
            let r = e
                .call_extension("VOICE", caller, phone, number, &Uuid::new_v4().to_string())
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
            call_id(&r)
        }
    };

    // SIMULTANEOUS: member 1 answers.
    let front = dial("2000").await;
    let offer =
        e.a.invitation_for(front, members[1].endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(front).await;
    assert_eq!(
        e.a.accept(front, offer, &members[1], v).await.status,
        StatusCode::OK
    );
    // ORDERED: member 0's turn expires, member 1 answers.
    let hunted = dial("3000").await;
    e.h().clock.advance(chrono::Duration::seconds(11));
    e.h().state.app.expire_due(100).await.unwrap();
    let offer =
        e.a.invitation_for(hunted, members[1].endpoint)
            .await
            .expect("the hunt moved to member 1");
    let (_, v) = e.a.state(hunted).await;
    assert_eq!(
        e.a.accept(hunted, offer, &members[1], v).await.status,
        StatusCode::OK
    );

    let h0 = app_history(&e.a, members[0].principal, "").await;
    let h1 = app_history(&e.a, members[1].principal, "").await;
    let h2 = app_history(&e.a, members[2].principal, "").await;
    assert_eq!(
        outcome(entry(&h0, front)),
        ("INBOUND", "ANSWERED_ELSEWHERE", false)
    );
    assert_eq!(
        outcome(entry(&h2, front)),
        ("INBOUND", "ANSWERED_ELSEWHERE", false)
    );
    assert_eq!(outcome(entry(&h1, front)), ("INBOUND", "ANSWERED", false));
    assert_eq!(outcome(entry(&h0, hunted)), ("INBOUND", "MISSED", true));
    assert_eq!(outcome(entry(&h1, hunted)), ("INBOUND", "ANSWERED", false));
    // Member 2 was never rung on the hunt line (answered before its turn).
    assert!(!ids(&h2).contains(&hunted.to_string()));
    // Safe enterprise labels: the dialed extension and the ring group.
    let labels = &entry(&h1, front)["enterprise"];
    assert_eq!(labels["entryExtension"], "2000");
    assert_eq!(labels["entryLabel"], "Extension 2000");
    assert_eq!(labels["ringGroupLabel"], "Front desk");
    assert_eq!(labels["phase"], "ANSWERED");
    let caller_history = app_history(&e.a, caller.id().as_uuid(), "").await;
    assert_eq!(
        outcome(entry(&caller_history, front)),
        ("OUTBOUND", "ANSWERED", false)
    );
    // Enterprise destination filters (with and without a principal).
    let front_id = id_of(&all, "ringGroupId");
    let r =
        e.a.get(&format!(
            "/v1/calls?enterpriseDestinationType=RING_GROUP&enterpriseDestinationId={front_id}"
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(ids(&r.json), [front.to_string()]);
    let ext = strings(
        e.h().conn(),
        "SELECT id::text FROM enterprise_extensions WHERE extension_number = '3000'",
    )
    .await
    .pop()
    .unwrap();
    let through = app_history(
        &e.a,
        members[1].principal,
        &format!("&enterpriseDestinationType=EXTENSION&enterpriseDestinationId={ext}"),
    )
    .await;
    assert_eq!(ids(&through), [hunted.to_string()]);
    let r =
        e.a.get(&format!(
            "/v1/calls?enterpriseDestinationType=QUEUE&enterpriseDestinationId={front_id}"
        ))
        .await;
    assert!(ids(&r.json).is_empty());
}

/// §135: the voicemail left on a missed Call is linked for its mailbox's owner only.
#[tokio::test]
async fn voicemail_is_linked_for_the_mailbox_owner_only() {
    let store = Arc::new(cf_object_storage::MemoryObjectStore::new());
    let capture = Arc::new(RecordingCapture::default());
    let (s, c) = (store.clone(), capture.clone());
    let Some(e) = EnterpriseWorld::configured(move |runtime| {
        runtime.artifacts = s;
        runtime.capture = c;
    })
    .await
    else {
        return;
    };
    let vm_admin = common::enterprise::credential(
        e.h(),
        &e.a.w.consumer,
        e.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ],
    )
    .await;
    let caller = e.member("vm-caller").await;
    let caller_phone = e.a.device(&caller, "vm-caller-desk").await;
    let owner = e.member("vm-owner").await;
    let owner_phone = e.a.device(&owner, "vm-owner-desk").await;
    let r = e
        .send(
            vm_admin.secret.expose_once(),
            Method::POST,
            "/v1/enterprise/voicemail/mailboxes",
            Some(
                json!({ "owner": { "type": "PRINCIPAL", "id": owner.id().to_string() },
                         "displayLabel": "Owner box", "maxMessageSeconds": 120,
                         "retentionDays": 30 }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let mailbox = id_of(&r.json, "mailboxId");
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/forwarding-rules",
            Some(json!({ "principalId": owner.id(), "condition": "NO_ANSWER",
                         "destination": { "type": "VOICEMAIL", "id": mailbox.to_string() } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    e.extension("4200", "ENTERPRISE_MEMBER", owner.id()).await;
    let r = e
        .call_extension("VOICE", &caller, &caller_phone, "4200", "vm-1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call = call_id(&r);
    e.h().clock.advance(chrono::Duration::seconds(46));
    e.h().state.app.expire_due(100).await.unwrap();
    let message: Uuid = strings(
        e.h().conn(),
        &format!("SELECT id::text FROM voicemail_messages WHERE call_id = '{call}'"),
    )
    .await
    .pop()
    .expect("a voicemail message")
    .parse()
    .unwrap();
    let audio = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
    let _ = e
        .h()
        .state
        .app
        .deliver_capture(
            message,
            call,
            CapturedMedia {
                content_type: "audio/wav".into(),
                duration_ms: 1500,
                sha256: cf_object_storage::sha256_hex(&audio),
                bytes: audio.clone(),
            },
        )
        .await;

    let owner_history = reconnect_history(&e.a, &owner_phone, "").await;
    let entry_owner = entry(&owner_history, call);
    assert_eq!(outcome(entry_owner), ("INBOUND", "MISSED", true));
    assert_eq!(entry_owner["voicemail"]["messageId"], message.to_string());
    assert_eq!(entry_owner["voicemail"]["readState"], "UNREAD");
    assert_eq!(entry_owner["enterprise"]["entryExtension"], "4200");
    // The caller left the message; the link is not theirs.
    let caller_history = reconnect_history(&e.a, &caller_phone, "").await;
    assert!(entry(&caller_history, call)["voicemail"].is_null());
    // Calls routed to the mailbox.
    let r =
        e.a.get(&format!(
            "/v1/calls?enterpriseDestinationType=VOICEMAIL&enterpriseDestinationId={mailbox}"
        ))
        .await;
    assert_eq!(ids(&r.json), [call.to_string()]);
    // No object key, URL or provider identifier in a history response.
    for text in [owner_history.to_string(), caller_history.to_string()] {
        assert!(!text.contains("voicemail/"), "{text}");
        assert!(!text.contains("http"), "{text}");
    }
}

/// §137: a principal's history is one keyset range of `idx_call_participants_history`
/// (no sort, no scan of the participants table), and `call_created_at` is the Call's own,
/// immutable.
#[tokio::test]
async fn history_reads_one_index_range() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    for _ in 0..3 {
        a.ringing_call("VOICE").await;
    }
    let conn = a.h().conn();
    assert_eq!(
        common::scalar_i64(
            conn,
            "SELECT count(*) FROM call_participants p JOIN calls c ON c.id = p.call_id \
             WHERE p.call_created_at <> c.created_at"
        )
        .await,
        0
    );
    let err = common::exec(
        conn,
        "UPDATE call_participants SET call_created_at = call_created_at + interval '1 day'",
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("is immutable"), "{err}");

    let scope = CallScope::of_platform(&a.w.ctx);
    for (filter, query) in [
        (HistoryFilter::default(), ListCallsQuery::default()),
        (
            HistoryFilter {
                missed: Some(true),
                ..Default::default()
            },
            ListCallsQuery {
                state: Some(CallState::NoAnswer),
                ..Default::default()
            },
        ),
    ] {
        let statement = history_statement(scope, a.w.recipient.id(), &query, filter, None, 25);
        let txn = conn.begin().await.unwrap();
        txn.execute_unprepared("SET LOCAL enable_seqscan = off; SET LOCAL enable_sort = off")
            .await
            .unwrap();
        let rows = txn
            .query_all(Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                format!("EXPLAIN {}", statement.sql),
                statement.values.map(|v| v.0).unwrap_or_default(),
            ))
            .await
            .unwrap();
        let plan: Vec<String> = rows
            .iter()
            .map(|r| r.try_get_by_index::<String>(0).unwrap())
            .collect();
        let text = plan.join("\n");
        txn.rollback().await.unwrap();
        assert!(text.contains("idx_call_participants_history"), "{text}");
        assert!(!text.contains("Seq Scan on call_participants"), "{text}");
        assert!(!text.contains("Sort"), "{text}");
    }
}
