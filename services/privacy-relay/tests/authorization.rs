//! §29–§30: communication authorization — directions, time windows, completion grace,
//! session reuse across calls, executable capability.

mod common;

use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind as Kind;
use chrono::Duration;
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::json;

#[tokio::test]
async fn explicit_directions_are_asymmetric_until_a_reverse_edge_exists() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h
        .create_context(&c, context_request(&[a, b], "EXPLICIT", &[(a, b)]))
        .await;
    let id = ctx["contextId"].as_str().unwrap();
    let s = h.app_session(&c, id).await;
    let sid = s["relaySessionId"].as_str().unwrap();
    assert_eq!(h.reason(&c, sid, a, &[b]).await, "AUTHORIZED");
    assert_eq!(h.reason(&c, sid, b, &[a]).await, "DIRECTION_NOT_ALLOWED");

    // Add the reverse edge through a versioned update: B -> A is now allowed, also for
    // the existing (inherited) session.
    let r = h
        .command(
            &format!("/internal/v1/contexts/{id}/update"),
            &c,
            json!({
                "expectedVersion": 1,
                "directions": [
                    { "from": { "principalId": a }, "to": { "principalId": b } },
                    { "from": { "principalId": b }, "to": { "principalId": a } },
                ],
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["context"]["version"], 2);
    assert_eq!(h.reason(&c, sid, b, &[a]).await, "AUTHORIZED");
    assert_eq!(h.reason(&c, sid, a, &[b]).await, "AUTHORIZED");

    // BIDIRECTIONAL: both directions without edges.
    let bi = h.bidirectional(&c, &[a, b]).await;
    let bs = h.app_session(&c, bi["contextId"].as_str().unwrap()).await;
    let bsid = bs["relaySessionId"].as_str().unwrap();
    assert_eq!(h.reason(&c, bsid, a, &[b]).await, "AUTHORIZED");
    assert_eq!(h.reason(&c, bsid, b, &[a]).await, "AUTHORIZED");
    // A non-participant never is.
    let outsider = uuid::Uuid::new_v4();
    assert_eq!(
        h.reason(&c, bsid, a, &[outsider]).await,
        "PARTICIPANT_NOT_AUTHORIZED"
    );
    assert_eq!(
        h.reason(&c, bsid, outsider, &[a]).await,
        "PARTICIPANT_NOT_AUTHORIZED"
    );

    // Every decision was recorded, AUTHORIZED and DENIED alike.
    assert_eq!(
        h.count("SELECT count(*) FROM relay_authorization_decisions")
            .await,
        8
    );
    assert_eq!(
        h.count("SELECT count(*) FROM relay_authorization_decisions WHERE decision = 'DENIED'")
            .await,
        3
    );
}

#[tokio::test]
async fn authority_follows_the_server_clock_without_waiting_for_workers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let mut req = context_request(&[a, b], "BIDIRECTIONAL", &[]);
    req["startsAt"] = json!(iso(at(10)));
    req["expiresAt"] = json!(iso(at(60)));
    req["graceExpiresAt"] = json!(iso(at(90)));
    req["policy"] = json!({ "graceAllowsNewCalls": true });
    let ctx = h.create_context(&c, req).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let s = h.app_session(&c, &id).await;
    let sid = s["relaySessionId"].as_str().unwrap().to_string();

    // Before startsAt.
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "CONTEXT_NOT_STARTED");
    // During the window.
    h.clock.set(at(10));
    let ok = h.authorize(&c, &sid, a, &[b]).await;
    assert_eq!(ok.json["decision"], "AUTHORIZED");
    assert_eq!(ok.json["validUntil"], "2031-03-01T11:00:00.000000Z");
    // After expiresAt (ACTIVE, not completed): denied — even though no worker ran.
    h.clock.set(at(61));
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "CONTEXT_EXPIRED");
    assert_eq!(
        h.count("SELECT count(*) FROM communication_contexts WHERE status = 'ACTIVE'")
            .await,
        1
    );

    // A completed context inside its grace, with grace authority.
    h.clock.set(at(30));
    let v = h.version_of(&c, &id).await;
    assert_eq!(
        h.lifecycle(&c, &id, "complete", v).await.status,
        StatusCode::OK
    );
    h.clock.set(at(70));
    let grace = h.authorize(&c, &sid, a, &[b]).await;
    assert_eq!(grace.json["decision"], "AUTHORIZED", "{}", grace.text);
    assert_eq!(grace.json["validUntil"], "2031-03-01T11:30:00.000000Z");
    // After the final grace: everything is denied; then the worker materializes EXPIRED.
    h.clock.set(at(90));
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "CONTEXT_EXPIRED");
    let report = h.relay.run_sweep().await.unwrap();
    assert_eq!(report.contexts_expired, 1);
    let after = h.get(&format!("/internal/v1/contexts/{id}"), &c).await;
    assert_eq!(after.json["status"], "EXPIRED");
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "CONTEXT_EXPIRED");
}

