//! Part F §56 call queues: configuration, FIFO order and authoritative positions, offers
//! to available agents (one pending offer per entry and per agent), offer timeout and
//! agent rotation, maximum wait with overflow or timeout, capacity (overflow / BUSY),
//! abandonment, paused / unreachable agents, ring-group fallback into a queue, and the
//! multi-instance races (two dispatchers, abandon vs answer).

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
    // Unique names without digit runs (names never look like telephone numbers).
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

async fn queue(
    e: &EnterpriseWorld,
    max_wait: i64,
    offer: i64,
    capacity: i64,
    overflow: Option<Value>,
) -> Value {
    let mut body = json!({ "name": format!("Queue {}", tag()), "maxWaitSeconds": max_wait,
                           "offerTimeoutSeconds": offer, "capacity": capacity });
    if let Some(o) = overflow {
        body["overflow"] = o;
    }
    e.create("/v1/enterprise/queues", body).await
}

async fn version(e: &EnterpriseWorld, path: &str) -> i64 {
    e.admin(Method::GET, path, None).await.json["version"]
        .as_i64()
        .unwrap()
}

async fn set_agents(e: &EnterpriseWorld, q: &Value, members: Vec<Value>) -> Value {
    let id = id_of(q, "queueId");
    let v = version(e, &format!("/v1/enterprise/queues/{id}")).await;
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/queues/{id}/members"),
            Some(json!({ "expectedVersion": v, "members": members })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json
}

fn agent(p: &Principal) -> Value {
    json!({ "principalId": p.id().to_string() })
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

async fn entries(e: &EnterpriseWorld, q: &Value, state: &str) -> Vec<Value> {
    let r = e
        .admin(
            Method::GET,
            &format!(
                "/v1/enterprise/queues/{}/entries?state={state}",
                id_of(q, "queueId")
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("QueueEntryList", &r.json);
    r.json["entries"].as_array().unwrap().clone()
}

async fn entry_state(e: &EnterpriseWorld, call: Uuid) -> String {
    common::enterprise::strings(
        e.h().conn(),
        &format!("SELECT state FROM enterprise_queue_entries WHERE call_id = '{call}' ORDER BY seq DESC LIMIT 1"),
    )
    .await
    .pop()
    .unwrap_or_default()
}

async fn tick(e: &EnterpriseWorld) {
    e.h().state.app.expire_due(100).await.unwrap();
}

#[tokio::test]
async fn queue_configuration() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let a = desk(&e, "agent-a").await;
    let q = queue(&e, 600, 20, 50, None).await;
    conforms("Queue", &q);
    assert_eq!(q["waiting"], 0);
    assert_eq!(q["version"], 1);
    let id = id_of(&q, "queueId");

    // Bounds, raw numbers, unknown kinds and fields are refused.
    for body in [
        json!({ "name": "x", "maxWaitSeconds": 5, "offerTimeoutSeconds": 5, "capacity": 1 }),
        json!({ "name": "x", "maxWaitSeconds": 60, "offerTimeoutSeconds": 121, "capacity": 1 }),
        json!({ "name": "x", "maxWaitSeconds": 20, "offerTimeoutSeconds": 30, "capacity": 1 }),
        json!({ "name": "x", "maxWaitSeconds": 60, "offerTimeoutSeconds": 10, "capacity": 1001 }),
        json!({ "name": "x", "maxWaitSeconds": 60, "offerTimeoutSeconds": 10, "capacity": 1,
                "overflow": { "type": "PHONE", "number": "+2348012345678" } }),
        json!({ "name": "x", "maxWaitSeconds": 60, "offerTimeoutSeconds": 10, "capacity": 1,
                "overflow": { "type": "PRINCIPAL", "id": a.principal.id().to_string() } }),
        json!({ "name": "x", "maxWaitSeconds": 60, "offerTimeoutSeconds": 10, "capacity": 1,
                "callerNumber": "+2348012345678" }),
        json!({ "name": "+2348012345678", "maxWaitSeconds": 60, "offerTimeoutSeconds": 10, "capacity": 1 }),
    ] {
        let r = e
            .admin(Method::POST, "/v1/enterprise/queues", Some(body.clone()))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body} -> {}", r.text);
    }
    // Unknown overflow target: 404 (same as another scope's).
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/queues",
            Some(
                json!({ "name": "x", "maxWaitSeconds": 60, "offerTimeoutSeconds": 10, "capacity": 1,
                         "overflow": dest("EXTENSION", Uuid::new_v4()) }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // A queue never overflows to itself.
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/queues/{id}"),
            Some(json!({ "expectedVersion": 1, "overflow": dest("QUEUE", id) })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // Another queue as overflow is fine; a stale version is 409.
    let q2 = queue(&e, 60, 10, 5, None).await;
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/queues/{id}"),
            Some(
                json!({ "expectedVersion": 1, "overflow": dest("QUEUE", id_of(&q2, "queueId")),
                         "capacity": 10 }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["version"], 2);
    assert_eq!(r.json["capacity"], 10);
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/queues/{id}"),
            Some(json!({ "expectedVersion": 1, "capacity": 11 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "VERSION_CONFLICT");

    // Agents: distinct, bounded, ACTIVE principals of this scope.
    let b = desk(&e, "agent-b").await;
    let view = set_agents(
        &e,
        &q,
        vec![
            agent(&a.principal),
            json!({ "principalId": b.principal.id().to_string(), "status": "PAUSED" }),
        ],
    )
    .await;
    conforms("Queue", &view);
    assert_eq!(view["members"][1]["status"], "PAUSED");
    let v = view["version"].as_i64().unwrap();
    for members in [
        json!([agent(&a.principal), agent(&a.principal)]),
        json!([{ "principalId": a.principal.id().to_string(), "status": "BUSY" }]),
        json!((0..101)
            .map(|_| json!({ "principalId": Uuid::new_v4() }))
            .collect::<Vec<_>>()),
    ] {
        let r = e
            .admin(
                Method::PUT,
                &format!("/v1/enterprise/queues/{id}/members"),
                Some(json!({ "expectedVersion": v, "members": members })),
            )
            .await;
        assert!(
            r.status == StatusCode::BAD_REQUEST || r.status == StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            r.text
        );
    }
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/queues/{id}/members"),
            Some(json!({ "expectedVersion": v, "members": [{ "principalId": Uuid::new_v4() }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    // Read-only administrators read but never write; another tenant sees nothing.
    let r = e
        .send(
            e.reader.secret.expose_once(),
            Method::GET,
            &format!("/v1/enterprise/queues/{id}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = e
        .send(
            e.reader.secret.expose_once(),
            Method::POST,
            "/v1/enterprise/queues",
            Some(json!({ "name": "x", "maxWaitSeconds": 60, "offerTimeoutSeconds": 10, "capacity": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let (_, other) = e.other_tenant("queue-other").await;
    for (method, path) in [
        (Method::GET, format!("/v1/enterprise/queues/{id}")),
        (Method::GET, format!("/v1/enterprise/queues/{id}/entries")),
    ] {
        let r = e
            .send(other.secret.expose_once(), method, &path, None)
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    }
    let r = e
        .admin(Method::GET, "/v1/enterprise/queues?limit=1", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("QueueList", &r.json);
    assert!(r.json["nextCursor"].is_string());
    assert!(entries(&e, &q, "ACTIVE").await.is_empty());

    // Disable (needs the current version), audited.
    let v = version(&e, &format!("/v1/enterprise/queues/{id}")).await;
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/queues/{id}?expectedVersion={v}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DISABLED");
    assert!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_admin_audit WHERE resource_id = '{id}' AND resource_type = 'QUEUE'"
        ))
        .await
            >= 4
    );
}

/// FIFO with one agent: one offer at a time, the others wait with authoritative
/// positions; a busy agent receives nothing until their Call ends.
#[tokio::test]
async fn fifo_positions_and_one_offer_at_a_time() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let ag = desk(&e, "agent").await;
    let q = queue(&e, 600, 20, 10, None).await;
    set_agents(&e, &q, vec![agent(&ag.principal)]).await;
    e.extension("5000", "QUEUE", id_of(&q, "queueId")).await;
    let mut callers = Vec::new();
    let mut calls = Vec::new();
    for i in 0..3 {
        let c = desk(&e, &format!("caller-{i}")).await;
        let (call, created) = dial(&e, &c, "5000").await;
        assert_eq!(created["enterprise"]["phase"], "QUEUED");
        assert_eq!(created["enterprise"]["destinationType"], "QUEUE");
        assert_eq!(created["enterprise"]["queue"]["position"], i + 1);
        assert_eq!(e.a.state(call).await.0, CallState::Ringing);
        callers.push(c);
        calls.push(call);
    }
    // Only the head is offered; positions are 1..3 in arrival order.
    let active = entries(&e, &q, "ACTIVE").await;
    assert_eq!(active.len(), 3);
    for (i, entry) in active.iter().enumerate() {
        assert_eq!(entry["callId"], calls[i].to_string());
        assert_eq!(entry["position"], (i + 1) as i64);
    }
    assert_eq!(active[0]["state"], "OFFERING");
    assert_eq!(active[1]["state"], "WAITING");
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_queue_offers WHERE principal_id = '{}' AND outcome = 'PENDING'",
            ag.principal.id()
        ))
        .await,
        1
    );
    // The agent answers the head; the agent is now busy, so nothing else is offered.
    let invitation =
        e.a.invitation_for(calls[0], ag.phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(calls[0]).await;
    let r = e.a.accept(calls[0], invitation, &ag.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(entry_state(&e, calls[0]).await, "ANSWERED");
    tick(&e).await;
    assert_eq!(entry_state(&e, calls[1]).await, "WAITING");
    let active = entries(&e, &q, "ACTIVE").await;
    assert_eq!(active.len(), 2);
    assert_eq!(active[0]["position"], 1);
    assert_eq!(active[0]["callId"], calls[1].to_string());
    // The answered Call ends: the next caller (FIFO) is offered, never the third.
    let (_, v) = e.a.state(calls[0]).await;
    let r = e.a.command(calls[0], "end", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    tick(&e).await;
    assert_eq!(entry_state(&e, calls[1]).await, "OFFERING");
    assert_eq!(entry_state(&e, calls[2]).await, "WAITING");
    assert!(e
        .a
        .invitation_for(calls[1], ag.phone.endpoint)
        .await
        .is_some());
    assert!(e
        .a
        .invitation_for(calls[2], ag.phone.endpoint)
        .await
        .is_none());
    let finished = entries(&e, &q, "FINISHED").await;
    assert_eq!(finished[0]["state"], "ANSWERED");
    assert_eq!(
        finished[0]["assignedPrincipalId"],
        ag.principal.id().to_string()
    );
    assert!(finished[0]["position"].is_null());
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_queue_offers o JOIN enterprise_queue_entries q ON q.id = o.entry_id \
              WHERE q.call_id = '{}' AND o.outcome = 'ACCEPTED'",
            calls[0]
        ))
        .await,
        1
    );
    assert!(
        calling::metrics::observations(
            "calling_enterprise_queue_wait_seconds",
            &[("result", "ANSWERED")]
        ) >= 1
    );
}

/// An unanswered offer expires (server time); the next available agent is offered the
/// SAME Call; the agent who missed it cools down first. Declines rotate the same way.
#[tokio::test]
async fn offer_timeout_and_decline_rotate_agents() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let a = desk(&e, "agent-a").await;
    let b = desk(&e, "agent-b").await;
    let paused = desk(&e, "agent-paused").await;
    let unreachable = e.member("agent-no-device").await;
    let q = queue(&e, 600, 10, 10, None).await;
    set_agents(
        &e,
        &q,
        vec![
            json!({ "principalId": paused.principal.id().to_string(), "status": "PAUSED" }),
            json!({ "principalId": unreachable.id().to_string() }),
            agent(&a.principal),
            agent(&b.principal),
        ],
    )
    .await;
    e.extension("5001", "QUEUE", id_of(&q, "queueId")).await;
    let caller = desk(&e, "caller").await;
    let (call, _) = dial(&e, &caller, "5001").await;
    // A paused agent and an agent without a device are never offered.
    assert!(e
        .a
        .invitation_for(call, paused.phone.endpoint)
        .await
        .is_none());
    assert!(e.a.invitation_for(call, a.phone.endpoint).await.is_some());
    e.h().clock.advance(chrono::Duration::seconds(11));
    tick(&e).await;
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    assert!(e.a.invitation_for(call, a.phone.endpoint).await.is_none());
    let to_b = e.a.invitation_for(call, b.phone.endpoint).await.unwrap();
    // B declines: back to A (B, who just declined, cools down first), same Call.
    let (_, v) = e.a.state(call).await;
    let r = e.a.decline(call, to_b, &b.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    let to_a = e.a.invitation_for(call, a.phone.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, to_a, &a.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Answered);
    let outcomes = common::enterprise::strings(
        e.h().conn(),
        &format!(
            "SELECT o.outcome FROM enterprise_queue_offers o JOIN enterprise_queue_entries q \
               ON q.id = o.entry_id WHERE q.call_id = '{call}' ORDER BY o.generation"
        ),
    )
    .await;
    assert_eq!(outcomes, ["EXPIRED", "DECLINED", "ACCEPTED"]);
    assert_eq!(
        e.count("SELECT count(*) FROM calls").await,
        1,
        "one logical Call"
    );
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_answer_group_claims WHERE call_id = '{call}'"
        ))
        .await,
        1
    );
}

/// Maximum wait: the overflow destination takes the SAME Call (OVERFLOWED); without one
/// the Call ends NO_ANSWER (TIMED_OUT). A pending offer is withdrawn first.
#[tokio::test]
async fn max_wait_overflows_or_times_out() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let busy_agent = desk(&e, "agent").await;
    let backup = desk(&e, "backup").await;
    let backup_ext = e
        .extension("3000", "ENTERPRISE_MEMBER", backup.principal.id())
        .await;
    let q = queue(
        &e,
        30,
        10,
        10,
        Some(dest("EXTENSION", id_of(&backup_ext, "extensionId"))),
    )
    .await;
    set_agents(&e, &q, vec![agent(&busy_agent.principal)]).await;
    e.extension("5002", "QUEUE", id_of(&q, "queueId")).await;
    let caller = desk(&e, "caller").await;
    let (call, _) = dial(&e, &caller, "5002").await;
    assert!(e
        .a
        .invitation_for(call, busy_agent.phone.endpoint)
        .await
        .is_some());
    // The offer stays unanswered until the maximum wait passes.
    for _ in 0..3 {
        e.h().clock.advance(chrono::Duration::seconds(10));
        tick(&e).await;
    }
    e.h().clock.advance(chrono::Duration::seconds(1));
    tick(&e).await;
    assert_eq!(entry_state(&e, call).await, "OVERFLOWED");
    assert!(e
        .a
        .invitation_for(call, busy_agent.phone.endpoint)
        .await
        .is_none());
    let to_backup =
        e.a.invitation_for(call, backup.phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, to_backup, &backup.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_queue_offers o JOIN enterprise_queue_entries q ON q.id = o.entry_id \
              WHERE q.call_id = '{call}' AND o.outcome = 'PENDING'"
        ))
        .await,
        0
    );

    // Without overflow: TIMED_OUT and NO_ANSWER.
    let q2 = queue(&e, 20, 10, 10, None).await;
    e.extension("5003", "QUEUE", id_of(&q2, "queueId")).await;
    let (call, _) = dial(&e, &caller, "5003").await;
    e.h().clock.advance(chrono::Duration::seconds(21));
    tick(&e).await;
    assert_eq!(entry_state(&e, call).await, "TIMED_OUT");
    assert_eq!(e.a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call}' AND outcome = 'NO_ANSWER'"
        ))
        .await,
        1
    );
}

/// Capacity: a full queue sends the Call to its overflow, or ends it BUSY.
#[tokio::test]
async fn full_queue_overflows_or_is_busy() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let overflow_q = queue(&e, 60, 10, 5, None).await;
    let q = queue(
        &e,
        60,
        10,
        1,
        Some(dest("QUEUE", id_of(&overflow_q, "queueId"))),
    )
    .await;
    e.extension("5004", "QUEUE", id_of(&q, "queueId")).await;
    let c1 = desk(&e, "caller-one").await;
    let c2 = desk(&e, "caller-two").await;
    let (first, _) = dial(&e, &c1, "5004").await;
    let (second, created) = dial(&e, &c2, "5004").await;
    assert_eq!(entry_state(&e, first).await, "WAITING");
    assert_eq!(
        created["enterprise"]["queue"]["queueId"],
        id_of(&overflow_q, "queueId").to_string()
    );
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_routing_steps WHERE call_id = '{second}' AND decision = 'QUEUE_FULL'"
        ))
        .await,
        1
    );
    // No overflow configured: BUSY.
    let small = queue(&e, 60, 10, 1, None).await;
    e.extension("5005", "QUEUE", id_of(&small, "queueId")).await;
    let c3 = desk(&e, "caller-three").await;
    dial(&e, &c1, "5005").await;
    let (busy, _) = dial(&e, &c3, "5005").await;
    assert_eq!(e.a.state(busy).await.0, CallState::Busy);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{busy}' AND outcome = 'BUSY'"
        ))
        .await,
        1
    );
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_queue_entries WHERE call_id = '{busy}'"
        ))
        .await,
        0
    );
}

