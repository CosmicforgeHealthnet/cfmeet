//! Block / opt-out rules (Part H §77.19) and repeated refusals (§77.21), end to end:
//! a rule is consulted before any protected side effect — no Call, no Privacy Relay
//! authorization, no spend reservation, no Telephony command — on every path that can
//! connect the two sides (app-to-app both ways, app-to-phone, callback relay, inbound,
//! ring groups, queues, forwarding); it never leaks across tenants or applications,
//! never ends an existing Call and never reveals a number; repeated refusals become one
//! deduplicated incident per window with bounded metrics.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{calls::CallState, domain::EnvironmentKind, platform::scopes::names};
use common::{
    app::{AppWorld, Device},
    enterprise::{default_rules, id_of, strings, EnterpriseWorld},
    privacy::FakePrivacyRelay,
    public::{number_like_runs, public_policy, spend_policy, PublicWorld},
    telephony::{FakePublicMedia, FakeTelephony},
    TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

type Principal = calling::platform::principals::ResolvedPrincipal;

fn code(r: &TestResponse) -> (StatusCode, String) {
    (
        r.status,
        r.json["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

async fn delete(w: &AppWorld, token: &str, path: &str) -> TestResponse {
    w.h().request(Method::DELETE, path, Some(token), None).await
}

async fn incidents(conn: &sea_orm::DatabaseConnection, category: &str) -> Vec<Value> {
    strings(
        conn,
        &format!(
            "SELECT json_build_object('status', status, 'occurrences', occurrences, \
               'resourceType', resource_type, 'resourceId', resource_id, \
               'evidence', restricted_evidence)::text \
             FROM calling_incidents WHERE category = '{category}' ORDER BY first_seen_at"
        ),
    )
    .await
    .iter()
    .map(|s| serde_json::from_str(s).unwrap())
    .collect()
}

#[tokio::test]
async fn a_principal_block_refuses_app_calls_both_ways_and_release_restores_them() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let (a, b) = (&w.w.initiator, &w.w.recipient);
    let da = w.device(a, "a-main").await;
    let db = w.device(b, "b-main").await;
    let r = w
        .post(
            "/v1/blocks",
            json!({ "principalId": a.id(), "blockedPrincipalId": b.id(), "reasonCode": "HARASSMENT" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let block = r.json["blockId"].as_str().unwrap().to_string();
    // The same pair again is the same rule (idempotent).
    let again = w
        .post(
            "/v1/blocks",
            json!({ "principalId": a.id(), "blockedPrincipalId": b.id(), "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.json["blockId"], block.as_str());

    let forward = w.create_call("VOICE", a, &da, &[b], "blk-ab").await;
    let backward = w.create_call("VOICE", b, &db, &[a], "blk-ba").await;
    for r in [&forward, &backward] {
        assert_eq!(
            code(r),
            (StatusCode::CONFLICT, "TARGET_UNAVAILABLE".into()),
            "{}",
            r.text
        );
        // Never discloses that (or which side) a block exists.
        assert!(!r.text.to_ascii_lowercase().contains("block"), "{}", r.text);
        assert!(number_like_runs(&r.text).is_empty());
    }
    assert_eq!(forward.normalized_error(), backward.normalized_error());
    assert_eq!(w.w.count("SELECT count(*) FROM calls").await, 0);
    assert_eq!(w.w.count("SELECT count(*) FROM call_invitations").await, 0);

    // Release: allowed again.
    let r = delete(&w, w.secret(), &format!("/v1/blocks/{block}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "RELEASED");
    let ok = w.create_call("VOICE", a, &da, &[b], "blk-ab-2").await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text);
}

#[tokio::test]
async fn a_blocked_contact_is_refused_before_privacy_relay_spend_and_telephony() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let initiator = x.w.w.initiator.id();
    // The initiator blocks the contact.
    let r = x
        .block(
            x.w.secret(),
            json!({ "principalId": initiator, "blockedContactReferenceId": x.contact,
                    "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let mine = r.json["blockId"].as_str().unwrap().to_string();
    let refused = x.call_to(x.contact, "contact-1").await;
    assert_eq!(
        code(&refused),
        (StatusCode::FORBIDDEN, "PUBLIC_ROUTE_DENIED".into()),
        "{}",
        refused.text
    );
    assert!(number_like_runs(&refused.text).is_empty());
    assert!(x.privacy.requests().is_empty(), "Privacy Relay was asked");
    assert!(x.privacy.grants().is_empty());
    assert!(x.telephony.originates().is_empty());
    assert_eq!(
        x.effects().await,
        (0, 0, 0),
        "a Call, command or reservation exists"
    );

    // The application blocks the same contact; releasing the principal's rule is not
    // enough, and a calls.write credential cannot release the application's rule.
    let admin = x.app_admin().await;
    let r = x
        .block(
            admin.secret.expose_once(),
            json!({ "blockedContactReferenceId": x.contact, "reasonCode": "UNWANTED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let app_rule = r.json["blockId"].as_str().unwrap().to_string();
    // Creating an application rule needs enterprise.config.write.
    let r = x
        .block(
            x.w.secret(),
            json!({ "blockedContactReferenceId": Uuid::new_v4(), "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = delete(&x.w, x.w.secret(), &format!("/v1/blocks/{mine}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = delete(&x.w, x.w.secret(), &format!("/v1/blocks/{app_rule}")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let refused = x.call_to(x.contact, "contact-2").await;
    assert_eq!(
        code(&refused),
        (StatusCode::FORBIDDEN, "PUBLIC_ROUTE_DENIED".into())
    );
    assert!(x.privacy.requests().is_empty());
    assert_eq!(x.effects().await, (0, 0, 0));

    // Released: the call goes through every later gate and is placed.
    let r = delete(
        &x.w,
        admin.secret.expose_once(),
        &format!("/v1/blocks/{app_rule}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let ok = x.call_to(x.contact, "contact-3").await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text);
    assert_eq!(x.privacy.requests().len(), 1);
    assert_eq!(x.telephony.originates().len(), 1);
}

#[tokio::test]
async fn block_rules_never_cross_tenants_or_applications() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let admin = x.app_admin().await;
    let r = x
        .block(
            admin.secret.expose_once(),
            json!({ "blockedContactReferenceId": x.contact, "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let rule = r.json["blockId"].as_str().unwrap().to_string();
    let other = x.other_tenant("globex").await;

    // Tenant B cannot see or release tenant A's rule (404, like a missing one), and A's
    // credential cannot name B's principal.
    let missing = delete(
        &x.w,
        other.admin.secret.expose_once(),
        &format!("/v1/blocks/{}", Uuid::new_v4()),
    )
    .await;
    for token in [
        other.admin.secret.expose_once(),
        other.token.secret.expose_once(),
    ] {
        let r = delete(&x.w, token, &format!("/v1/blocks/{rule}")).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
        assert_eq!(r.normalized_error(), missing.normalized_error());
    }
    let r = x
        .block(
            x.w.secret(),
            json!({ "principalId": x.w.w.initiator.id(), "blockedPrincipalId": other.initiator.id(),
                    "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // Another application of tenant A cannot release it either.
    let sibling =
        x.w.h()
            .application_in(&x.w.w.consumer.tenant, "sibling")
            .await;
    common::enterprise::allow_enterprise_scopes(x.w.h(), &sibling).await;
    let sibling_admin = common::enterprise::credential(
        x.w.h(),
        &sibling,
        EnvironmentKind::Production,
        &[names::ENTERPRISE_CONFIG_WRITE],
    )
    .await;
    let r = delete(
        &x.w,
        sibling_admin.secret.expose_once(),
        &format!("/v1/blocks/{rule}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM calling_block_rules WHERE id = '{rule}' AND status = 'ACTIVE'"
        ))
        .await,
        1
    );

    // Tenant A's rule on the contact id never blocks tenant B's call to the same id.
    let r = other.call_to(&x, x.contact, "other-tenant-call").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(x.telephony.originates().len(), 1);
    // ... while tenant A stays blocked.
    let refused = x.call_to(x.contact, "tenant-a").await;
    assert_eq!(
        code(&refused),
        (StatusCode::FORBIDDEN, "PUBLIC_ROUTE_DENIED".into())
    );
    assert_eq!(x.telephony.originates().len(), 1);
}

async fn ring_group(e: &EnterpriseWorld, members: &[&Principal]) -> Value {
    let g = e
        .create(
            "/v1/enterprise/ring-groups",
            json!({ "name": "Blocked members group", "strategy": "SIMULTANEOUS",
                    "memberRingSeconds": 20, "groupTimeoutSeconds": 40 }),
        )
        .await;
    let id = id_of(&g, "ringGroupId");
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ring-groups/{id}/members"),
            Some(
                json!({ "expectedVersion": g["version"], "members": members.iter()
                .map(|m| json!({ "type": "ENTERPRISE_MEMBER", "id": m.id().to_string() }))
                .collect::<Vec<_>>() }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    g
}

async fn queue(e: &EnterpriseWorld, agents: &[&Principal]) -> Value {
    let q = e
        .create(
            "/v1/enterprise/queues",
            json!({ "name": "Blocked agents queue", "maxWaitSeconds": 600,
                    "offerTimeoutSeconds": 20, "capacity": 10 }),
        )
        .await;
    let id = id_of(&q, "queueId");
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/queues/{id}/members"),
            Some(
                json!({ "expectedVersion": q["version"], "members": agents.iter()
                .map(|a| json!({ "principalId": a.id().to_string() }))
                .collect::<Vec<_>>() }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    q
}

async fn dial(e: &EnterpriseWorld, from: &Principal, phone: &Device, number: &str) -> Value {
    let r = e
        .call_extension("VOICE", from, phone, number, &Uuid::new_v4().to_string())
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json
}

#[tokio::test]
async fn ring_groups_skip_blocked_members_and_queues_never_offer_blocked_agents() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = e.member("blk-caller").await;
    let caller_phone = e.a.device(&caller, "caller-desk").await;
    let mut desks = Vec::new();
    for name in ["blk-m0", "blk-m1", "blk-a0", "blk-a1"] {
        let p = e.member(name).await;
        let d = e.a.device(&p, &format!("{name}-desk")).await;
        desks.push((p, d));
    }
    // m0 blocks the caller; the caller blocks agent a0 (either direction counts).
    for (blocker, blocked) in [(&desks[0].0, &caller), (&caller, &desks[2].0)] {
        let r =
            e.a.post(
                "/v1/blocks",
                json!({ "principalId": blocker.id(), "blockedPrincipalId": blocked.id(),
                        "reasonCode": "UNWANTED" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    let g = ring_group(&e, &[&desks[0].0, &desks[1].0]).await;
    e.extension("7100", "RING_GROUP", id_of(&g, "ringGroupId"))
        .await;
    let created = dial(&e, &caller, &caller_phone, "7100").await;
    let call: Uuid = created["call"]["callId"].as_str().unwrap().parse().unwrap();
    assert_eq!(created["liveInvitationCount"], 1, "{created}");
    assert!(e
        .a
        .invitation_for(call, desks[1].1.endpoint)
        .await
        .is_some());
    assert!(e
        .a
        .invitation_for(call, desks[0].1.endpoint)
        .await
        .is_none());
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_participants WHERE call_id = '{call}' AND principal_id = '{}'",
            desks[0].0.id()
        ))
        .await,
        0
    );

    // Queue: a0 is listed first, yet only a1 is ever offered the caller.
    let q = queue(&e, &[&desks[2].0, &desks[3].0]).await;
    e.extension("7200", "QUEUE", id_of(&q, "queueId")).await;
    let created = dial(&e, &caller, &caller_phone, "7200").await;
    let queued: Uuid = created["call"]["callId"].as_str().unwrap().parse().unwrap();
    e.h().state.app.expire_due(100).await.unwrap();
    let offers = |p: &Principal| {
        format!(
            "SELECT count(*) FROM enterprise_queue_offers o JOIN enterprise_queue_entries q \
               ON q.id = o.entry_id WHERE q.call_id = '{queued}' AND o.principal_id = '{}'",
            p.id()
        )
    };
    assert_eq!(
        e.count(&offers(&desks[2].0)).await,
        0,
        "a blocked agent was offered"
    );
    assert_eq!(e.count(&offers(&desks[3].0)).await, 1);
    assert!(e
        .a
        .invitation_for(queued, desks[2].1.endpoint)
        .await
        .is_none());
}

#[tokio::test]
async fn forwarding_to_a_blocked_public_target_is_refused() {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    let media = Arc::new(FakePublicMedia::default());
    let Some(a) =
        AppWorld::with_public_network(privacy.clone(), telephony.clone(), Some(media.clone()))
            .await
    else {
        return;
    };
    let e = EnterpriseWorld::around(a).await;
    let scope = e.a.w.scope();
    calling::routing::store::store_policy(
        e.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        scope.environment_id().as_uuid(),
        &public_policy(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    calling::telephony::spend::store_policy(
        e.h().conn(),
        calling::telephony::spend::SpendScope {
            tenant_id: scope.tenant_id().as_uuid(),
            application_id: scope.application_id().as_uuid(),
            environment_id: scope.environment_id().as_uuid(),
        },
        &spend_policy(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    e.h()
        .state
        .enterprise
        .operator_set_capabilities(
            scope,
            vec![
                calling::enterprise::policy::AdminCapability::ManageRouting,
                calling::enterprise::policy::AdminCapability::ManagePublicForwarding,
            ],
        )
        .await
        .unwrap();
    let mut rules = default_rules();
    rules["externalPublicCallingAllowed"] = json!(true);
    rules["publicForwardingAllowed"] = json!(true);
    rules["publicDestinationCountries"] = json!(["NG"]);
    rules["adminCapabilities"] = json!(["MANAGE_ROUTING", "MANAGE_PUBLIC_FORWARDING"]);
    e.create(
        "/v1/enterprise/policies",
        json!({ "name": "Public forwarding baseline", "isDefault": true, "rules": rules }),
    )
    .await;
    let reference = privacy.add_contact("NG");
    let contact = e
        .create(
            "/v1/enterprise/public-contacts",
            json!({ "label": "On call phone", "contactReference": reference,
                    "privacyRelaySessionId": Uuid::new_v4() }),
        )
        .await;
    let caller = e.member("fwd-caller").await;
    privacy.allow(caller.id().as_uuid());
    let caller_phone = e.a.device(&caller, "caller-desk").await;
    let target = e.member("fwd-target").await;
    e.extension("7300", "ENTERPRISE_MEMBER", target.id()).await;
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/forwarding-rules",
            Some(json!({ "principalId": target.id(), "condition": "ALWAYS",
                         "destination": { "type": "PUBLIC_CONTACT",
                                          "id": id_of(&contact, "publicContactId").to_string() } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let steps = |call: Uuid| {
        let conn = e.h().conn().clone();
        async move {
            strings(
                &conn,
                &format!(
                    "SELECT decision || ':' || reason FROM enterprise_routing_steps \
                     WHERE call_id = '{call}' ORDER BY step_number"
                ),
            )
            .await
        }
    };

    // The application blocks the contact, then (released) the caller blocks it: each
    // refuses the forwarding step with no public leg and no originate.
    let admin_rule = e
        .admin(
            Method::POST,
            "/v1/blocks",
            Some(json!({ "blockedContactReferenceId": reference, "reasonCode": "SPAM" })),
        )
        .await;
    assert_eq!(
        admin_rule.status,
        StatusCode::CREATED,
        "{}",
        admin_rule.text
    );
    let caller_rule = json!({ "principalId": caller.id(), "blockedContactReferenceId": reference,
                              "reasonCode": "SPAM" });
    for round in 0..2 {
        let created = dial(&e, &caller, &caller_phone, "7300").await;
        let call: Uuid = created["call"]["callId"].as_str().unwrap().parse().unwrap();
        let s = steps(call).await;
        assert!(
            s.contains(&"NO_ROUTE:DESTINATION_BLOCKED".to_string()),
            "round {round}: {s:?}"
        );
        assert_eq!(e.a.state(call).await.0, CallState::Failed);
        assert!(telephony.originates().is_empty(), "round {round}");
        if round == 0 {
            let id = admin_rule.json["blockId"].as_str().unwrap();
            let r = e
                .admin(Method::DELETE, &format!("/v1/blocks/{id}"), None)
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
            let r = e.a.post("/v1/blocks", caller_rule.clone()).await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        }
    }
    // Without any rule the same forwarding reaches the public leg.
    let rules =
        e.a.get(&format!("/v1/blocks?principalId={}", caller.id()))
            .await;
    let id = rules.json["items"][0]["blockId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = delete(&e.a, e.a.secret(), &format!("/v1/blocks/{id}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let created = dial(&e, &caller, &caller_phone, "7300").await;
    let call: Uuid = created["call"]["callId"].as_str().unwrap().parse().unwrap();
    assert!(
        steps(call)
            .await
            .contains(&"FORWARD_PUBLIC:ORIGINATING".to_string()),
        "{:?}",
        steps(call).await
    );
    assert_eq!(telephony.originates().len(), 1);
}

#[tokio::test]
async fn callback_relay_to_a_blocked_destination_is_refused() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let mine = x.privacy.add_contact("NG");
    let r = x
        .block(
            x.w.secret(),
            json!({ "principalId": x.w.w.initiator.id(), "blockedContactReferenceId": x.contact,
                    "reasonCode": "UNWANTED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let block = r.json["blockId"].as_str().unwrap().to_string();
    let refused = x.callback_to(mine, x.contact, "cb-1").await;
    assert_eq!(
        code(&refused),
        (StatusCode::FORBIDDEN, "PUBLIC_ROUTE_DENIED".into()),
        "{}",
        refused.text
    );
    assert!(x.privacy.requests().is_empty());
    assert!(x.telephony.originates().is_empty());
    assert_eq!(x.effects().await, (0, 0, 0));
    let r = delete(&x.w, x.w.secret(), &format!("/v1/blocks/{block}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let ok = x.callback_to(mine, x.contact, "cb-2").await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text);
}

#[tokio::test]
async fn inbound_callers_blocked_by_the_application_or_a_target_never_ring_it() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    x.configure_owner().await;
    let recipient = x.w.w.recipient.clone();
    let third = x.w.principal("third-1").await;
    for p in [&recipient, &third] {
        x.privacy.allow(p.id().as_uuid());
    }
    let recipient_phone = x.w.device(&recipient, "recipient-main").await;
    let third_phone = x.w.device(&third, "third-main").await;
    let caller = x.contact;
    let targets = [recipient.id().as_uuid(), third.id().as_uuid()];

    // The application blocks the caller: the offer is refused, nothing exists.
    let admin = x.app_admin().await;
    let r = x
        .block(
            admin.secret.expose_once(),
            json!({ "blockedContactReferenceId": caller, "reasonCode": "HARASSMENT" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let app_rule = r.json["blockId"].as_str().unwrap().to_string();
    let r = x
        .as_telephony(
            "/internal/v1/telephony/inbound-calls",
            x.offer(Uuid::new_v4(), caller, &targets),
        )
        .await;
    assert!(!r.status.is_success(), "{}", r.text);
    assert!(number_like_runs(&r.text).is_empty());
    assert!(x.privacy.requests().is_empty());
    assert_eq!(x.count("SELECT count(*) FROM calls").await, 0);
    let r = delete(
        &x.w,
        admin.secret.expose_once(),
        &format!("/v1/blocks/{app_rule}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);

    // The recipient blocks the caller: as the only target, no Call; with another target,
    // the Call rings that target only.
    let r = x
        .block(
            x.w.secret(),
            json!({ "principalId": recipient.id(), "blockedContactReferenceId": caller,
                    "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = x
        .as_telephony(
            "/internal/v1/telephony/inbound-calls",
            x.offer(Uuid::new_v4(), caller, &targets[..1]),
        )
        .await;
    assert!(!r.status.is_success(), "{}", r.text);
    assert_eq!(x.count("SELECT count(*) FROM calls").await, 0);
    let r = x
        .as_telephony(
            "/internal/v1/telephony/inbound-calls",
            x.offer(Uuid::new_v4(), caller, &targets),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let call: Uuid = r.json["callId"].as_str().unwrap().parse().unwrap();
    assert!(x
        .w
        .invitation_for(call, third_phone.endpoint)
        .await
        .is_some());
    assert!(x
        .w
        .invitation_for(call, recipient_phone.endpoint)
        .await
        .is_none());
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM call_participants WHERE call_id = '{call}' AND principal_id = '{}'",
            recipient.id()
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn creating_a_block_never_ends_a_connected_call() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let (call, _, _) = w.connected_call("VOICE").await;
    let (_, version) = w.state(call).await;
    let r = w
        .post(
            "/v1/blocks",
            json!({ "principalId": w.w.recipient.id(), "blockedPrincipalId": w.w.initiator.id(),
                    "reasonCode": "HARASSMENT" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    w.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(w.state(call).await, (CallState::Connected, version));
    // The parties may still end it themselves.
    let r = w.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn repeated_blocked_attempts_raise_exactly_one_incident_per_window() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let (a, b) = (&w.w.initiator, &w.w.recipient);
    let da = w.device(a, "a-main").await;
    let r = w
        .post(
            "/v1/blocks",
            json!({ "principalId": b.id(), "blockedPrincipalId": a.id(), "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let threshold =
        calling::calls::app::AbuseControls::default().blocked_attempts_incident_threshold;
    for i in 0..threshold - 1 {
        let r = w
            .create_call("VOICE", a, &da, &[b], &format!("rep-{i}"))
            .await;
        assert_eq!(code(&r).1, "TARGET_UNAVAILABLE");
    }
    assert!(incidents(w.h().conn(), "REPEATED_BLOCKED_ATTEMPTS")
        .await
        .is_empty());
    for i in threshold - 1..threshold + 3 {
        let r = w
            .create_call("VOICE", a, &da, &[b], &format!("rep-{i}"))
            .await;
        assert_eq!(code(&r).1, "TARGET_UNAVAILABLE");
    }
    let open = incidents(w.h().conn(), "REPEATED_BLOCKED_ATTEMPTS").await;
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0]["status"], "OPEN");
    assert_eq!(open[0]["occurrences"], 1);
    assert_eq!(open[0]["resourceType"], "PRINCIPAL");
    assert_eq!(open[0]["resourceId"], a.id().to_string());
    // Evidence is counts only.
    let evidence = open[0]["evidence"].to_string();
    assert!(!evidence.contains(&b.id().to_string()));
    assert_eq!(open[0]["evidence"]["refusals"], threshold);
    assert_eq!(w.w.count("SELECT count(*) FROM calls").await, 0);
    // The next window counts afresh; the open incident is not duplicated.
    w.h().clock.advance(chrono::Duration::minutes(10));
    for i in 0..threshold {
        w.create_call("VOICE", a, &da, &[b], &format!("rep-next-{i}"))
            .await;
    }
    let open = incidents(w.h().conn(), "REPEATED_BLOCKED_ATTEMPTS").await;
    assert_eq!(open.len(), 1, "{open:?}");
    // Rendered metrics carry bounded labels only — never a principal or contact id.
    let rendered = calling::metrics::render();
    assert!(rendered.contains("REPEATED_BLOCKED_ATTEMPTS"));
    for id in [a.id().to_string(), b.id().to_string()] {
        assert!(!rendered.contains(&id), "an identifier reached the metrics");
    }
}

#[tokio::test]
async fn repeated_route_denials_raise_one_incident_and_stay_unlabelled() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let denied = x.privacy.add_contact("FR");
    let threshold =
        calling::calls::app::AbuseControls::default().blocked_attempts_incident_threshold;
    for i in 0..threshold - 1 {
        let r = x.call_to(denied, &format!("deny-{i}")).await;
        assert_eq!(
            code(&r),
            (StatusCode::FORBIDDEN, "PUBLIC_ROUTE_DENIED".into()),
            "{}",
            r.text
        );
    }
    assert!(incidents(x.w.h().conn(), "REPEATED_ROUTE_DENIALS")
        .await
        .is_empty());
    for i in threshold - 1..threshold + 2 {
        x.call_to(denied, &format!("deny-{i}")).await;
    }
    let open = incidents(x.w.h().conn(), "REPEATED_ROUTE_DENIALS").await;
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0]["resourceId"], x.w.w.initiator.id().to_string());
    assert!(!open[0]["evidence"]
        .to_string()
        .contains(&denied.to_string()));
    assert!(x.telephony.originates().is_empty());
    assert_eq!(x.effects().await, (0, 0, 0));
    let rendered = calling::metrics::render();
    assert!(!rendered.contains(&denied.to_string()));
    assert!(!rendered.contains(&x.w.w.initiator.id().to_string()));
}
