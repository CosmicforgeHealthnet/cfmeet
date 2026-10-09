//! Part F §55 ring groups: SIMULTANEOUS and ORDERED strategies on ONE logical Call,
//! group-level single winner (application claim + database invariant), answered-
//! elsewhere for the other members, bounded fan-out, server-time stepping, group timeout,
//! fallback and fallback-loop protection.

mod common;

use axum::http::{Method, StatusCode};
use calling::{calls::CallState, domain::EnvironmentKind, platform::scopes::names};
use common::{
    app::Device,
    enterprise::{credential, default_rules, id_of, EnterpriseWorld},
};
use serde_json::{json, Value};
use uuid::Uuid;

async fn group(
    e: &EnterpriseWorld,
    strategy: &str,
    ring: i64,
    timeout: i64,
    fallback: Option<Value>,
) -> Value {
    // A unique name without digit runs (names never look like telephone numbers).
    let tag: String = Uuid::new_v4()
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
        .collect();
    let mut body = json!({ "name": format!("Group {tag}"), "strategy": strategy,
                           "memberRingSeconds": ring, "groupTimeoutSeconds": timeout });
    if let Some(f) = fallback {
        body["fallback"] = f;
    }
    e.create("/v1/enterprise/ring-groups", body).await
}

async fn set_members(e: &EnterpriseWorld, group: &Value, members: Vec<Value>) -> Value {
    let id = id_of(group, "ringGroupId");
    let version = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/ring-groups/{id}"),
            None,
        )
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ring-groups/{id}/members"),
            Some(json!({ "expectedVersion": version, "members": members })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json
}

fn member(kind: &str, id: impl std::fmt::Display) -> Value {
    json!({ "type": kind, "id": id.to_string() })
}

struct Team {
    caller: calling::platform::principals::ResolvedPrincipal,
    caller_phone: Device,
    members: Vec<(calling::platform::principals::ResolvedPrincipal, Device)>,
}

/// A caller and `n` enterprise members, each with one device.
async fn team(e: &EnterpriseWorld, n: usize) -> Team {
    let caller = e.member("caller").await;
    let caller_phone = e.a.device(&caller, "caller-desk").await;
    let mut members = Vec::new();
    for i in 0..n {
        let m = e.member(&format!("member-{i}")).await;
        let d = e.a.device(&m, &format!("member-{i}-desk")).await;
        members.push((m, d));
    }
    Team {
        caller,
        caller_phone,
        members,
    }
}

