//! Endpoints, ring offers and per-participant arbitration (Part C §5–§18, §31–§33, §82,
//! §87, §97–§104).

mod common;

use axum::http::{Method, StatusCode};
use calling::{calls::CallState, domain::EnvironmentKind, platform::scopes::names};
use common::{
    app::{AppWorld, Device},
    calls::CallWorld,
    TEST_PEPPER,
};
use serde_json::{json, Value};
use uuid::Uuid;

fn call_id(r: &common::TestResponse) -> Uuid {
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

async fn invitations(a: &AppWorld, call: Uuid) -> Vec<Value> {
    a.call_view(call).await["invitations"]
        .as_array()
        .unwrap()
        .clone()
}

async fn status_of(a: &AppWorld, call: Uuid, endpoint: Uuid) -> Vec<String> {
    invitations(a, call)
        .await
        .iter()
        .filter(|i| i["endpointId"] == endpoint.to_string())
        .map(|i| i["status"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn endpoints_belong_to_one_principal_of_one_application_environment() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let bob = a.w.recipient.clone();
    let bob_1 = a.device(&bob, "scoped-bob-1").await;
    // The same key for another principal is a conflict; refreshing one's own is not.
    let r = a
        .post(
            "/v1/calling/endpoints",
            json!({ "principalId": a.w.initiator.id(), "endpointKey": "scoped-bob-1", "endpointKind": "WEB", "capabilities": { "audio": true, "video": false } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let again = a.device(&bob, "scoped-bob-1").await;
    assert_eq!(again.endpoint, bob_1.endpoint);
    // Telephone-number keys and unknown fields are refused.
    for body in [
        json!({ "principalId": bob.id(), "endpointKey": "4155550123", "endpointKind": "MOBILE", "capabilities": { "audio": true, "video": false } }),
        json!({ "principalId": bob.id(), "endpointKey": "tel:+14155550123", "endpointKind": "MOBILE", "capabilities": { "audio": true, "video": false } }),
        json!({ "principalId": bob.id(), "endpointKey": "ok-key", "endpointKind": "MOBILE", "capabilities": { "audio": true, "video": false }, "tenantId": Uuid::new_v4() }),
        json!({ "principalId": bob.id(), "endpointKey": "ok-key", "endpointKind": "PSTN", "capabilities": { "audio": true, "video": false } }),
    ] {
        let r = a.post("/v1/calling/endpoints", body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text);
    }
    // An unknown principal is 404.
    let r = a
        .post(
            "/v1/calling/endpoints",
            json!({ "principalId": Uuid::new_v4(), "endpointKey": "nobody", "endpointKind": "WEB", "capabilities": { "audio": true, "video": false } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Another application (same database) sees and touches nothing of this one.
    let other = AppWorld {
        w: CallWorld::in_harness(
            a.h().sibling(TEST_PEPPER),
            "other-app",
            EnvironmentKind::Production,
        )
        .await,
    };
    let r = other
        .get(&format!("/v1/calling/endpoints?principalId={}", bob.id()))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json["data"].as_array().unwrap().is_empty());
    let r = other
        .h()
        .request(
            Method::DELETE,
            &format!("/v1/calling/endpoints/{}", bob_1.endpoint),
            Some(other.secret()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    for body in [
        json!({ "principalId": bob.id(), "endpointKey": "steal", "endpointKind": "WEB", "capabilities": { "audio": true, "video": false } }),
    ] {
        assert_eq!(
            other.post("/v1/calling/endpoints", body).await.status,
            StatusCode::NOT_FOUND
        );
    }
    let r = other
        .post(
            "/v1/calling/signaling-credentials",
            json!({ "principalId": bob.id(), "endpointId": bob_1.endpoint }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let foreign_caller = other.device(&other.w.initiator, "foreign-caller").await;
    let r = other
        .create_call(
            "VOICE",
            &other.w.initiator,
            &foreign_caller,
            &[&bob],
            "foreign-1",
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "a foreign principal is never rung: {}",
        r.text
    );
    // Nor can it use this application's endpoint as its own initiator endpoint.
    let r = other
        .post_with(
            "/v1/calls",
            json!({ "kind": "VOICE", "initiatorPrincipalId": other.w.initiator.id(), "initiatorEndpointId": bob_1.endpoint, "targetPrincipalIds": [other.w.recipient.id()] }),
            &[("idempotency-key", "foreign-2")],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn signaling_credentials_are_separate_from_api_credentials() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let bob_1 = a.device(&a.w.recipient, "sig-bob-1").await;
    let token = a.signaling_token(&bob_1).await;
    assert!(token.starts_with("cfsig_"));
    // A signaling credential is not an API credential, and vice versa.
    let r = a
        .as_endpoint(&token, Method::GET, "/v1/calls", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = a
        .as_endpoint(
            &token,
            Method::POST,
            "/v1/calling/endpoints",
            Some(json!({})),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = a
        .as_endpoint(
            a.secret(),
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "API credentials never reach signaling routes"
    );
    let r = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::OK);
    // A tampered credential fails uniformly.
    let mut tampered = token.clone();
    tampered.pop();
    tampered.push(if token.ends_with('A') { 'B' } else { 'A' });
    let r = a
        .as_endpoint(&tampered, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Minting requires calls.write.
    let read_only = a
        .h()
        .issue(
            a.w.consumer.sa(EnvironmentKind::Production),
            &[names::CALLS_READ],
        )
        .await;
    let r = a
        .h()
        .request(
            Method::POST,
            "/v1/calling/signaling-credentials",
            Some(read_only.secret.expose_once()),
            Some(json!({ "principalId": a.w.recipient.id(), "endpointId": bob_1.endpoint })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Revoking the minting API credential ends the signaling credential's authority.
    let minter = a
        .h()
        .issue(
            a.w.consumer.sa(EnvironmentKind::Production),
            &[names::CALLS_WRITE],
        )
        .await;
    let r = a
        .h()
        .request(
            Method::POST,
            "/v1/calling/signaling-credentials",
            Some(minter.secret.expose_once()),
            Some(json!({ "principalId": a.w.recipient.id(), "endpointId": bob_1.endpoint })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let minted = r.json["token"].as_str().unwrap().to_string();
    let ok = a
        .as_endpoint(&minted, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(ok.status, StatusCode::OK);
    a.h()
        .platform
        .revoke_credential(minter.credential.id)
        .await
        .unwrap();
    let r = a
        .as_endpoint(&minted, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Expiry is enforced on the service clock.
    a.h().clock.advance(chrono::Duration::hours(2));
    let r = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_endpoints_stop_ringing_and_lose_signaling() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "rev-caller").await;
    let bob_1 = a.device(&a.w.recipient, "rev-bob-1").await;
    let bob_2 = a.device(&a.w.recipient, "rev-bob-2").await;
    let token = a.signaling_token(&bob_1).await;
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &[&a.w.recipient], "rev-1")
        .await;
    let call = call_id(&r);
    assert_eq!(status_of(&a, call, bob_1.endpoint).await, ["RINGING"]);
    let r = a
        .h()
        .request(
            Method::DELETE,
            &format!("/v1/calling/endpoints/{}", bob_1.endpoint),
            Some(a.secret()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["status"], "REVOKED");
    // Its live offer is cancelled; the other device keeps ringing; the Call too.
    let view = invitations(&a, call).await;
    let offer = view
        .iter()
        .find(|i| i["endpointId"] == bob_1.endpoint.to_string())
        .unwrap();
    assert_eq!(offer["status"], "CANCELLED");
    assert_eq!(offer["terminalReason"], "ENDPOINT_REVOKED");
    assert_eq!(status_of(&a, call, bob_2.endpoint).await, ["RINGING"]);
    assert_eq!(a.state(call).await.0, CallState::Ringing);
    let r = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // New calls never ring it; it cannot initiate.
    let (_, version) = a.state(call).await;
    a.command(call, "cancel", version).await;
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &[&a.w.recipient], "rev-2")
        .await;
    let call = call_id(&r);
    assert!(status_of(&a, call, bob_1.endpoint).await.is_empty());
    assert_eq!(status_of(&a, call, bob_2.endpoint).await, ["RINGING"]);
    let r = a
        .create_call("VOICE", &a.w.recipient, &bob_1, &[&a.w.initiator], "rev-3")
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn presence_is_a_hint_and_only_audio_capable_endpoints_ring() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "hint-caller").await;
    // Never connected (OFFLINE) endpoints are still rung: presence never authorizes or
    // excludes.
    let offline = a.device(&a.w.recipient, "hint-offline").await;
    let r = a
        .post(
            "/v1/calling/endpoints",
            json!({ "principalId": a.w.recipient.id(), "endpointKey": "hint-audio-only", "endpointKind": "DESKTOP", "capabilities": { "audio": true, "video": false } }),
        )
        .await;
    let audio_only: Uuid = r.json["endpointId"].as_str().unwrap().parse().unwrap();
    assert_eq!(r.json["presence"], "OFFLINE");
    let r = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "hint-1",
        )
        .await;
    let voice = call_id(&r);
    assert_eq!(status_of(&a, voice, offline.endpoint).await, ["RINGING"]);
    assert_eq!(status_of(&a, voice, audio_only).await, ["RINGING"]);
    let r = a
        .create_call(
            "VIDEO",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "hint-2",
        )
        .await;
    let video = call_id(&r);
    assert_eq!(status_of(&a, video, offline.endpoint).await, ["RINGING"]);
    // Every application Call needs audio; video is optional per endpoint.
    assert_eq!(status_of(&a, video, audio_only).await, ["RINGING"]);
    let r = a
        .post(
            "/v1/calling/endpoints",
            json!({ "principalId": a.w.recipient.id(), "endpointKey": "hint-video-only", "endpointKind": "WEB", "capabilities": { "audio": false, "video": true } }),
        )
        .await;
    let video_only: Uuid = r.json["endpointId"].as_str().unwrap().parse().unwrap();
    let r = a
        .create_call(
            "VIDEO",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "hint-3",
        )
        .await;
    assert!(
        status_of(&a, call_id(&r), video_only).await.is_empty(),
        "endpoints without audio are never rung"
    );
}

#[tokio::test]
async fn one_device_decline_declines_only_that_offer() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let carol = a.principal("decline-carol").await;
    let caller = a.device(&a.w.initiator, "decline-caller").await;
    let bob_1 = a.device(&a.w.recipient, "decline-bob-1").await;
    let bob_2 = a.device(&a.w.recipient, "decline-bob-2").await;
    let carol_1 = a.device(&carol, "decline-carol-1").await;

    // One-to-one: bob_1 declines, bob_2 still rings, then bob_2 declines → DECLINED.
    let r = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "decline-1",
        )
        .await;
    let call = call_id(&r);
    let offer = a.invitation_for(call, bob_1.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.decline(call, offer, &bob_1, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "RINGING");
    assert_eq!(status_of(&a, call, bob_2.endpoint).await, ["RINGING"]);
    // Declining again is a replay of the same outcome, not an error.
    let (_, version) = a.state(call).await;
    let again = a.decline(call, offer, &bob_1, version).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    // A declined offer can never be accepted.
    let r = a.accept(call, offer, &bob_1, version).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let offer_2 = a.invitation_for(call, bob_2.endpoint).await.unwrap();
    let r = a.decline(call, offer_2, &bob_2, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Declined);
    assert!(!a.media().room_exists(&AppWorld::room(call)));

    // Group: bob declines on both devices while carol still rings → still RINGING; then
    // carol answers → ANSWERED (bob is simply not part of the conversation).
    let r = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient, &carol],
            "decline-2",
        )
        .await;
    let call = call_id(&r);
    for device in [&bob_1, &bob_2] {
        let offer = a.invitation_for(call, device.endpoint).await.unwrap();
        let (_, version) = a.state(call).await;
        assert_eq!(
            a.decline(call, offer, device, version).await.status,
            StatusCode::OK
        );
    }
    assert_eq!(a.state(call).await.0, CallState::Ringing);
    let offer = a.invitation_for(call, carol_1.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, offer, &carol_1, version).await.status,
        StatusCode::OK
    );
    assert_eq!(a.state(call).await.0, CallState::Answered);
}

#[tokio::test]
async fn offers_expire_on_the_server_and_are_never_revived() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "exp-caller").await;
    let bob_1 = a.device(&a.w.recipient, "exp-bob-1").await;
    let token = a.signaling_token(&bob_1).await;
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &[&a.w.recipient], "exp-1")
        .await;
    let call = call_id(&r);
    let offer = a.invitation_for(call, bob_1.endpoint).await.unwrap();
    let recovered = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(recovered.json["data"][0]["actionable"], true);
    // Past the server expiry the offer is refused even before the worker ran.
    a.h().clock.advance(chrono::Duration::seconds(46));
    let (_, version) = a.state(call).await;
    let r = a.accept(call, offer, &bob_1, version).await;
    assert_eq!(r.status, StatusCode::GONE, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "INVITATION_EXPIRED");
    let recovered = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(recovered.json["data"][0]["actionable"], false);
    // The worker expires it and the Call ends NO_ANSWER; nothing revives it.
    let report = a.h().state.app.expire_due(100).await.unwrap();
    let _ = report;
    assert_eq!(a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(status_of(&a, call, bob_1.endpoint).await, ["EXPIRED"]);
    let (_, version) = a.state(call).await;
    for r in [
        a.accept(call, offer, &bob_1, version).await,
        a.as_endpoint(
            &token,
            Method::POST,
            &format!("/v1/calling/invitations/{offer}/accept"),
            Some(json!({ "expectedVersion": version })),
            &[],
        )
        .await,
    ] {
        assert!(
            r.status == StatusCode::GONE || r.status == StatusCode::CONFLICT,
            "{}",
            r.text
        );
    }
    assert_eq!(status_of(&a, call, bob_1.endpoint).await, ["EXPIRED"]);
    assert_eq!(a.state(call).await.0, CallState::NoAnswer);
    // Running the worker again changes nothing.
    a.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(a.state(call).await.0, CallState::NoAnswer);
    // A terminal Call cannot be answered, cancelled into another state or transferred.
    let r = a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
}

#[tokio::test]
async fn group_calls_have_one_winning_device_per_participant() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let carol = a.principal("group-carol").await;
    let caller = a.device(&a.w.initiator, "group-caller").await;
    let bob: Vec<Device> = vec![
        a.device(&a.w.recipient, "group-bob-1").await,
        a.device(&a.w.recipient, "group-bob-2").await,
    ];
    let carol_d: Vec<Device> = vec![
        a.device(&carol, "group-carol-1").await,
        a.device(&carol, "group-carol-2").await,
    ];
    let bob_token = a.signaling_token(&bob[1]).await;
    let r = a
        .create_call(
            "VIDEO",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient, &carol],
            "group-1",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["liveInvitationCount"], 4);
    let call = call_id(&r);
    // bob answers on device 1; device 2's offer is answered elsewhere and stale for it.
    let offer = a.invitation_for(call, bob[0].endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.accept(call, offer, &bob[0], version).await;
    assert_eq!(r.json["callAnswered"], true);
    let stale = invitations(&a, call)
        .await
        .into_iter()
        .find(|i| i["endpointId"] == bob[1].endpoint.to_string())
        .unwrap();
    assert_eq!(stale["status"], "ANSWERED_ELSEWHERE");
    let (_, version) = a.state(call).await;
    let r = a
        .as_endpoint(
            &bob_token,
            Method::POST,
            &format!(
                "/v1/calling/invitations/{}/accept",
                stale["invitationId"].as_str().unwrap()
            ),
            Some(json!({ "expectedVersion": version })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["error"]["code"], "ANSWERED_ELSEWHERE");
    // carol's devices still ring: her own arbitration is independent of bob's.
    assert_eq!(status_of(&a, call, carol_d[0].endpoint).await, ["RINGING"]);
    let offer = a.invitation_for(call, carol_d[1].endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.accept(call, offer, &carol_d[1], version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json["callAnswered"], false,
        "the first answer moved the Call to ANSWERED"
    );
    assert_eq!(
        status_of(&a, call, carol_d[0].endpoint).await,
        ["ANSWERED_ELSEWHERE"]
    );
    // One authorized session per participant (plus the initiator's).
    let view = a.call_view(call).await;
    let authorized: Vec<&Value> = view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["mediaAccess"] == "AUTHORIZED")
        .collect();
    assert_eq!(authorized.len(), 3);
    for (device, expected) in [
        (&bob[0], true),
        (&bob[1], false),
        (&carol_d[0], false),
        (&carol_d[1], true),
    ] {
        assert_eq!(
            a.session_on(call, device.endpoint).await.is_some(),
            expected
        );
    }
    // All three join; group media uses one resource and video for every participant.
    for device in [&caller, &bob[0], &carol_d[1]] {
        let session = a.session_on(call, device.endpoint).await.unwrap();
        a.connect(call, session).await;
    }
    assert_eq!(a.state(call).await.0, CallState::Connected);
    let participants = a.media().participants(&AppWorld::room(call));
    assert_eq!(participants.len(), 3);
    assert!(participants.iter().all(|p| p.permissions.can_publish_video));
}

#[tokio::test]
async fn group_size_is_limited_and_targets_are_validated() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "limit-caller").await;
    let mut targets = Vec::new();
    for i in 0..16 {
        targets.push(a.principal(&format!("limit-{i}")).await);
    }
    let refs: Vec<_> = targets.iter().collect();
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &refs, "limit-1")
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "PARTICIPANT_LIMIT_EXCEEDED");
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &refs[..15], "limit-2")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // Duplicates, self-calls and empty target lists are invalid.
    for (targets, key) in [
        (vec![&a.w.recipient, &a.w.recipient], "limit-3"),
        (vec![&a.w.initiator], "limit-4"),
        (vec![], "limit-5"),
    ] {
        let r = a
            .create_call("VOICE", &a.w.initiator, &caller, &targets, key)
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{key}: {}", r.text);
    }
    // Client-chosen state, scope, room or permissions are refused.
    for extra in [
        json!({ "state": "CONNECTED" }),
        json!({ "tenantId": Uuid::new_v4() }),
        json!({ "room": "cfc-mine" }),
        json!({ "permissions": { "canPublishVideo": true } }),
        json!({ "provider": "x" }),
    ] {
        let mut body = json!({ "kind": "VOICE", "initiatorPrincipalId": a.w.initiator.id(), "initiatorEndpointId": caller.endpoint, "targetPrincipalIds": [a.w.recipient.id()] });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let r = a
            .post_with("/v1/calls", body, &[("idempotency-key", "limit-extra")])
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{extra}: {}", r.text);
    }
    // Idempotency-Key is required to create a call.
    let r = a
        .post(
            "/v1/calls",
            json!({ "kind": "VOICE", "initiatorPrincipalId": a.w.initiator.id(), "initiatorEndpointId": caller.endpoint, "targetPrincipalIds": [a.w.recipient.id()] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn past_the_endpoint_cap_the_newest_registrations_ring() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "cap-caller").await;
    let mut devices = Vec::new();
    for i in 0..11 {
        a.h().clock.advance(chrono::Duration::seconds(1));
        devices.push(a.device(&a.w.recipient, &format!("cap-device-{i}")).await);
    }
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &[&a.w.recipient], "cap-1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["liveInvitationCount"], 10, "at most the cap is rung");
    let call = call_id(&r);
    assert!(
        status_of(&a, call, devices[0].endpoint).await.is_empty(),
        "the oldest (never seen) registration is the one left out"
    );
    assert_eq!(status_of(&a, call, devices[10].endpoint).await, ["RINGING"]);
}