/// The caller hangs up while waiting / while an agent is offered the Call: ABANDONED,
/// the pending offer is cancelled and the agent can no longer answer.
#[tokio::test]
async fn caller_abandons_while_waiting_or_offered() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let ag = desk(&e, "agent").await;
    let q = queue(&e, 600, 20, 10, None).await;
    set_agents(&e, &q, vec![agent(&ag.principal)]).await;
    e.extension("5006", "QUEUE", id_of(&q, "queueId")).await;
    let c1 = desk(&e, "caller-one").await;
    let c2 = desk(&e, "caller-two").await;
    let (offered, _) = dial(&e, &c1, "5006").await;
    let (waiting, _) = dial(&e, &c2, "5006").await;
    let invitation =
        e.a.invitation_for(offered, ag.phone.endpoint)
            .await
            .unwrap();
    // Abandon while waiting (position 2).
    let (_, v) = e.a.state(waiting).await;
    let r = e.a.command(waiting, "cancel", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(entry_state(&e, waiting).await, "ABANDONED");
    // Abandon while the agent is being offered the Call.
    let (_, v) = e.a.state(offered).await;
    let r = e.a.command(offered, "cancel", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(entry_state(&e, offered).await, "ABANDONED");
    let (_, v) = e.a.state(offered).await;
    let r = e.a.accept(offered, invitation, &ag.phone, v).await;
    assert_ne!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_queue_offers WHERE call_id = '{offered}' AND outcome = 'CANCELLED'"
        ))
        .await,
        1
    );
    assert_eq!(
        e.count("SELECT count(*) FROM enterprise_call_contexts WHERE outcome = 'ABANDONED'")
            .await,
        2
    );
    assert!(entries(&e, &q, "ACTIVE").await.is_empty());
    // The freed agent receives nothing stale.
    tick(&e).await;
    assert_eq!(
        e.count("SELECT count(*) FROM enterprise_queue_offers WHERE outcome = 'PENDING'")
            .await,
        0
    );
}