async fn dial(e: &EnterpriseWorld, t: &Team, number: &str) -> (Uuid, Value) {
    let r = e
        .call_extension(
            "VOICE",
            &t.caller,
            &t.caller_phone,
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

fn live_endpoints(view: &Value) -> Vec<String> {
    view["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["status"] == "RINGING")
        .map(|i| i["endpointId"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn ring_group_configuration() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let alice = e.member("alice").await;
    let bob = e.a.principal("bob").await;
    let carol = e.member("carol").await;
    let carol_ext = e.extension("1003", "ENTERPRISE_MEMBER", carol.id()).await;
    let g = group(&e, "SIMULTANEOUS", 10, 30, None).await;
    common::enterprise::conforms("RingGroup", &g);
    let id = id_of(&g, "ringGroupId");
    let g = set_members(
        &e,
        &g,
        vec![
            member("ENTERPRISE_MEMBER", alice.id()),
            member("PRINCIPAL", bob.id()),
            member("EXTENSION", id_of(&carol_ext, "extensionId")),
        ],
    )
    .await;
    assert_eq!(g["members"].as_array().unwrap().len(), 3);
    assert_eq!(g["members"][2]["position"], 3);
    assert_eq!(g["version"], 2);

    // An extension routed to the group, and the no-nesting rule.
    let to_group = e.extension("2000", "RING_GROUP", id).await;
    for (members, status, what) in [
        (
            vec![member("EXTENSION", id_of(&to_group, "extensionId"))],
            StatusCode::BAD_REQUEST,
            "nested group",
        ),
        (
            vec![member("RING_GROUP", id)],
            StatusCode::BAD_REQUEST,
            "group as member",
        ),
        (
            vec![member("PRINCIPAL", bob.id()), member("PRINCIPAL", bob.id())],
            StatusCode::BAD_REQUEST,
            "duplicate",
        ),
        (
            vec![member("PRINCIPAL", Uuid::new_v4())],
            StatusCode::NOT_FOUND,
            "unknown",
        ),
        (
            vec![member("ENTERPRISE_MEMBER", bob.id())],
            StatusCode::NOT_FOUND,
            "not a member kind",
        ),
        (
            (0..51)
                .map(|_| member("PRINCIPAL", Uuid::new_v4()))
                .collect(),
            StatusCode::BAD_REQUEST,
            "too many",
        ),
    ] {
        let r = e
            .admin(
                Method::PUT,
                &format!("/v1/enterprise/ring-groups/{id}/members"),
                Some(json!({ "expectedVersion": 2, "members": members })),
            )
            .await;
        assert_eq!(r.status, status, "{what}: {}", r.text);
    }
    // Timing and fallback validation.
    for (body, status, what) in [
        (
            json!({ "name": "x", "strategy": "ORDERED", "memberRingSeconds": 3, "groupTimeoutSeconds": 30 }),
            StatusCode::BAD_REQUEST,
            "ring too short",
        ),
        (
            json!({ "name": "x", "strategy": "ORDERED", "memberRingSeconds": 30, "groupTimeoutSeconds": 20 }),
            StatusCode::BAD_REQUEST,
            "timeout below ring",
        ),
        (
            json!({ "name": "x", "strategy": "ORDERED", "memberRingSeconds": 10, "groupTimeoutSeconds": 30,
                 "fallback": member("PUBLIC_CONTACT", Uuid::new_v4()) }),
            StatusCode::BAD_REQUEST,
            "public fallback",
        ),
        (
            json!({ "name": "x", "strategy": "ORDERED", "memberRingSeconds": 10, "groupTimeoutSeconds": 30,
                 "fallback": member("EXTENSION", Uuid::new_v4()) }),
            StatusCode::NOT_FOUND,
            "unknown fallback",
        ),
        (
            json!({ "name": "x", "strategy": "RANDOM", "memberRingSeconds": 10, "groupTimeoutSeconds": 30 }),
            StatusCode::BAD_REQUEST,
            "strategy",
        ),
    ] {
        let r = e
            .admin(Method::POST, "/v1/enterprise/ring-groups", Some(body))
            .await;
        assert_eq!(r.status, status, "{what}: {}", r.text);
    }
    // Versioned change; IDOR from another tenant.
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/ring-groups/{id}"),
            Some(json!({ "expectedVersion": 2, "strategy": "ORDERED",
                         "fallback": member("EXTENSION", id_of(&carol_ext, "extensionId")) })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["strategy"], "ORDERED");
    let (_, other_admin) = e.other_tenant("globex").await;
    for (method, path, body) in [
        (
            Method::GET,
            format!("/v1/enterprise/ring-groups/{id}"),
            None,
        ),
        (
            Method::PATCH,
            format!("/v1/enterprise/ring-groups/{id}"),
            Some(json!({ "expectedVersion": 3, "name": "x" })),
        ),
        (
            Method::PUT,
            format!("/v1/enterprise/ring-groups/{id}/members"),
            Some(json!({ "expectedVersion": 3, "members": [] })),
        ),
        (
            Method::DELETE,
            format!("/v1/enterprise/ring-groups/{id}?expectedVersion=3"),
            None,
        ),
    ] {
        let r = e
            .send(
                other_admin.secret.expose_once(),
                method.clone(),
                &path,
                body,
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "{method} {path}: {}",
            r.text
        );
    }
    // Another tenant's extension cannot point at our group.
    let r = e
        .send(
            other_admin.secret.expose_once(),
            Method::POST,
            "/v1/enterprise/extensions",
            Some(json!({ "extensionNumber": "2000", "displayLabel": "x", "destination": member("RING_GROUP", id) })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = e
        .admin(Method::GET, "/v1/enterprise/ring-groups", None)
        .await;
    common::enterprise::conforms("RingGroupList", &r.json);
}

#[tokio::test]
async fn simultaneous_group_rings_everyone_and_one_member_wins() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let t = team(&e, 3).await;
    let g = group(&e, "SIMULTANEOUS", 10, 30, None).await;
    set_members(
        &e,
        &g,
        t.members
            .iter()
            .map(|(m, _)| member("ENTERPRISE_MEMBER", m.id()))
            .collect(),
    )
    .await;
    e.extension("2000", "RING_GROUP", id_of(&g, "ringGroupId"))
        .await;
    let (call, created) = dial(&e, &t, "2000").await;
    assert_eq!(created["call"]["state"], "RINGING");
    assert_eq!(created["enterprise"]["phase"], "RING_GROUP");
    assert_eq!(created["liveInvitationCount"], 3);
    // One answer group across the three members.
    assert_eq!(
        e.count(&format!(
            "SELECT count(DISTINCT answer_group_id) FROM call_invitations WHERE call_id = '{call}' \
             AND answer_group_id IS NOT NULL"
        ))
        .await,
        1
    );
    // Member 2 answers.
    let (m2, d2) = &t.members[1];
    let invitation = e.a.invitation_for(call, d2.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, invitation, d2, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Answered);
    let view = e.a.call_view(call).await;
    let elsewhere: Vec<&str> = view["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["status"] == "ANSWERED_ELSEWHERE")
        .map(|i| i["endpointId"].as_str().unwrap())
        .collect();
    assert_eq!(elsewhere.len(), 2);
    // The losing members left the Call; exactly one group claim.
    let ended: Vec<&Value> = view["participants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| !p["endedAt"].is_null())
        .collect();
    assert_eq!(ended.len(), 2);
    assert_eq!(
        e.count("SELECT count(*) FROM call_answer_group_claims")
            .await,
        1
    );
    // A loser's late answer is refused.
    let (_, d1) = &t.members[0];
    let late = view["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["endpointId"] == d1.endpoint.to_string())
        .unwrap()["invitationId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, late, d1, v).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "ANSWERED_ELSEWHERE");
    // CONNECTED with the winner; the context names who answered.
    let s_caller = e.a.session_on(call, t.caller_phone.endpoint).await.unwrap();
    let s_winner = e.a.session_on(call, d2.endpoint).await.unwrap();
    e.a.connect(call, s_caller).await;
    e.a.connect(call, s_winner).await;
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call}' \
             AND answered_principal_id = '{}'",
            m2.id()
        ))
        .await,
        1
    );
    assert_eq!(
        e.count("SELECT count(*) FROM calls").await,
        1,
        "one logical Call"
    );
}

/// §46 / §189: two members answer at the same instant — exactly one wins, every time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_member_answers_have_exactly_one_winner() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let t = team(&e, 2).await;
    let g = group(&e, "SIMULTANEOUS", 10, 30, None).await;
    set_members(
        &e,
        &g,
        t.members
            .iter()
            .map(|(m, _)| member("ENTERPRISE_MEMBER", m.id()))
            .collect(),
    )
    .await;
    e.extension("2000", "RING_GROUP", id_of(&g, "ringGroupId"))
        .await;
    for round in 0..5 {
        let (call, _) = dial(&e, &t, "2000").await;
        let (_, d1) = &t.members[0];
        let (_, d2) = &t.members[1];
        let i1 = e.a.invitation_for(call, d1.endpoint).await.unwrap();
        let i2 = e.a.invitation_for(call, d2.endpoint).await.unwrap();
        let (_, v) = e.a.state(call).await;
        let (r1, r2) = tokio::join!(e.a.accept(call, i1, d1, v), e.a.accept(call, i2, d2, v));
        let ok = [&r1, &r2]
            .iter()
            .filter(|r| r.status == StatusCode::OK)
            .count();
        assert_eq!(ok, 1, "round {round}: {} / {}", r1.text, r2.text);
        let loser = if r1.status == StatusCode::OK {
            &r2
        } else {
            &r1
        };
        assert_eq!(loser.status, StatusCode::CONFLICT, "{}", loser.text);
        assert_eq!(loser.json["error"]["code"], "ANSWERED_ELSEWHERE");
        assert_eq!(
            e.count(&format!(
                "SELECT count(*) FROM call_answer_group_claims WHERE call_id = '{call}'"
            ))
            .await,
            1
        );
        assert_eq!(
            e.count(&format!(
                "SELECT count(*) FROM call_invitations WHERE call_id = '{call}' AND status = 'ACCEPTED'"
            ))
            .await,
            1
        );
        // No double media ownership: exactly one recipient session may get media.
        assert_eq!(
            e.count(&format!(
                "SELECT count(*) FROM call_sessions s JOIN call_participants p ON p.id = s.participant_id \
                 WHERE s.call_id = '{call}' AND p.role = 'RECIPIENT' AND s.media_access = 'AUTHORIZED'"
            ))
            .await,
            1
        );
        let (_, v) = e.a.state(call).await;
        let r = e.a.command(call, "end", v).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
}

#[tokio::test]
async fn ordered_group_steps_member_by_member() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let t = team(&e, 3).await;
    let unreachable = e.member("no-device").await;
    let g = group(&e, "ORDERED", 10, 60, None).await;
    let mut members = vec![member("ENTERPRISE_MEMBER", unreachable.id())];
    members.extend(
        t.members
            .iter()
            .map(|(m, _)| member("ENTERPRISE_MEMBER", m.id())),
    );
    set_members(&e, &g, members).await;
    e.extension("2000", "RING_GROUP", id_of(&g, "ringGroupId"))
        .await;
    let (call, created) = dial(&e, &t, "2000").await;
    // The unreachable first member is skipped: member 0 rings, alone.
    assert_eq!(created["liveInvitationCount"], 1, "{created}");
    let (_, d0) = &t.members[0];
    let (m1, d1) = &t.members[1];
    assert_eq!(
        live_endpoints(&e.a.call_view(call).await),
        [d0.endpoint.to_string()]
    );
    // Member 0 does not answer in time: server time moves the Call to member 1.
    e.h().clock.advance(chrono::Duration::seconds(11));
    e.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    assert_eq!(
        live_endpoints(&e.a.call_view(call).await),
        [d1.endpoint.to_string()]
    );
    // Member 1 answers: one logical Call, member 2 never rings.
    let invitation = e.a.invitation_for(call, d1.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, invitation, d1, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (_, d2) = &t.members[2];
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}' AND endpoint_id = '{}'",
            d2.endpoint
        ))
        .await,
        0,
        "no member rings after a valid winner"
    );
    e.h().clock.advance(chrono::Duration::seconds(30));
    e.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(e.a.state(call).await.0, CallState::Answered);
    assert_eq!(e.count("SELECT count(*) FROM calls").await, 1);
    let steps = common::enterprise::strings(
        e.h().conn(),
        &format!("SELECT decision FROM enterprise_routing_steps WHERE call_id = '{call}' ORDER BY step_number"),
    )
    .await;
    assert_eq!(
        steps,
        [
            "ENTER",
            "RING_GROUP_MEMBER",
            "RING_GROUP_MEMBER",
            "ANSWERED"
        ]
    );
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call}' \
             AND answered_principal_id = '{}'",
            m1.id()
        ))
        .await,
        1
    );
    // A decline moves on immediately (no wait for the ring time).
    let (call, _) = dial(&e, &t, "2000").await;
    let invitation = e.a.invitation_for(call, d0.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.decline(call, invitation, d0, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        live_endpoints(&e.a.call_view(call).await),
        [d1.endpoint.to_string()]
    );
}

