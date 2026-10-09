//! Part F §57 IVR (application / RTC caller path; the SIP path is certified against real
//! Asterisk + SIPp in `enterprise_sip_certification`): flow / version / activation
//! administration and graph validation, prompt, valid key, invalid key with retry,
//! timeout with retry, route to extension / queue, hangup, the step budget bounding a
//! menu cycle, version pinning of a running Call, caller abandonment, keypad
//! authorization and DTMF privacy.

mod common;

use axum::http::{Method, StatusCode};
use calling::calls::CallState;
use common::{
    app::Device,
    enterprise::{conforms, id_of, EnterpriseWorld},
};
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

struct Desk {
    principal: Principal,
    phone: Device,
}

async fn desk(e: &EnterpriseWorld, name: &str) -> Desk {
    let principal = e.member(name).await;
    let phone = e.a.device(&principal, &format!("{name}-desk")).await;
    Desk { principal, phone }
}

async fn flow(e: &EnterpriseWorld) -> Uuid {
    let f = e
        .create(
            "/v1/enterprise/ivr-flows",
            json!({ "name": format!("Main menu {}", tag()) }),
        )
        .await;
    conforms("IvrFlow", &f);
    assert!(f["activeVersionNumber"].is_null());
    id_of(&f, "ivrFlowId")
}

async fn publish(e: &EnterpriseWorld, flow: Uuid, graph: Value) -> (StatusCode, Value) {
    let r = e
        .admin(
            Method::POST,
            &format!("/v1/enterprise/ivr-flows/{flow}/versions"),
            Some(json!({ "graph": graph })),
        )
        .await;
    (r.status, r.json)
}

async fn activate(e: &EnterpriseWorld, flow: Uuid, number: i64) -> Value {
    let v = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/ivr-flows/{flow}"),
            None,
        )
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ivr-flows/{flow}/activation"),
            Some(json!({ "expectedVersion": v, "versionNumber": number })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("IvrFlow", &r.json);
    r.json
}

fn menu(options: Value, timeout: i64, retries: i64) -> Value {
    json!({ "id": "main", "kind": "MENU", "prompt": { "builtin": "MENU" }, "inputTimeoutSeconds": timeout,
            "maxRetries": retries, "options": options, "onInvalid": "bye", "onTimeout": "bye" })
}

fn bye() -> Value {
    json!({ "id": "bye", "kind": "HANGUP", "prompt": { "builtin": "GOODBYE" } })
}

