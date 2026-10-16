//! §30: a Relay Session can only NARROW its context.

mod common;

use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use serde_json::json;

#[tokio::test]
async fn sessions_never_broaden_their_context() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b, cc, outsider] = principals(4).try_into().unwrap();
    let mut req = context_request(&[a, b, cc], "EXPLICIT", &[(a, b), (b, a), (a, cc)]);
    req["startsAt"] = json!(iso(at(0)));
    req["expiresAt"] = json!(iso(at(60)));
    req["graceExpiresAt"] = json!(iso(at(70)));
    let ctx = h.create_context(&c, req).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let path = format!("/internal/v1/contexts/{id}/sessions");
    let p = |s| json!({ "principalId": s });
    for (what, body, code) in [
        (
            "starts before the context",
            json!({ "relayStrategy": "APP_ONLY", "startsAt": iso(at(-5)) }),
            "VALIDITY_NOT_CONTAINED",
        ),
        (
            "expires after the context",
            json!({ "relayStrategy": "APP_ONLY", "expiresAt": iso(at(61)) }),
            "VALIDITY_NOT_CONTAINED",
        ),
        (
            "grace beyond the context",
            json!({ "relayStrategy": "APP_ONLY", "expiresAt": iso(at(60)), "graceExpiresAt": iso(at(80)) }),
            "VALIDITY_NOT_CONTAINED",
        ),
        (
            "participant outside the context",
            json!({ "relayStrategy": "APP_ONLY", "participants": [p(a), p(outsider)] }),
            "PARTICIPANT_NOT_AUTHORIZED",
        ),
        (
            "channel outside the context",
            json!({ "relayStrategy": "APP_ONLY", "allowedChannels": ["VOICE", "MESSAGING"] }),
            "CHANNEL_NOT_ALLOWED",
        ),
        (
            "forbidden direction",
            json!({ "relayStrategy": "APP_ONLY", "directions": [{ "from": p(cc), "to": p(a) }] }),
            "DIRECTION_NOT_ALLOWED",
        ),
        (
            "bidirectional on an explicit context",
            json!({ "relayStrategy": "APP_ONLY", "directionPolicy": "BIDIRECTIONAL" }),
            "DIRECTION_NOT_ALLOWED",
        ),
        (
            "strategy beyond the context",
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
            "STRATEGY_NOT_ALLOWED",
        ),
        (
            "client relay identity",
            json!({ "relayStrategy": "APP_ONLY", "relayIdentityBindingRef": uuid::Uuid::new_v4() }),
            "VALIDATION_ERROR",
        ),
        (
            "client relay number",
            json!({ "relayStrategy": "APP_ONLY", "relayNumber": "+2348012345678" }),
            "VALIDATION_ERROR",
        ),
        (
            "one participant",
            json!({ "relayStrategy": "APP_ONLY", "participants": [p(a)] }),
            "VALIDATION_ERROR",
        ),
    ] {
        let r = h.command(&path, &c, body).await;
        assert_eq!(r.code(), code, "{what}: {}", r.text);
    }
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_sessions").await,
        0
    );

    // A narrower session passes and is narrower in effect.
    let narrow = h
        .create_session(
            &c,
            &id,
            json!({
                "relayStrategy": "APP_ONLY",
                "participants": [p(a), p(b)],
                "directions": [{ "from": p(a), "to": p(b) }],
                "startsAt": iso(at(5)),
                "expiresAt": iso(at(30)),
            }),
        )
        .await;
    assert_eq!(narrow["participantScope"], "NARROWED");
    assert_eq!(narrow["directionScope"], "NARROWED");
    assert_eq!(narrow["directionPolicy"], "EXPLICIT");
    let sid = narrow["relaySessionId"].as_str().unwrap();
    h.clock.set(at(10));
    assert_eq!(h.reason(&c, sid, a, &[b]).await, "AUTHORIZED");
    assert_eq!(
        h.reason(&c, sid, b, &[a]).await,
        "DIRECTION_NOT_ALLOWED",
        "the context allows it, the session does not"
    );
    assert_eq!(
        h.reason(&c, sid, a, &[cc]).await,
        "PARTICIPANT_NOT_AUTHORIZED"
    );
    // The database independently refuses a session outside its context.
    use sea_orm::ConnectionTrait;
    let err = h
        .conn()
        .execute_unprepared(&format!(
            "UPDATE privacy_relay_sessions SET expires_at = '2031-03-01T12:00:00Z' WHERE id = '{sid}'"
        ))
        .await;
    assert!(err.is_err(), "containment trigger");
    // Narrowing the context narrows (clamps) the session; it never widens it back.
    let r = h.command(&format!("/internal/v1/contexts/{id}/update"), &c, json!({ "expectedVersion": 1, "expiresAt": iso(at(20)), "graceExpiresAt": iso(at(20)) })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let s = h
        .get(&format!("/internal/v1/sessions/{sid}"), &c)
        .await
        .json;
    assert_eq!(s["expiresAt"], "2031-03-01T10:20:00.000000Z");
    let r = h.command(&format!("/internal/v1/contexts/{id}/update"), &c, json!({ "expectedVersion": 2, "expiresAt": iso(at(50)), "graceExpiresAt": iso(at(50)) })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let s = h
        .get(&format!("/internal/v1/sessions/{sid}"), &c)
        .await
        .json;
    assert_eq!(
        s["expiresAt"], "2031-03-01T10:20:00.000000Z",
        "a session never re-broadens"
    );
    // A context narrowed to before the session's start expires the session.
    let later = h
        .create_session(
            &c,
            &id,
            json!({ "relayStrategy": "APP_ONLY", "startsAt": iso(at(40)) }),
        )
        .await;
    let later_id = later["relaySessionId"].as_str().unwrap();
    let r = h.command(&format!("/internal/v1/contexts/{id}/update"), &c, json!({ "expectedVersion": 3, "expiresAt": iso(at(35)), "graceExpiresAt": iso(at(35)) })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let s = h
        .get(&format!("/internal/v1/sessions/{later_id}"), &c)
        .await
        .json;
    assert_eq!(s["status"], "EXPIRED");
}

#[tokio::test]
async fn a_narrowed_session_does_not_gain_new_context_participants() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b, cc] = principals(3).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let narrowed = h
        .create_session(&c, &id, json!({ "relayStrategy": "APP_ONLY", "participants": [{ "principalId": a }, { "principalId": b }] }))
        .await;
    let inherited = h.app_session(&c, &id).await;
    let r = h.command(
        &format!("/internal/v1/contexts/{id}/update"),
        &c,
        json!({ "expectedVersion": 1, "participants": [{ "principalId": a }, { "principalId": b }, { "principalId": cc }] }),
    ).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        h.reason(&c, inherited["relaySessionId"].as_str().unwrap(), a, &[cc])
            .await,
        "AUTHORIZED"
    );
    assert_eq!(
        h.reason(&c, narrowed["relaySessionId"].as_str().unwrap(), a, &[cc])
            .await,
        "PARTICIPANT_NOT_AUTHORIZED"
    );
}