#[tokio::test]
async fn group_timeout_falls_back_on_the_same_call_or_ends() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let t = team(&e, 3).await;
    let backup = e.member("backup").await;
    let backup_phone = e.a.device(&backup, "backup-desk").await;
    let backup_ext = e.extension("3000", "ENTERPRISE_MEMBER", backup.id()).await;
    let g = group(
        &e,
        "ORDERED",
        10,
        20,
        Some(member("EXTENSION", id_of(&backup_ext, "extensionId"))),
    )
    .await;
    set_members(
        &e,
        &g,
        t.members
            .iter()
            .map(|(m, _)| member("ENTERPRISE_MEMBER", m.id()))
            .collect(),
    )
    .await;
    e.extension("2000", "RING_GROUP", id_of(&g, "ringGroupId"))
        .await;
    let (call, _) = dial(&e, &t, "2000").await;
    e.h().clock.advance(chrono::Duration::seconds(11));
    e.h().state.app.expire_due(100).await.unwrap();
    // Member 1 rings for the remaining 9 s only (the group deadline bounds it).
    e.h().clock.advance(chrono::Duration::seconds(10));
    e.h().state.app.expire_due(100).await.unwrap();
    // Deadline passed: member 2 never rings; the fallback extension does, same Call.
    let (_, d2) = &t.members[2];
    assert_eq!(
        live_endpoints(&e.a.call_view(call).await),
        [backup_phone.endpoint.to_string()]
    );
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}' AND endpoint_id = '{}'",
            d2.endpoint
        ))
        .await,
        0
    );
    let invitation =
        e.a.invitation_for(call, backup_phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, invitation, &backup_phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.count("SELECT count(*) FROM calls").await, 1);

    // Without a fallback: everyone ignores a simultaneous group -> NO_ANSWER.
    let g2 = group(&e, "SIMULTANEOUS", 10, 15, None).await;
    set_members(
        &e,
        &g2,
        t.members
            .iter()
            .map(|(m, _)| member("ENTERPRISE_MEMBER", m.id()))
            .collect(),
    )
    .await;
    e.extension("2001", "RING_GROUP", id_of(&g2, "ringGroupId"))
        .await;
    let (call, _) = dial(&e, &t, "2001").await;
    e.h().clock.advance(chrono::Duration::seconds(16));
    e.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(e.a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call}' AND outcome = 'NO_ANSWER'"
        ))
        .await,
        1
    );
    // ... and everyone declines -> DECLINED.
    let (call, _) = dial(&e, &t, "2001").await;
    for (_, d) in &t.members {
        let invitation = e.a.invitation_for(call, d.endpoint).await.unwrap();
        let (_, v) = e.a.state(call).await;
        let r = e.a.decline(call, invitation, d, v).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    assert_eq!(e.a.state(call).await.0, CallState::Declined);
}