#[tokio::test]
async fn grace_policy_decides_new_calls_during_grace() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Test);
    let [a, b] = principals(2).try_into().unwrap();
    for (allow, expected) in [(true, "AUTHORIZED"), (false, "GRACE_POLICY_DENIED")] {
        let mut req = context_request(&[a, b], "BIDIRECTIONAL", &[]);
        req["graceExpiresAt"] = json!(iso(at(90)));
        req["policy"] = json!({ "graceAllowsNewCalls": allow });
        let ctx = h.create_context(&c, req).await;
        let id = ctx["contextId"].as_str().unwrap();
        let sid = h.app_session(&c, id).await["relaySessionId"]
            .as_str()
            .unwrap()
            .to_string();
        let r = h.lifecycle(&c, id, "complete", 1).await;
        assert_eq!(r.json["context"]["status"], "COMPLETED");
        // Grace remains: no active-call termination is requested at completion.
        assert_eq!(r.json["enforcement"]["status"], "NOT_REQUIRED");
        assert_eq!(
            h.reason(&c, &sid, a, &[b]).await,
            expected,
            "grace allows: {allow}"
        );
    }
    // Completion WITHOUT grace ends new authority and terminates active calls.
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap();
    let sid = h.app_session(&c, id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = h.lifecycle(&c, id, "complete", 1).await;
    assert_eq!(r.json["enforcement"]["status"], "APPLIED");
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "CONTEXT_NOT_ACTIVE");
    // COMPLETED never returns to ACTIVE.
    let resume = h.lifecycle(&c, id, "resume", 2).await;
    assert_eq!(
        (resume.status, resume.code()),
        (StatusCode::CONFLICT, "CONTEXT_NOT_ACTIVE")
    );
    assert!(h.conn().execute_unprepared(&format!(
        "UPDATE communication_contexts SET status = 'ACTIVE', version = version + 1 WHERE id = '{id}'"
    )).await.is_err(), "the database refuses COMPLETED -> ACTIVE");
}

#[tokio::test]
async fn one_relay_session_authorizes_many_call_attempts() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let sid = h.app_session(&c, ctx["contextId"].as_str().unwrap()).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let mut decisions = Vec::new();
    for minute in [1, 5, 20] {
        h.clock.set(at(minute));
        let r = h
            .authorize(
                &c,
                &sid,
                if minute == 5 { b } else { a },
                &[if minute == 5 { a } else { b }],
            )
            .await;
        assert_eq!(r.json["decision"], "AUTHORIZED");
        assert_eq!(r.json["relaySessionId"], sid);
        decisions.push(r.json["decisionId"].clone());
    }
    decisions.dedup();
    assert_eq!(decisions.len(), 3, "distinct decisions, one stable session");
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_sessions").await,
        1
    );
}