/// A ring group's fallback is a queue: the SAME Call waits there and is offered.
#[tokio::test]
async fn ring_group_fallback_into_queue() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let member = desk(&e, "member").await;
    let ag = desk(&e, "agent").await;
    let q = queue(&e, 600, 20, 10, None).await;
    set_agents(&e, &q, vec![agent(&ag.principal)]).await;
    let g = e
        .create(
            "/v1/enterprise/ring-groups",
            json!({ "name": format!("Group {}", tag()), "strategy": "SIMULTANEOUS",
                    "memberRingSeconds": 10, "groupTimeoutSeconds": 10,
                    "fallback": dest("QUEUE", id_of(&q, "queueId")) }),
        )
        .await;
    let gid = id_of(&g, "ringGroupId");
    let v = version(&e, &format!("/v1/enterprise/ring-groups/{gid}")).await;
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ring-groups/{gid}/members"),
            Some(json!({ "expectedVersion": v,
                         "members": [dest("ENTERPRISE_MEMBER", member.principal.id())] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.extension("2100", "RING_GROUP", gid).await;
    let caller = desk(&e, "caller").await;
    let (call, _) = dial(&e, &caller, "2100").await;
    assert!(e
        .a
        .invitation_for(call, member.phone.endpoint)
        .await
        .is_some());
    e.h().clock.advance(chrono::Duration::seconds(11));
    tick(&e).await;
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    assert_eq!(entry_state(&e, call).await, "OFFERING");
    let invitation = e.a.invitation_for(call, ag.phone.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, invitation, &ag.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.count("SELECT count(*) FROM calls").await, 1);
    let steps = common::enterprise::strings(
        e.h().conn(),
        &format!(
            "SELECT decision FROM enterprise_routing_steps WHERE call_id = '{call}' ORDER BY step_number"
        ),
    )
    .await;
    assert!(steps.contains(&"FALLBACK".to_string()), "{steps:?}");
    assert!(steps.contains(&"QUEUE_ENTER".to_string()), "{steps:?}");
    assert_eq!(steps.last().unwrap(), "ANSWERED");
}

/// Disabled queue: no new Call enters it (fails safe); one already waiting still drains.
#[tokio::test]
async fn disabled_queue_refuses_new_calls_but_drains() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let q = queue(&e, 20, 10, 10, None).await;
    e.extension("5007", "QUEUE", id_of(&q, "queueId")).await;
    let caller = desk(&e, "caller").await;
    let (waiting, _) = dial(&e, &caller, "5007").await;
    let id = id_of(&q, "queueId");
    let v = version(&e, &format!("/v1/enterprise/queues/{id}")).await;
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/queues/{id}?expectedVersion={v}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let other = desk(&e, "caller-two").await;
    let (refused, _) = dial(&e, &other, "5007").await;
    assert_eq!(e.a.state(refused).await.0, CallState::Failed);
    e.h().clock.advance(chrono::Duration::seconds(21));
    tick(&e).await;
    assert_eq!(entry_state(&e, waiting).await, "TIMED_OUT");
    assert_eq!(e.a.state(waiting).await.0, CallState::NoAnswer);
}

/// Two dispatchers (instances) on the same queue at the same instant: every entry is
/// offered at most once and no agent holds two offers — five rounds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_dispatchers_never_double_offer() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let a = desk(&e, "agent-a").await;
    let b = desk(&e, "agent-b").await;
    let q = queue(&e, 600, 20, 50, None).await;
    let qid = id_of(&q, "queueId");
    e.extension("5008", "QUEUE", qid).await;
    for round in 0..5 {
        let mut calls = Vec::new();
        for i in 0..3 {
            let c = desk(&e, &format!("caller-{round}-{i}")).await;
            calls.push(dial(&e, &c, "5008").await.0);
        }
        // Agents join only now, so both dispatchers race for the same waiting entries.
        set_agents(&e, &q, vec![agent(&a.principal), agent(&b.principal)]).await;
        let app = &e.h().state.app;
        tokio::join!(app.dispatch_queue(qid), app.dispatch_queue(qid), tick(&e));
        assert_eq!(
            e.count(&format!(
                "SELECT count(*) FROM enterprise_queue_offers WHERE queue_id = '{qid}' AND outcome = 'PENDING'"
            ))
            .await,
            2,
            "round {round}"
        );
        assert_eq!(
            e.count(
                "SELECT count(*) FROM (SELECT entry_id FROM enterprise_queue_offers WHERE outcome = 'PENDING' \
                  GROUP BY entry_id HAVING count(*) > 1) d"
            )
            .await,
            0
        );
        // FIFO: the two oldest entries are the ones offered.
        assert_eq!(entry_state(&e, calls[0]).await, "OFFERING", "round {round}");
        assert_eq!(entry_state(&e, calls[1]).await, "OFFERING", "round {round}");
        assert_eq!(entry_state(&e, calls[2]).await, "WAITING", "round {round}");
        for call in calls {
            let (_, v) = e.a.state(call).await;
            let r = e.a.command(call, "cancel", v).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        }
        set_agents(&e, &q, vec![]).await;
    }
    // The database refuses a second pending offer for one agent even from a buggy writer.
    let c = desk(&e, "caller-x").await;
    let (call, _) = dial(&e, &c, "5008").await;
    let entry = common::enterprise::strings(
        e.h().conn(),
        &format!("SELECT id::text FROM enterprise_queue_entries WHERE call_id = '{call}'"),
    )
    .await
    .pop()
    .unwrap();
    let insert = |principal: Uuid, generation: i32| {
        format!(
            "INSERT INTO enterprise_queue_offers (id, tenant_id, application_id, environment_id, entry_id, \
             queue_id, call_id, principal_id, generation, outcome, offered_at, expires_at) \
             SELECT gen_random_uuid(), tenant_id, application_id, environment_id, id, queue_id, call_id, \
             '{principal}', {generation}, 'PENDING', now(), now() + interval '10 seconds' \
             FROM enterprise_queue_entries WHERE id = '{entry}'"
        )
    };
    common::exec(e.h().conn(), &insert(a.principal.id().as_uuid(), 1))
        .await
        .unwrap();
    // A second pending offer of the same entry (another agent) is refused...
    assert!(
        common::exec(e.h().conn(), &insert(b.principal.id().as_uuid(), 2))
            .await
            .is_err()
    );
    // ... and so is a pending offer of another entry to an agent who holds one.
    let c2 = desk(&e, "caller-y").await;
    let (call2, _) = dial(&e, &c2, "5008").await;
    let entry2 = common::enterprise::strings(
        e.h().conn(),
        &format!("SELECT id::text FROM enterprise_queue_entries WHERE call_id = '{call2}'"),
    )
    .await
    .pop()
    .unwrap();
    let again = insert(a.principal.id().as_uuid(), 1).replace(&entry, &entry2);
    assert!(common::exec(e.h().conn(), &again).await.is_err());
    // A final offer never changes.
    assert!(common::exec(
        e.h().conn(),
        &format!(
            "UPDATE enterprise_queue_offers SET outcome = 'EXPIRED', resolved_at = now() WHERE entry_id = '{entry}'"
        )
    )
    .await
    .is_ok());
    assert!(common::exec(
        e.h().conn(),
        &format!(
            "UPDATE enterprise_queue_offers SET outcome = 'ACCEPTED' WHERE entry_id = '{entry}'"
        )
    )
    .await
    .is_err());
}