#[tokio::test]
async fn simultaneous_fanout_is_bounded_by_policy() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let t = team(&e, 5).await;
    let mut rules = default_rules();
    rules["maxRingGroupFanout"] = json!(2);
    e.create(
        "/v1/enterprise/policies",
        json!({ "name": "Small fan-out", "isDefault": true, "rules": rules }),
    )
    .await;
    let g = group(&e, "SIMULTANEOUS", 10, 30, None).await;
    set_members(
        &e,
        &g,
        t.members
            .iter()
            .map(|(m, _)| member("ENTERPRISE_MEMBER", m.id()))
            .collect(),
    )
    .await;
    e.extension("2000", "RING_GROUP", id_of(&g, "ringGroupId"))
        .await;
    let (call, created) = dial(&e, &t, "2000").await;
    assert_eq!(created["liveInvitationCount"], 2, "{created}");
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_participants WHERE call_id = '{call}' AND role = 'RECIPIENT'"
        ))
        .await,
        2,
        "no unbounded participant / session explosion"
    );
}

#[tokio::test]
async fn a_fallback_loop_is_blocked_and_fails_safe() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let t = team(&e, 1).await;
    let g = group(&e, "SIMULTANEOUS", 10, 10, None).await;
    set_members(
        &e,
        &g,
        vec![member("ENTERPRISE_MEMBER", t.members[0].0.id())],
    )
    .await;
    let ext = e
        .extension("2000", "RING_GROUP", id_of(&g, "ringGroupId"))
        .await;
    // The group falls back to the very extension that routes to it: 2000 -> G -> 2000.
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/ring-groups/{}", id_of(&g, "ringGroupId")),
            Some(json!({ "expectedVersion": 2, "fallback": member("EXTENSION", id_of(&ext, "extensionId")) })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (call, _) = dial(&e, &t, "2000").await;
    e.h().clock.advance(chrono::Duration::seconds(11));
    e.h().state.app.expire_due(100).await.unwrap();
    let (state, _) = e.a.state(call).await;
    assert_eq!(state, CallState::Failed, "the loop ends the Call safely");
    let reasons = common::enterprise::strings(
        e.h().conn(),
        &format!("SELECT reason FROM enterprise_routing_steps WHERE call_id = '{call}' ORDER BY step_number"),
    )
    .await;
    assert!(reasons.contains(&"LOOP_BLOCKED".to_string()), "{reasons:?}");
    // Exactly one ring round happened: the group was never re-entered.
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}'"
        ))
        .await,
        1
    );
    let _ = (EnvironmentKind::Production, names::CALLS_READ, credential);
}