#[tokio::test]
async fn routes_channels_and_contact_references_are_not_faked() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    // A real Contact Vault contact of this scope (contexts never take unknown references).
    let contact = h.create_contact(&c, "+39 02 1234 5678").await["contactReference"].clone();
    let mut req = context_request(&[a, b], "BIDIRECTIONAL", &[]);
    req["participants"] =
        json!([{ "principalId": a }, { "principalId": b }, { "contactReferenceId": contact }]);
    req["allowedChannels"] = json!(["VOICE", "MESSAGING"]);
    req["allowedRelayStrategies"] = json!(["APP_ONLY", "PUBLIC_RELAY_ALLOWED"]);
    let ctx = h.create_context(&c, req).await;
    let id = ctx["contextId"].as_str().unwrap();
    let public = h
        .create_session(&c, id, json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }))
        .await;
    assert_eq!(public["executableRoutes"], json!(["APPLICATION"]));
    assert!(public.get("relayIdentityBindingRef").is_none());
    let sid = public["relaySessionId"].as_str().unwrap();
    let ask = |source: serde_json::Value, target: serde_json::Value, channel: &str, route: &str| {
        json!({
            "relaySessionId": sid,
            "source": source,
            "targets": [target],
            "channel": channel,
            "route": route,
        })
    };
    let reason = |body: serde_json::Value| {
        let h = &h;
        let c = c.clone();
        async move {
            let r = h
                .post("/internal/v1/communication/authorize", &c, body)
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
            r.json["reason"].as_str().unwrap().to_string()
        }
    };
    let pa = json!({ "principalId": a });
    let pb = json!({ "principalId": b });
    let pc = json!({ "contactReferenceId": contact });
    assert_eq!(
        reason(ask(pa.clone(), pb.clone(), "VOICE", "APPLICATION")).await,
        "AUTHORIZED"
    );
    // Policy-allowed public relay is NOT executable yet: no fake telephone route.
    assert_eq!(
        reason(ask(pa.clone(), pb.clone(), "VOICE", "PUBLIC_RELAY")).await,
        "CAPABILITY_UNAVAILABLE"
    );
    // Callback relay is not allowed by this session's strategy at all.
    assert_eq!(
        reason(ask(pa.clone(), pb.clone(), "VOICE", "CALLBACK_RELAY")).await,
        "STRATEGY_NOT_ALLOWED"
    );
    // MESSAGING is a declaration, never executed.
    assert_eq!(
        reason(ask(pa.clone(), pb.clone(), "MESSAGING", "APPLICATION")).await,
        "CAPABILITY_UNAVAILABLE"
    );
    // A ContactReference participant is never reachable over the APPLICATION route, and
    // over the public network it needs an effectively VERIFIED contact (§33).
    assert_eq!(
        reason(ask(pa.clone(), pc.clone(), "VOICE", "APPLICATION")).await,
        "CAPABILITY_UNAVAILABLE"
    );
    assert_eq!(
        reason(ask(pc, pa, "VOICE", "PUBLIC_RELAY")).await,
        "CONTACT_NOT_VERIFIED"
    );
    // The session strategy cannot exceed the context's allowed strategies.
    let r = h
        .command(
            &format!("/internal/v1/contexts/{id}/sessions"),
            &c,
            json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::UNPROCESSABLE_ENTITY, "STRATEGY_NOT_ALLOWED")
    );
    // A session window shorter than the context is honoured.
    h.clock.set(t0() + Duration::minutes(5));
    let short = h
        .create_session(
            &c,
            id,
            json!({ "relayStrategy": "APP_ONLY", "expiresAt": iso(at(20)) }),
        )
        .await;
    let short_id = short["relaySessionId"].as_str().unwrap();
    h.clock.set(at(25));
    assert_eq!(h.reason(&c, short_id, a, &[b]).await, "SESSION_EXPIRED");
    assert_eq!(h.reason(&c, sid, a, &[b]).await, "AUTHORIZED");
}
