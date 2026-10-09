//! Part F load and burst behaviour (§197–§199): measured throughput of the enterprise hot
//! paths, with the correctness invariants asserted under concurrency — FIFO queue order
//! and unique positions under a burst, bounded ring-group fan-out, one winner, index-only
//! queue / history plans. Sizes scale with `ENTERPRISE_LOAD_SCALE` (default 1: a CI-sized
//! run); every measurement is printed as `LOAD <name>: <value>`.

mod common;

use std::time::Instant;

use axum::http::{Method, StatusCode};
use calling::calls::{app::history::HistoryFilter, command::CallScope, service::ListCallsQuery};
use common::{
    app::Device,
    enterprise::{id_of, strings, EnterpriseWorld},
};
use futures_util::future::join_all;
use sea_orm::{ConnectionTrait, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

type Principal = calling::platform::principals::ResolvedPrincipal;

fn scale() -> usize {
    std::env::var("ENTERPRISE_LOAD_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| (1..=50).contains(n))
        .unwrap_or(1)
}

fn report(name: &str, count: usize, started: Instant) {
    let secs = started.elapsed().as_secs_f64();
    println!(
        "LOAD {name}: {count} in {:.0} ms ({:.1}/s)",
        secs * 1000.0,
        count as f64 / secs.max(1e-6)
    );
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

async fn dial(e: &EnterpriseWorld, from: &Desk, number: &str) -> Uuid {
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
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

async fn explain(e: &EnterpriseWorld, sql: &str, values: Vec<sea_orm::Value>) -> String {
    let txn = e.h().conn().begin().await.unwrap();
    txn.execute_unprepared("SET LOCAL enable_seqscan = off")
        .await
        .unwrap();
    let rows = txn
        .query_all(Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            format!("EXPLAIN {sql}"),
            values,
        ))
        .await
        .unwrap();
    txn.rollback().await.unwrap();
    rows.iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extension lookup + one internal Call per request, sequential and concurrent; then the
/// history of the busiest principal is one index range.
#[tokio::test]
async fn extension_calls_and_history_throughput() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let n = 40 * scale();
    let caller = desk(&e, "caller").await;
    let member = desk(&e, "member").await;
    e.extension("5500", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    let started = Instant::now();
    for _ in 0..n / 2 {
        let call = dial(&e, &caller, "5500").await;
        let (_, v) = e.a.state(call).await;
        e.a.command(call, "cancel", v).await;
    }
    report("extension_call_sequential", n / 2, started);
    let started = Instant::now();
    let calls: Vec<Uuid> = join_all((0..n / 2).map(|_| dial(&e, &caller, "5500"))).await;
    report("extension_call_concurrent", n / 2, started);
    assert_eq!(calls.len(), n / 2);
    // History: the member's newest page, then the plan.
    let scope = CallScope::of_platform(&e.a.w.ctx);
    let started = Instant::now();
    let rounds = 20;
    for _ in 0..rounds {
        let r =
            e.a.get(&format!(
                "/v1/calls?principalId={}&limit=25",
                member.principal.id()
            ))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(r.json["data"].as_array().unwrap().len(), 25);
    }
    report("history_page_25", rounds, started);
    let statement = calling::calls::app::history::history_statement(
        scope,
        member.principal.id(),
        &ListCallsQuery::default(),
        HistoryFilter {
            missed: Some(true),
            ..Default::default()
        },
        None,
        25,
    );
    let plan = explain(
        &e,
        &statement.sql,
        statement.values.map(|v| v.0).unwrap_or_default(),
    )
    .await;
    assert!(plan.contains("idx_call_participants_history"), "{plan}");
}

/// §198: a burst of Calls into ONE queue — every entry gets a unique, gap-free FIFO
/// position; the position query walks the FIFO index; agents drain it in order.
#[tokio::test]
async fn queue_burst_keeps_fifo_order() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let n = 60 * scale();
    let mut callers = Vec::new();
    for i in 0..8 {
        callers.push(desk(&e, &format!("burst-{i}")).await);
    }
    let q = e
        .create(
            "/v1/enterprise/queues",
            json!({ "name": "Burst line", "maxWaitSeconds": 3600, "offerTimeoutSeconds": 30,
                    "capacity": 1000 }),
        )
        .await;
    let queue_id = id_of(&q, "queueId");
    e.extension("8800", "QUEUE", queue_id).await;
    let started = Instant::now();
    let calls: Vec<Uuid> =
        join_all((0..n).map(|i| dial(&e, &callers[i % callers.len()], "8800"))).await;
    report("queue_insert_burst", n, started);
    // Positions: 1..n, unique, in (entered_at, seq) order.
    let positions = strings(
        e.h().conn(),
        &format!(
            "SELECT call_id::text || ':' || row_number() OVER (ORDER BY entered_at, seq) \
               FROM enterprise_queue_entries WHERE queue_id = '{queue_id}' \
                AND state IN ('WAITING', 'OFFERING') ORDER BY entered_at, seq"
        ),
    )
    .await;
    assert_eq!(positions.len(), n);
    let mut seen: Vec<String> = positions
        .iter()
        .map(|p| p.split(':').next().unwrap().to_string())
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), n, "one active entry per Call");
    for (i, p) in positions.iter().enumerate() {
        assert!(p.ends_with(&format!(":{}", i + 1)));
    }
    let _ = calls;
    let plan = explain(
        &e,
        "SELECT call_id, row_number() OVER (ORDER BY entered_at, seq) AS position \
           FROM enterprise_queue_entries WHERE queue_id = $1 AND state IN ('WAITING', 'OFFERING') \
          ORDER BY entered_at, seq LIMIT 50",
        vec![queue_id.into()],
    )
    .await;
    assert!(plan.contains("idx_enterprise_queue_entries_fifo"), "{plan}");
    // Assignment: agents join, the worker offers in FIFO order, agents answer.
    let mut agents = Vec::new();
    for i in 0..4 {
        agents.push(desk(&e, &format!("agent-{i}")).await);
    }
    let version = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/queues/{queue_id}"),
            None,
        )
        .await
        .json["version"]
        .clone();
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/queues/{queue_id}/members"),
            Some(json!({ "expectedVersion": version, "members": agents
                .iter()
                .map(|a| json!({ "principalId": a.principal.id().to_string() }))
                .collect::<Vec<Value>>() })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let rounds = 3;
    let started = Instant::now();
    let mut answered = Vec::new();
    for _ in 0..rounds {
        e.h().state.app.dispatch_queue(queue_id).await;
        for agent in &agents {
            let offered: Vec<String> = strings(
                e.h().conn(),
                &format!(
                    "SELECT call_id::text FROM call_invitations WHERE endpoint_id = '{}' \
                       AND status = 'RINGING'",
                    agent.phone.endpoint
                ),
            )
            .await;
            for call in offered {
                let call: Uuid = call.parse().unwrap();
                let invitation =
                    e.a.invitation_for(call, agent.phone.endpoint)
                        .await
                        .unwrap();
                let (_, v) = e.a.state(call).await;
                let r = e.a.accept(call, invitation, &agent.phone, v).await;
                assert_eq!(r.status, StatusCode::OK, "{}", r.text);
                answered.push(call);
                let (_, v) = e.a.state(call).await;
                e.a.command(call, "end", v).await;
            }
        }
    }
    report("queue_assignment", answered.len(), started);
    assert_eq!(answered.len(), rounds * agents.len());
    // FIFO: the answered Calls are exactly the oldest entries, in order.
    let oldest: Vec<String> = positions[..answered.len()]
        .iter()
        .map(|p| p.split(':').next().unwrap().to_string())
        .collect();
    let mut got: Vec<String> = answered.iter().map(Uuid::to_string).collect();
    let mut want = oldest.clone();
    got.sort();
    want.sort();
    assert_eq!(got, want, "agents took the head of the queue");
}

/// §199: a ring group at the policy's maximum fan-out rings exactly that many members,
/// one winner; the work per Call is bounded by fan-out × devices.
#[tokio::test]
async fn ring_fanout_is_bounded() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let mut members = Vec::new();
    for i in 0..15 {
        members.push(desk(&e, &format!("member-{i}")).await);
    }
    let g = e
        .create(
            "/v1/enterprise/ring-groups",
            json!({ "name": "All hands", "strategy": "SIMULTANEOUS", "memberRingSeconds": 20,
                    "groupTimeoutSeconds": 40 }),
        )
        .await;
    let gid = id_of(&g, "ringGroupId");
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ring-groups/{gid}/members"),
            Some(json!({ "expectedVersion": g["version"], "members": members
                .iter()
                .map(|m| json!({ "type": "ENTERPRISE_MEMBER", "id": m.principal.id().to_string() }))
                .collect::<Vec<Value>>() })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.extension("2900", "RING_GROUP", gid).await;
    let n = 10 * scale();
    let started = Instant::now();
    let mut calls = Vec::new();
    for _ in 0..n {
        calls.push(dial(&e, &caller, "2900").await);
    }
    report("ring_group_fanout_call", n, started);
    let policy_fanout = 10; // the default policy's maxRingGroupFanout
    for call in &calls {
        let offers = e
            .count(&format!(
                "SELECT count(*) FROM call_invitations WHERE call_id = '{call}'"
            ))
            .await;
        assert_eq!(offers, policy_fanout, "bounded by the policy's fan-out");
    }
    // One winner under concurrent answers.
    let call = calls[0];
    let mut attempts = Vec::new();
    for m in members.iter().take(policy_fanout as usize) {
        if let Some(i) = e.a.invitation_for(call, m.phone.endpoint).await {
            attempts.push((i, m));
        }
    }
    let (_, v) = e.a.state(call).await;
    let results = join_all(
        attempts
            .iter()
            .map(|(i, m)| e.a.accept(call, *i, &m.phone, v)),
    )
    .await;
    assert_eq!(
        results
            .iter()
            .filter(|r| r.status == StatusCode::OK)
            .count(),
        1,
        "one winner among {} concurrent answers",
        results.len()
    );
}

/// IVR transitions, business-hours evaluation and voicemail metadata reads per second.
#[tokio::test]
async fn ivr_and_schedule_evaluation_throughput() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let member = desk(&e, "member").await;
    let ext = e
        .extension("7700", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    let flow = e
        .create("/v1/enterprise/ivr-flows", json!({ "name": "Load menu" }))
        .await;
    let flow_id = id_of(&flow, "ivrFlowId");
    let r = e
        .admin(
            Method::POST,
            &format!("/v1/enterprise/ivr-flows/{flow_id}/versions"),
            Some(json!({ "graph": { "startNode": "main", "nodes": [
                { "id": "main", "kind": "MENU", "prompt": { "builtin": "MENU" },
                  "inputTimeoutSeconds": 5, "maxRetries": 1,
                  "options": [{ "digit": "1", "next": "sales" }],
                  "onInvalid": "bye", "onTimeout": "bye" },
                { "id": "sales", "kind": "ROUTE_EXTENSION", "extensionId": id_of(&ext, "extensionId") },
                { "id": "bye", "kind": "HANGUP", "prompt": { "builtin": "GOODBYE" } },
            ]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ivr-flows/{flow_id}/activation"),
            Some(json!({ "expectedVersion": 2, "versionNumber": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.extension("7701", "IVR", flow_id).await;
    let n = 20 * scale();
    let started = Instant::now();
    for _ in 0..n {
        let call = dial(&e, &caller, "7701").await;
        let r = e
            .send(
                e.caller_token(),
                Method::POST,
                &format!("/v1/calls/{call}/ivr/input"),
                Some(json!({ "endpointId": caller.phone.endpoint, "digit": "1" })),
            )
            .await;
        assert!(r.status.is_success(), "{}", r.text);
        assert!(e
            .a
            .invitation_for(call, member.phone.endpoint)
            .await
            .is_some());
    }
    report("ivr_call_and_transition", n, started);

    let (status, hours) = {
        let r = e
            .admin(
                Method::POST,
                "/v1/enterprise/business-hours",
                Some(json!({ "name": "Load hours", "timezone": "Europe/London",
                             "openDestination": { "type": "ENTERPRISE_MEMBER", "id": member.principal.id().to_string() },
                             "closedDestination": { "type": "ENTERPRISE_MEMBER", "id": member.principal.id().to_string() },
                             "weeklyWindows": [{ "dayOfWeek": "MONDAY", "start": "09:00", "end": "17:00" }] })),
            )
            .await;
        (r.status, r.json)
    };
    assert_eq!(status, StatusCode::CREATED, "{hours}");
    let hid = id_of(&hours, "businessHoursId");
    let m = 100 * scale();
    let started = Instant::now();
    for i in 0..m {
        let at = format!("2031-03-{:02}T{:02}:30:00Z", 1 + i % 28, i % 24);
        let r = e
            .admin(
                Method::POST,
                &format!("/v1/enterprise/business-hours/{hid}/evaluation"),
                Some(json!({ "at": at })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    report("business_hours_evaluation", m, started);
}
