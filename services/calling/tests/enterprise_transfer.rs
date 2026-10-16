//! Part F §59: hold / resume / transfer of enterprise Calls through the canonical Part C
//! actions — enterprise transfer targets (extension, member, ring group, queue,
//! schedule; public contact refused), policy, the SAME Call preserved, failed transfers
//! recovering to CONNECTED / HELD without orphans, and races without double ownership.

mod common;

use axum::http::{Method, StatusCode};
use calling::calls::CallState;
use common::{
    app::Device,
    enterprise::{default_rules, id_of, strings, EnterpriseWorld},
    TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

type Principal = calling::platform::principals::ResolvedPrincipal;

struct Desk {
    principal: Principal,
    phone: Device,
}

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

async fn desk(e: &EnterpriseWorld, name: &str) -> Desk {
    let principal = e.member(&format!("{name}-{}", tag())).await;
    let phone = e.a.device(&principal, &format!("{name}-desk")).await;
    Desk { principal, phone }
}

/// A CONNECTED enterprise Call: `caller` dials `number`, `callee` answers, both media up.
async fn connected(e: &EnterpriseWorld, caller: &Desk, number: &str, callee: &Desk) -> Uuid {
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

async fn transfer(e: &EnterpriseWorld, call: Uuid, from: &Desk, target: Value) -> TestResponse {
    let participant =
        e.a.participant_of(call, from.principal.id().as_uuid())
            .await;
    let (_, v) = e.a.state(call).await;
    let mut body = json!({ "transferringParticipantId": participant, "expectedVersion": v });
    for (k, val) in target.as_object().unwrap() {
        body[k] = val.clone();
    }
    e.h()
        .request_with(
            Method::POST,
            &format!("/v1/calls/{call}/transfer"),
            Some(e.caller_token()),
            Some(body),
            &[("idempotency-key", &Uuid::new_v4().to_string())],
        )
        .await
}

async fn steps(e: &EnterpriseWorld, call: Uuid) -> Vec<String> {
    strings(
        e.h().conn(),
        &format!(
            "SELECT decision || ':' || reason FROM enterprise_routing_steps WHERE call_id = '{call}' \
              ORDER BY step_number"
        ),
    )
    .await
}

async fn version_command(e: &EnterpriseWorld, call: Uuid, action: &str) -> TestResponse {
    let (_, v) = e.a.state(call).await;
    e.h()
        .request_with(
            Method::POST,
            &format!("/v1/calls/{call}/{action}"),
            Some(e.caller_token()),
            Some(json!({ "expectedVersion": v })),
            &[("idempotency-key", &Uuid::new_v4().to_string())],
        )
        .await
}

/// Completes the ringing transfer: `target` answers and joins.
async fn target_joins(e: &EnterpriseWorld, call: Uuid, target: &Desk) {
    let inv =
        e.a.invitation_for(call, target.phone.endpoint)
            .await
            .expect("target rings");
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, inv, &target.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let s = e.a.session_on(call, target.phone.endpoint).await.unwrap();
    e.a.connect(call, s).await;
}

async fn live_participants(e: &EnterpriseWorld, call: Uuid) -> i64 {
    e.count(&format!(
        "SELECT count(*) FROM call_participants WHERE call_id = '{call}' AND ended_at IS NULL"
    ))
    .await
}

#[tokio::test]
async fn hold_resume_and_transfer_to_an_extension_keep_one_call() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let a = desk(&e, "a").await;
    let c = desk(&e, "c").await;
    e.extension("7101", "ENTERPRISE_MEMBER", a.principal.id())
        .await;
    e.extension("7103", "ENTERPRISE_MEMBER", c.principal.id())
        .await;
    let call = connected(&e, &caller, "7101", &a).await;

    // Hold / resume: the canonical actions.
    let r = version_command(&e, call, "hold").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Held);
    let r = version_command(&e, call, "resume").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Connected);

    // A (who answered) hands the Call to extension 7103.
    let r = transfer(&e, call, &a, json!({ "targetExtension": "7103" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Transferring);
    // The stable path is kept until the outcome is known.
    assert_eq!(live_participants(&e, call).await, 3);
    target_joins(&e, call, &c).await;
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
    assert_eq!(live_participants(&e, call).await, 2);
    assert_eq!(
        e.count(&format!("SELECT count(*) FROM calls WHERE id = '{call}'"))
            .await,
        1
    );
    assert_eq!(
        strings(
            e.h().conn(),
            &format!("SELECT state FROM call_transfers WHERE call_id = '{call}'")
        )
        .await,
        ["COMPLETED"]
    );
    let s = steps(&e, call).await;
    assert!(
        s.contains(&"TRANSFER:TRANSFER_STARTED".to_string()),
        "{s:?}"
    );
    assert!(
        s.contains(&"TRANSFER:TRANSFER_COMPLETED".to_string()),
        "{s:?}"
    );
    // The same authorization evidence still governs the Call.
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_authorizations WHERE call_id = '{call}' AND decision = 'APPROVED'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn enterprise_transfer_targets_and_policy() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let a = desk(&e, "a").await;
    let g1 = e.member(&format!("g1-{}", tag())).await; // no device: skipped
    let g2 = desk(&e, "g2").await;
    let agent = desk(&e, "agent").await;
    e.extension("7201", "ENTERPRISE_MEMBER", a.principal.id())
        .await;

    // Ring group -> its first member with an eligible endpoint.
    let group = e
        .create(
            "/v1/enterprise/ring-groups",
            json!({ "name": format!("Group {}", tag()), "strategy": "ORDERED",
                    "memberRingSeconds": 10, "groupTimeoutSeconds": 30 }),
        )
        .await;
    let gid = id_of(&group, "ringGroupId");
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ring-groups/{gid}/members"),
            Some(json!({ "expectedVersion": 1, "members": [
                { "type": "ENTERPRISE_MEMBER", "id": g1.id() },
                { "type": "ENTERPRISE_MEMBER", "id": g2.principal.id() }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let call = connected(&e, &caller, "7201", &a).await;
    let r = transfer(
        &e,
        call,
        &a,
        json!({ "targetDestination": { "type": "RING_GROUP", "id": gid } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(e.a.invitation_for(call, g2.phone.endpoint).await.is_some());
    target_joins(&e, call, &g2).await;
    assert_eq!(e.a.state(call).await.0, CallState::Connected);

    // Queue -> its first available agent.
    let q = e
        .create(
            "/v1/enterprise/queues",
            json!({ "name": format!("Support {}", tag()), "maxWaitSeconds": 600,
                    "offerTimeoutSeconds": 20, "capacity": 10 }),
        )
        .await;
    let qid = id_of(&q, "queueId");
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/queues/{qid}/members"),
            Some(json!({ "expectedVersion": 1, "members": [{ "principalId": agent.principal.id() }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = transfer(
        &e,
        call,
        &g2,
        json!({ "targetDestination": { "type": "QUEUE", "id": qid } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    target_joins(&e, call, &agent).await;
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
    assert_eq!(
        e.count(&format!("SELECT count(*) FROM calls WHERE id = '{call}'"))
            .await,
        1
    );

    // Refusals change nothing.
    let before = e.a.state(call).await;
    for (target, status) in [
        (json!({ "targetExtension": "9999" }), StatusCode::NOT_FOUND),
        (
            json!({ "targetDestination": { "type": "QUEUE", "id": Uuid::new_v4() } }),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({ "targetDestination": { "type": "PUBLIC_CONTACT", "id": Uuid::new_v4() } }),
            StatusCode::FORBIDDEN,
        ),
        (
            json!({ "targetExtension": "7201", "targetPrincipalId": a.principal.id() }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "targetPhoneNumber": "+2348012345678" }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let r = transfer(&e, call, &agent, target.clone()).await;
        assert_eq!(r.status, status, "{target}: {}", r.text);
    }
    assert_eq!(e.a.state(call).await, before);
    // Enterprise targets need enterprise.calls.use (calls.* alone is not enough).
    let participant =
        e.a.participant_of(call, agent.principal.id().as_uuid())
            .await;
    let r =
        e.a.post_with(
            &format!("/v1/calls/{call}/transfer"),
            json!({ "transferringParticipantId": participant, "targetExtension": "7201",
                    "expectedVersion": before.1 }),
            &[("idempotency-key", "k-scope")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // The enterprise policy may forbid transfers of enterprise Calls (any target).
    let mut rules = default_rules();
    rules["transferAllowed"] = json!(false);
    e.create(
        "/v1/enterprise/policies",
        json!({ "name": "No transfers", "isDefault": true, "rules": rules }),
    )
    .await;
    let r = transfer(
        &e,
        call,
        &agent,
        json!({ "targetPrincipalId": a.principal.id() }),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(e.a.state(call).await, before);
}

#[tokio::test]
async fn failed_transfers_recover_without_orphans() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let a = desk(&e, "a").await;
    let c = desk(&e, "c").await;
    let d = desk(&e, "d").await;
    e.extension("7305", "ENTERPRISE_MEMBER", d.principal.id())
        .await;
    let nobody = e.member(&format!("nobody-{}", tag())).await;
    e.extension("7301", "ENTERPRISE_MEMBER", a.principal.id())
        .await;
    e.extension("7303", "ENTERPRISE_MEMBER", c.principal.id())
        .await;
    e.extension("7304", "ENTERPRISE_MEMBER", nobody.id()).await;
    let call = connected(&e, &caller, "7301", &a).await;

    // Unavailable destination: refused, the Call is untouched.
    let r = transfer(&e, call, &a, json!({ "targetExtension": "7304" })).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "TARGET_UNAVAILABLE");
    assert_eq!(e.a.state(call).await.0, CallState::Connected);

    // Declined: back to CONNECTED over the original path.
    let r = transfer(&e, call, &a, json!({ "targetExtension": "7303" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let inv = e.a.invitation_for(call, c.phone.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.decline(call, inv, &c.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
    assert_eq!(live_participants(&e, call).await, 2);

    // A principal joins a Call once (Part C): the declined target is not re-offered.
    let r = transfer(&e, call, &a, json!({ "targetExtension": "7303" })).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    // No answer from HELD: back to HELD.
    let r = version_command(&e, call, "hold").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = transfer(&e, call, &a, json!({ "targetExtension": "7305" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.h().clock.advance(chrono::Duration::seconds(46));
    e.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(e.a.state(call).await.0, CallState::Held);
    // No orphan participant, ring offer or transfer.
    assert_eq!(live_participants(&e, call).await, 2);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}' AND status IN ('PENDING', 'RINGING')"
        ))
        .await,
        0
    );
    assert_eq!(
        strings(
            e.h().conn(),
            &format!(
                "SELECT state FROM call_transfers WHERE call_id = '{call}' ORDER BY created_at"
            )
        )
        .await,
        ["FAILED", "FAILED"]
    );
    let s = steps(&e, call).await;
    assert_eq!(
        s.iter()
            .filter(|x| *x == "TRANSFER:TRANSFER_FAILED")
            .count(),
        2,
        "{s:?}"
    );
}

/// Races (§97): transfer vs end, transfer vs transfer, the target answering while the
/// Call ends, and the transferring party leaving first — never a second owner, never a
/// stuck TRANSFERRING Call.
#[tokio::test]
async fn transfer_races_leave_one_owner() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let a = desk(&e, "a").await;
    let c = desk(&e, "c").await;
    let d = desk(&e, "d").await;
    e.extension("7401", "ENTERPRISE_MEMBER", a.principal.id())
        .await;
    e.extension("7403", "ENTERPRISE_MEMBER", c.principal.id())
        .await;
    e.extension("7404", "ENTERPRISE_MEMBER", d.principal.id())
        .await;

    // Transfer vs another transfer: exactly one starts.
    let call = connected(&e, &caller, "7401", &a).await;
    let (r1, r2) = tokio::join!(
        transfer(&e, call, &a, json!({ "targetExtension": "7403" })),
        transfer(&e, call, &a, json!({ "targetExtension": "7404" })),
    );
    let ok = [&r1, &r2]
        .iter()
        .filter(|r| r.status == StatusCode::OK)
        .count();
    assert_eq!(ok, 1, "{} / {}", r1.text, r2.text);
    assert!([&r1, &r2]
        .iter()
        .all(|r| r.status == StatusCode::OK || r.status == StatusCode::CONFLICT));
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_transfers WHERE call_id = '{call}' AND state IN ('REQUESTED', 'RINGING_TARGET', 'ACCEPTED')"
        ))
        .await,
        1
    );

    // The target answers while the Call is ended: one consistent outcome.
    let target = if e.a.invitation_for(call, c.phone.endpoint).await.is_some() {
        &c
    } else {
        &d
    };
    let inv =
        e.a.invitation_for(call, target.phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(call).await;
    let (accept, end) = tokio::join!(
        e.a.accept(call, inv, &target.phone, v),
        version_command(&e, call, "end"),
    );
    assert!(
        accept.status == StatusCode::OK || end.status == StatusCode::OK,
        "{} / {}",
        accept.text,
        end.text
    );
    e.h().state.app.expire_due(100).await.unwrap();
    let state = e.a.state(call).await.0;
    assert!(
        matches!(
            state,
            CallState::Ending | CallState::Ended | CallState::Transferring | CallState::Connected
        ),
        "{state:?}"
    );
    if matches!(state, CallState::Ending | CallState::Ended) {
        assert_eq!(
            e.count(&format!(
                "SELECT count(*) FROM call_transfers WHERE call_id = '{call}' AND state IN ('REQUESTED', 'RINGING_TARGET', 'ACCEPTED')"
            ))
            .await,
            0
        );
    }

    // Transfer vs call end: the end wins or the transfer starts and is then cancelled.
    let call = connected(&e, &caller, "7401", &a).await;
    let (t, end) = tokio::join!(
        transfer(&e, call, &a, json!({ "targetExtension": "7403" })),
        version_command(&e, call, "end"),
    );
    assert!(
        t.status == StatusCode::OK || end.status == StatusCode::OK,
        "{} / {}",
        t.text,
        end.text
    );
    if end.status != StatusCode::OK {
        let r = version_command(&e, call, "end").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    assert!(matches!(
        e.a.state(call).await.0,
        CallState::Ending | CallState::Ended
    ));
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}' AND status IN ('PENDING', 'RINGING')"
        ))
        .await,
        0
    );

    // The transferring party's leg ends before the target answers (§97 "old leg ends
    // first"): the target still takes the Call next to the remaining party, the
    // transferring participant is retired, exactly one answering owner remains.
    let call = connected(&e, &caller, "7401", &a).await;
    let r = transfer(&e, call, &a, json!({ "targetExtension": "7403" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let a_session = e.a.session_on(call, a.phone.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e
        .h()
        .request_with(
            Method::POST,
            &format!("/v1/calls/{call}/sessions/{a_session}/remove"),
            Some(e.caller_token()),
            Some(json!({ "expectedVersion": v })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    target_joins(&e, call, &c).await;
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
    assert_eq!(live_participants(&e, call).await, 2);
    assert_eq!(
        strings(
            e.h().conn(),
            &format!("SELECT state FROM call_transfers WHERE call_id = '{call}'")
        )
        .await,
        ["COMPLETED"]
    );
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_participants WHERE call_id = '{call}' AND ended_at IS NULL \
               AND principal_id = '{}'",
            a.principal.id()
        ))
        .await,
        0
    );
}