/// A published, active main menu: 1 -> member extension, 2 -> queue, 0 -> hangup.
async fn main_menu(e: &EnterpriseWorld, member: &Desk, queue: Uuid) -> Uuid {
    let ext = e
        .extension(
            &format!("7{}", rand_digits(3)),
            "ENTERPRISE_MEMBER",
            member.principal.id(),
        )
        .await;
    let f = flow(e).await;
    let (status, version) = publish(
        e,
        f,
        json!({ "startNode": "hello", "nodes": [
            { "id": "hello", "kind": "PROMPT", "prompt": { "builtin": "WELCOME" }, "next": "main" },
            menu(json!([{ "digit": "1", "next": "sales" }, { "digit": "2", "next": "support" },
                        { "digit": "0", "next": "bye" }]), 5, 1),
            { "id": "sales", "kind": "ROUTE_EXTENSION", "extensionId": id_of(&ext, "extensionId") },
            { "id": "support", "kind": "ROUTE_QUEUE", "queueId": queue },
            bye(),
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{version}");
    conforms("IvrFlowVersion", &version);
    activate(e, f, 1).await;
    f
}

fn rand_digits(n: usize) -> String {
    let id = Uuid::new_v4();
    id.as_bytes()
        .iter()
        .take(n)
        .map(|b| char::from(b'0' + b % 10))
        .collect()
}

async fn queue(e: &EnterpriseWorld) -> Uuid {
    let q = e
        .create(
            "/v1/enterprise/queues",
            json!({ "name": format!("Support {}", tag()), "maxWaitSeconds": 600,
                    "offerTimeoutSeconds": 20, "capacity": 10 }),
        )
        .await;
    id_of(&q, "queueId")
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

async fn press(e: &EnterpriseWorld, call: Uuid, from: &Device, digit: &str) -> (StatusCode, Value) {
    let r = e
        .send(
            e.caller_token(),
            Method::POST,
            &format!("/v1/calls/{call}/ivr/input"),
            Some(json!({ "endpointId": from.endpoint, "digit": digit })),
        )
        .await;
    (r.status, r.json)
}

async fn session(e: &EnterpriseWorld, call: Uuid) -> (String, Option<String>, i64) {
    let rows = common::enterprise::strings(
        e.h().conn(),
        &format!(
            "SELECT state || '|' || COALESCE(selected_branch, '') || '|' || retries FROM enterprise_ivr_sessions \
              WHERE call_id = '{call}' ORDER BY started_at DESC LIMIT 1"
        ),
    )
    .await;
    let row = rows.first().cloned().unwrap_or_default();
    let mut parts = row.split('|');
    let state = parts.next().unwrap_or_default().to_string();
    let branch = parts.next().filter(|b| !b.is_empty()).map(str::to_string);
    let retries = parts.next().unwrap_or("0").parse().unwrap_or(0);
    (state, branch, retries)
}

async fn tick(e: &EnterpriseWorld) {
    e.h().state.app.expire_due(100).await.unwrap();
}

#[tokio::test]
async fn ivr_flow_administration_and_validation() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let member = desk(&e, "member").await;
    let ext = e
        .extension("1500", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    let ext_id = id_of(&ext, "extensionId");
    let f = flow(&e).await;
    // Invalid graphs: unknown node, multi-key option, duplicate key, unreachable node,
    // input-free loop, script / dialplan / URL content, unknown prompt, foreign
    // destination, unsupported destination kind, excessive size.
    let bad = [
        json!({ "startNode": "main", "nodes": [menu(json!([{ "digit": "1", "next": "ghost" }]), 5, 1), bye()] }),
        json!({ "startNode": "main", "nodes": [menu(json!([{ "digit": "12", "next": "bye" }]), 5, 1), bye()] }),
        json!({ "startNode": "main", "nodes": [menu(json!([{ "digit": "1", "next": "bye" }, { "digit": "1", "next": "main" }]), 5, 1), bye()] }),
        json!({ "startNode": "bye", "nodes": [bye(), menu(json!([{ "digit": "1", "next": "bye" }]), 5, 1)] }),
        json!({ "startNode": "a", "nodes": [
            { "id": "a", "kind": "PROMPT", "prompt": { "builtin": "WELCOME" }, "next": "b" },
            { "id": "b", "kind": "PROMPT", "prompt": { "builtin": "MENU" }, "next": "a" }] }),
        json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "SCRIPT", "code": "rm -rf /" }] }),
        json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "HANGUP", "dialplan": "Dial(PJSIP/1)" }] }),
        json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "PROMPT",
            "prompt": { "builtin": "WELCOME", "url": "https://x.example/a.wav" }, "next": "b" }, bye()] }),
        json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "PROMPT",
            "prompt": { "builtin": "tt-monkeys" }, "next": "bye" }, bye()] }),
        json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "ROUTE_SCHEDULE", "scheduleId": Uuid::new_v4() }] }),
        json!({ "startNode": "main", "nodes": [menu(json!([{ "digit": "1", "next": "bye" }]), 500, 1), bye()] }),
    ];
    for graph in bad {
        let (status, body) = publish(&e, f, graph.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{graph} -> {body}");
    }
    // An unknown / foreign destination is 404 (like another scope's).
    let (status, _) = publish(
        &e,
        f,
        json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "ROUTE_EXTENSION", "extensionId": Uuid::new_v4() }] }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mut nodes: Vec<Value> = (0..33)
        .map(|i| {
            json!({ "id": format!("n{i}"), "kind": "PROMPT", "prompt": { "builtin": "WELCOME" },
                          "next": format!("n{}", i + 1) })
        })
        .collect();
    nodes.push(json!({ "id": "n33", "kind": "HANGUP" }));
    let (status, _) = publish(&e, f, json!({ "startNode": "n0", "nodes": nodes })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A valid graph: version 1, not active until activated.
    let good = json!({ "startNode": "main", "nodes": [
        menu(json!([{ "digit": "1", "next": "sales" }, { "digit": "#", "next": "main" }]), 5, 2),
        { "id": "sales", "kind": "ROUTE_EXTENSION", "extensionId": ext_id },
        bye(),
    ]});
    let (status, v1) = publish(&e, f, good.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{v1}");
    assert_eq!(v1["versionNumber"], 1);
    assert_eq!(v1["active"], false);
    // An extension routed to a flow without an ACTIVE version is refused.
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/extensions",
            Some(json!({ "extensionNumber": "1501", "displayLabel": "Menu",
                         "destination": { "type": "IVR", "id": f } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let flow_view = activate(&e, f, 1).await;
    assert_eq!(flow_view["activeVersionNumber"], 1);
    let (status, v2) = publish(&e, f, good).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(v2["versionNumber"], 2);
    let r = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/ivr-flows/{f}/versions"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    conforms("IvrFlowVersionList", &r.json);
    assert_eq!(r.json["versions"][0]["versionNumber"], 2);
    assert_eq!(r.json["versions"][1]["active"], true);
    let r = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/ivr-flows/{f}/versions/9"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Versions are immutable (even for a buggy writer).
    assert!(common::exec(
        e.h().conn(),
        &format!("UPDATE enterprise_ivr_flow_versions SET node_count = 1 WHERE flow_id = '{f}'")
    )
    .await
    .is_err());
    // Stale activation, read-only administrator, other tenant.
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ivr-flows/{f}/activation"),
            Some(json!({ "expectedVersion": 1, "versionNumber": 2 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = e
        .send(
            e.reader.secret.expose_once(),
            Method::POST,
            &format!("/v1/enterprise/ivr-flows/{f}/versions"),
            Some(json!({ "graph": { "startNode": "bye", "nodes": [bye()] } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let (_, other) = e.other_tenant("ivr-other").await;
    let r = e
        .send(
            other.secret.expose_once(),
            Method::GET,
            &format!("/v1/enterprise/ivr-flows/{f}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = e.admin(Method::GET, "/v1/enterprise/ivr-flows", None).await;
    conforms("IvrFlowList", &r.json);
    // Activation is audited.
    assert!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_admin_audit WHERE resource_id = '{f}' AND operation = 'ACTIVATE'"
        ))
        .await
            >= 1
    );
}

/// Prompt, invalid key (retry with INVALID_OPTION), valid key -> extension on the SAME
/// Call; the member answers.
#[tokio::test]
async fn menu_routes_a_valid_key_after_an_invalid_one() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let member = desk(&e, "member").await;
    let caller = desk(&e, "caller").await;
    let q = queue(&e).await;
    let f = main_menu(&e, &member, q).await;
    e.extension("6000", "IVR", f).await;
    let (call, created) = dial(&e, &caller, "6000").await;
    assert_eq!(created["enterprise"]["phase"], "IVR");
    let ivr = &created["enterprise"]["ivr"];
    assert_eq!(ivr["awaitingInput"], true);
    assert_eq!(ivr["prompts"], json!(["WELCOME", "MENU"]));
    assert_eq!(ivr["options"], json!(["1", "2", "0"]));
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    // Invalid key: retry.
    let (status, body) = press(&e, call, &caller.phone, "9").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    conforms("IvrInputResult", &body);
    assert_eq!(body["accepted"], true);
    assert_eq!(session(&e, call).await, ("ACTIVE".into(), None, 1));
    // Valid key: the member's endpoint rings on the SAME Call.
    let (_, body) = press(&e, call, &caller.phone, "1").await;
    assert_eq!(body["accepted"], true);
    let (state, branch, _) = session(&e, call).await;
    assert_eq!(state, "ROUTED");
    assert_eq!(branch.as_deref(), Some("sales"));
    let invitation =
        e.a.invitation_for(call, member.phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, invitation, &member.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Answered);
    assert_eq!(e.count("SELECT count(*) FROM calls").await, 1);
    // Input after routing is stale: ignored, never an error that leaks state.
    let (_, body) = press(&e, call, &caller.phone, "2").await;
    assert_eq!(body["accepted"], false);
    // DTMF privacy: no column holds the keys pressed (only the branch node id).
    let cols = common::enterprise::strings(
        e.h().conn(),
        "SELECT column_name FROM information_schema.columns WHERE table_name = 'enterprise_ivr_sessions'",
    )
    .await;
    assert!(
        !cols
            .iter()
            .any(|c| c.contains("digit") || c.contains("dtmf") || c.contains("input_value")),
        "{cols:?}"
    );
    let steps = common::enterprise::strings(
        e.h().conn(),
        &format!("SELECT decision || ':' || reason FROM enterprise_routing_steps WHERE call_id = '{call}' ORDER BY step_number"),
    )
    .await;
    assert!(
        steps.contains(&"IVR_ENTER:DESTINATION_RESOLVED".to_string()),
        "{steps:?}"
    );
    assert!(
        steps.contains(&"IVR_ROUTE:BRANCH_SELECTED".to_string()),
        "{steps:?}"
    );
}

/// No key: NO_INPUT + retry; again: onTimeout -> HANGUP (COMPLETED_IN_IVR) once GOODBYE
/// has played. The input timer starts after the queued prompts' nominal length. Invalid
/// keys beyond the retries: onInvalid. Key 2: the Call waits in the queue.
#[tokio::test]
async fn timeouts_retries_hangup_and_queue_route() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let member = desk(&e, "member").await;
    let caller = desk(&e, "caller").await;
    let q = queue(&e).await;
    let f = main_menu(&e, &member, q).await;
    e.extension("6001", "IVR", f).await;

    // WELCOME (2 s) + MENU (2 s) play first, then 5 s for a key.
    let (call, _) = dial(&e, &caller, "6001").await;
    e.h().clock.advance(chrono::Duration::seconds(8));
    tick(&e).await;
    assert_eq!(session(&e, call).await, ("ACTIVE".into(), None, 0));
    e.h().clock.advance(chrono::Duration::seconds(1));
    tick(&e).await;
    assert_eq!(session(&e, call).await, ("ACTIVE".into(), None, 1));
    // NO_INPUT (2 s) + MENU (2 s) + 5 s; then onTimeout -> bye: GOODBYE plays (2 s), keys
    // are no longer taken, then the platform ends the Call.
    e.h().clock.advance(chrono::Duration::seconds(9));
    tick(&e).await;
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    assert_eq!(session(&e, call).await.0, "ACTIVE");
    let (status, body) = press(&e, call, &caller.phone, "1").await;
    assert_eq!(
        (status, body["accepted"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    e.h().clock.advance(chrono::Duration::seconds(2));
    tick(&e).await;
    assert_eq!(e.a.state(call).await.0, CallState::Declined);
    assert_eq!(session(&e, call).await.0, "COMPLETED");
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call}' AND outcome = 'COMPLETED_IN_IVR'"
        ))
        .await,
        1
    );

    // Invalid twice (maxRetries 1): onInvalid -> hangup.
    let (call, _) = dial(&e, &caller, "6001").await;
    press(&e, call, &caller.phone, "7").await;
    press(&e, call, &caller.phone, "8").await;
    e.h().clock.advance(chrono::Duration::seconds(2));
    tick(&e).await;
    assert_eq!(e.a.state(call).await.0, CallState::Declined);

    // 2: the SAME Call enters the queue.
    let (call, _) = dial(&e, &caller, "6001").await;
    let (_, body) = press(&e, call, &caller.phone, "2").await;
    assert_eq!(body["accepted"], true);
    assert_eq!(
        common::enterprise::strings(
            e.h().conn(),
            &format!("SELECT state FROM enterprise_queue_entries WHERE call_id = '{call}'"),
        )
        .await,
        ["WAITING"]
    );
    assert_eq!(session(&e, call).await.1.as_deref(), Some("support"));

    // 0: explicit hangup.
    let (call, _) = dial(&e, &caller, "6001").await;
    press(&e, call, &caller.phone, "0").await;
    e.h().clock.advance(chrono::Duration::seconds(2));
    tick(&e).await;
    assert_eq!(e.a.state(call).await.0, CallState::Declined);
}

/// A menu that loops back to itself through a prompt is bounded by the step budget:
/// pressing forever fails the Call safely.
#[tokio::test]
async fn the_step_budget_bounds_a_menu_cycle() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let f = flow(&e).await;
    let (status, _) = publish(
        &e,
        f,
        json!({ "startNode": "main", "nodes": [
            menu(json!([{ "digit": "1", "next": "again" }]), 5, 1),
            { "id": "again", "kind": "PROMPT", "prompt": { "builtin": "PLEASE_HOLD" }, "next": "main" },
            bye(),
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    activate(&e, f, 1).await;
    e.extension("6002", "IVR", f).await;
    let (call, _) = dial(&e, &caller, "6002").await;
    let mut presses = 0;
    while e.a.state(call).await.0 == CallState::Ringing {
        press(&e, call, &caller.phone, "1").await;
        presses += 1;
        assert!(presses < 40, "the IVR never ended");
    }
    assert_eq!(e.a.state(call).await.0, CallState::Failed);
    assert_eq!(session(&e, call).await.0, "FAILED");
}

/// A running Call keeps its version; the caller's hang-up abandons the session; only the
/// Call's own initiator endpoint may press keys.
#[tokio::test]
async fn version_pinning_abandonment_and_keypad_authorization() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let member = desk(&e, "member").await;
    let caller = desk(&e, "caller").await;
    let q = queue(&e).await;
    let f = main_menu(&e, &member, q).await;
    e.extension("6003", "IVR", f).await;
    let (call, _) = dial(&e, &caller, "6003").await;
    // Version 2 (only a hangup) becomes active while the Call is in version 1.
    let (status, _) = publish(&e, f, json!({ "startNode": "bye", "nodes": [bye()] })).await;
    assert_eq!(status, StatusCode::CREATED);
    activate(&e, f, 2).await;
    // Other endpoints / tenants cannot press; the scope is required.
    let (status, _) = press(&e, call, &member.phone, "1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, other) = e.other_tenant("ivr-keys").await;
    let r = e
        .send(
            other.secret.expose_once(),
            Method::POST,
            &format!("/v1/calls/{call}/ivr/input"),
            Some(json!({ "endpointId": caller.phone.endpoint, "digit": "1" })),
        )
        .await;
    assert!(
        r.status == StatusCode::NOT_FOUND || r.status == StatusCode::FORBIDDEN,
        "{}",
        r.text
    );
    let r = e
        .send(
            e.admin_token(),
            Method::POST,
            &format!("/v1/calls/{call}/ivr/input"),
            Some(json!({ "endpointId": caller.phone.endpoint, "digit": "1" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = e
        .send(
            e.caller_token(),
            Method::POST,
            &format!("/v1/calls/{call}/ivr/input"),
            Some(json!({ "endpointId": caller.phone.endpoint, "digit": "11" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // Version 1 still routes key 1 to the member.
    let (_, body) = press(&e, call, &caller.phone, "1").await;
    assert_eq!(body["accepted"], true);
    assert!(e
        .a
        .invitation_for(call, member.phone.endpoint)
        .await
        .is_some());
    // A new Call runs version 2 (straight hangup): GOODBYE plays, then the Call ends.
    let (call2, _) = dial(&e, &caller, "6003").await;
    assert_eq!(e.a.state(call2).await.0, CallState::Ringing);
    e.h().clock.advance(chrono::Duration::seconds(2));
    tick(&e).await;
    assert_eq!(e.a.state(call2).await.0, CallState::Declined);
    // The caller hanging up during GOODBYE was served by the IVR (not abandoned).
    let (call2b, _) = dial(&e, &caller, "6003").await;
    let (_, v) = e.a.state(call2b).await;
    let r = e.a.command(call2b, "cancel", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(session(&e, call2b).await.0, "COMPLETED");
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call2b}' AND outcome = 'COMPLETED_IN_IVR'"
        ))
        .await,
        1
    );
    // Caller hangs up during the IVR: ABANDONED.
    let (_, _) = publish(
        &e,
        f,
        json!({ "startNode": "main", "nodes": [menu(json!([{ "digit": "1", "next": "bye" }]), 5, 1), bye()] }),
    )
    .await;
    activate(&e, f, 3).await;
    let (call3, _) = dial(&e, &caller, "6003").await;
    let (_, v) = e.a.state(call3).await;
    let r = e.a.command(call3, "cancel", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(session(&e, call3).await.0, "ABANDONED");
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call3}' AND outcome = 'ABANDONED'"
        ))
        .await,
        1
    );
    // A disabled flow: new Calls fail safe.
    let v = e
        .admin(Method::GET, &format!("/v1/enterprise/ivr-flows/{f}"), None)
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/ivr-flows/{f}?expectedVersion={v}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (call4, _) = dial(&e, &caller, "6003").await;
    assert_eq!(e.a.state(call4).await.0, CallState::Failed);
}

/// Enterprise SIP device bindings (administration; the SIP path itself is certified with
/// real Asterisk + SIPp).
#[tokio::test]
async fn sip_device_bindings() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let member = desk(&e, "member").await;
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/sip-devices",
            Some(
                json!({ "deviceRef": "dev-front-desk-01", "principalId": member.principal.id(),
                         "displayLabel": "Front desk" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms("SipDevice", &r.json);
    let id = id_of(&r.json, "sipDeviceId");
    for body in [
        json!({ "deviceRef": "sip:frontdesk@10.0.0.5", "principalId": member.principal.id(), "displayLabel": "x" }),
        json!({ "deviceRef": "+2348012345678", "principalId": member.principal.id(), "displayLabel": "x" }),
        json!({ "deviceRef": "dev-x1", "principalId": member.principal.id(), "displayLabel": "x" }),
    ] {
        let r = e
            .admin(Method::POST, "/v1/enterprise/sip-devices", Some(body))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    }
    // One ACTIVE binding per device reference.
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/sip-devices",
            Some(
                json!({ "deviceRef": "dev-front-desk-01", "principalId": member.principal.id(),
                         "displayLabel": "Again" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/sip-devices",
            Some(json!({ "deviceRef": "dev-other-01", "principalId": Uuid::new_v4(), "displayLabel": "x" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = e
        .admin(Method::GET, "/v1/enterprise/sip-devices", None)
        .await;
    conforms("SipDeviceList", &r.json);
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/sip-devices/{id}?expectedVersion=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DISABLED");
}