/// The caller hangs up at the same instant the agent answers: exactly one wins and the
/// entry's final state agrees with the Call — five rounds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandon_and_answer_race_is_consistent() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let ag = desk(&e, "agent").await;
    let q = queue(&e, 600, 20, 10, None).await;
    set_agents(&e, &q, vec![agent(&ag.principal)]).await;
    e.extension("5009", "QUEUE", id_of(&q, "queueId")).await;
    for round in 0..5 {
        let caller = desk(&e, &format!("caller-{round}")).await;
        let (call, _) = dial(&e, &caller, "5009").await;
        let invitation = e.a.invitation_for(call, ag.phone.endpoint).await.unwrap();
        let (_, v) = e.a.state(call).await;
        let (cancel, accept) = tokio::join!(
            e.a.command(call, "cancel", v),
            e.a.accept(call, invitation, &ag.phone, v)
        );
        let ok = [&cancel, &accept]
            .iter()
            .filter(|r| r.status == StatusCode::OK)
            .count();
        assert_eq!(ok, 1, "round {round}: {} / {}", cancel.text, accept.text);
        let state = e.a.state(call).await.0;
        let entry = entry_state(&e, call).await;
        if accept.status == StatusCode::OK {
            assert_eq!(state, CallState::Answered);
            assert_eq!(entry, "ANSWERED");
            let (_, v) = e.a.state(call).await;
            let r = e.a.command(call, "end", v).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        } else {
            assert_eq!(state, CallState::Cancelled);
            assert_eq!(entry, "ABANDONED");
        }
        tick(&e).await;
    }
}
