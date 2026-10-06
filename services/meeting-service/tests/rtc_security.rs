//! Phase 2 Part E — permanent security suite for the RTC control plane (spec §17).

mod common;

use axum::http::StatusCode;
use common::{livekit_mock, rtc::*, *};
use livekit_api::access_token::{AccessToken, TokenVerifier, VideoGrants};
use livekit_protocol as proto;
use serde_json::json;
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::livekit().await {
            Some(a) => a,
            None => return,
        }
    };
}

/// S1. Forged / unsigned / tampered webhooks are rejected before touching state.
#[tokio::test]
async fn s01_forged_webhook_rejected() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s1", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    let body = livekit_mock::webhook_body(
        "participant_joined",
        "EV_forged",
        &room(cid),
        None,
        Some(&p.identity),
    );
    let other_body = livekit_mock::webhook_body(
        "participant_left",
        "EV_forged",
        &room(cid),
        None,
        Some(&p.identity),
    );
    let cases = vec![
        ("no signature", None),
        (
            "wrong secret",
            Some(livekit_mock::sign_webhook(
                &body,
                livekit_mock::API_KEY,
                "attacker-secret-attacker-secret-0000",
            )),
        ),
        (
            "wrong api key",
            Some(livekit_mock::sign_webhook(
                &body,
                "attacker-key",
                livekit_mock::API_SECRET,
            )),
        ),
        (
            "body swapped",
            Some(livekit_mock::sign_webhook(
                &other_body,
                livekit_mock::API_KEY,
                livekit_mock::API_SECRET,
            )),
        ),
        ("user access token", Some(owner.token.clone())),
        ("garbage", Some("not-a-jwt".to_string())),
    ];
    for (name, auth) in cases {
        let (s, v) = app.webhook(&body, auth.as_deref()).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED"),
            "{name}"
        );
    }
    assert_eq!(
        app.scalar("SELECT count(*) FROM rtc_webhook_events").await,
        0
    );
    assert_eq!(session_state(&app, p.session_id).await, "PENDING");
    // A correctly signed but expired callback (beyond the 60 s clock-skew leeway) is
    // rejected too; replays inside the leeway are absorbed by dedupe (S2).
    use base64::Engine;
    use sha2::Digest;
    let sha =
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(body.as_bytes()));
    let now = chrono::Utc::now().timestamp();
    let expired = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &json!({ "iss": livekit_mock::API_KEY, "sub": "", "nbf": now - 7200, "exp": now - 3600,
                 "sha256": sha, "name": "", "video": {}, "sip": {}, "metadata": "", "attributes": {} }),
        &jsonwebtoken::EncodingKey::from_secret(livekit_mock::API_SECRET.as_bytes()),
    )
    .unwrap();
    let (s, _) = app.webhook(&body, Some(&expired)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.scalar("SELECT count(*) FROM rtc_webhook_events").await,
        0
    );
}

/// S2. A replayed (validly signed) webhook is deduplicated.
#[tokio::test]
async fn s02_replayed_webhook_deduplicated() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s2", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    lk(&app).connect(&p.credential).unwrap();
    let body = livekit_mock::webhook_body(
        "participant_joined",
        "EV_s2",
        &room(cid),
        None,
        Some(&p.identity),
    );
    let outcomes: Vec<_> =
        futures_util::future::join_all((0..5).map(|_| app.signed_webhook(&body))).await;
    let applied = outcomes
        .iter()
        .filter(|(_, v)| v["outcome"] == "APPLIED")
        .count();
    let dup = outcomes
        .iter()
        .filter(|(_, v)| v["outcome"] == "DUPLICATE")
        .count();
    assert_eq!((applied, dup), (1, 4));
    assert_eq!(events(&app, cid, "participant.joined").await, 1);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}'"
        ))
        .await,
        1
    );
}

/// S3. An unknown provider room cannot mutate another conference, and a provider
/// identity is only ever resolved inside the room's own conference.
#[tokio::test]
async fn s03_unknown_room_cannot_mutate_other_conference() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, conf_a) = app
        .live_conference(&owner, json!({ "title": "A", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, conf_b) = app
        .live_conference(&owner, json!({ "title": "B", "accessPolicy": "PUBLIC" }))
        .await;
    let a = join_guest(&app, conf_a, "a").await;
    // Unknown room carrying A's identity.
    let body = livekit_mock::webhook_body(
        "participant_joined",
        "EV_s3a",
        &format!("cf-{}", Uuid::new_v4().simple()),
        None,
        Some(&a.identity),
    );
    let (s, v) = app.signed_webhook(&body).await;
    assert_eq!(
        (s, v["outcome"].as_str()),
        (StatusCode::OK, Some("UNKNOWN_ROOM"))
    );
    // Room B carrying A's identity: not resolvable in B; evicted from B only.
    let (_, v) = provider_event(&app, "participant_joined", conf_b, Some(&a.identity)).await;
    assert_eq!(v["outcome"], "UNKNOWN_IDENTITY_EVICTED");
    assert_eq!(
        session_state(&app, a.session_id).await,
        "PENDING",
        "conference A untouched"
    );
    let (_, v) = provider_event(&app, "room_finished", conf_b, None).await;
    assert_eq!(v["outcome"], "APPLIED");
    assert_eq!(conference_status(&app, conf_a).await, "LIVE");
    assert_eq!(
        conference_status(&app, conf_b).await,
        "LIVE",
        "room loss never ends a LIVE conference"
    );
}

/// S4. Provider identities cannot impersonate CF Meet users or create participants.
#[tokio::test]
async fn s04_provider_identity_cannot_impersonate() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s4", "accessPolicy": "PUBLIC" }))
        .await;
    let before = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await;
    for identity in [
        owner.id.to_string(),
        format!("user:{}", owner.id),
        "OWNER".to_string(),
    ] {
        lk(&app).insert_participant(
            &room(cid),
            &identity,
            proto::ParticipantPermission {
                can_publish: true,
                ..Default::default()
            },
        );
        let (_, v) = provider_event(&app, "participant_joined", cid, Some(&identity)).await;
        assert_eq!(v["outcome"], "UNKNOWN_IDENTITY_EVICTED", "{identity}");
        assert!(
            lk(&app).participant(&room(cid), &identity).is_none(),
            "{identity} evicted"
        );
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await,
        before
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}'"
        ))
        .await,
        0
    );
    assert_eq!(events(&app, cid, "participant.joined").await, 0);
}

/// S5/S6. VIEWER credentials carry no publish grant; a modified client cannot publish,
/// and a client-edited credential is rejected by the SFU.
#[tokio::test]
async fn s05_s06_viewer_cannot_publish() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "s5", "accessPolicy": "PUBLIC", "defaultRole": "VIEWER" }),
        )
        .await;
    let v = join_guest(&app, cid, "viewer").await;
    let claims = TokenVerifier::with_api_key(livekit_mock::API_KEY, livekit_mock::API_SECRET)
        .verify(&v.credential)
        .unwrap();
    assert!(!claims.video.can_publish && claims.video.can_publish_sources.is_empty());
    assert!(!claims.video.room_admin && !claims.video.room_record && !claims.video.room_create);
    connect(&app, cid, &v).await;
    assert!(lk(&app)
        .publish(&room(cid), &v.identity, proto::TrackSource::Microphone)
        .is_err());
    // The client cannot mint itself a better credential (not signed with the secret).
    let forged = AccessToken::with_api_key(
        livekit_mock::API_KEY,
        "client-guess-client-guess-client-gues",
    )
    .with_identity(&v.identity)
    .with_grants(VideoGrants {
        room_join: true,
        room: room(cid),
        can_publish: true,
        ..Default::default()
    })
    .to_jwt()
    .unwrap();
    assert!(lk(&app).connect(&forged).is_err());
    // Joining again does not let a viewer request grants: request bodies cannot carry them.
    let (s, body) = app
        .join(
            cid,
            v.auth(),
            json!({ "permissions": { "audioPublish": true } }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
}

/// S7. Role changes change provider grants (both directions), after commit.
#[tokio::test]
async fn s07_role_changes_change_grants() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s7", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    connect(&app, cid, &p).await;
    for (role, can_publish) in [("VIEWER", false), ("PRESENTER", true), ("VIEWER", false)] {
        let (s, _) = change_role(&app, cid, &owner, p.participant_id, role).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            lk(&app)
                .participant(&room(cid), &p.identity)
                .unwrap()
                .permission
                .can_publish,
            can_publish,
            "{role}"
        );
    }
}

/// S8. A removed participant is actually disconnected at the provider.
#[tokio::test]
async fn s08_removed_participant_disconnects() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s8", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    connect(&app, cid, &p).await;
    // Second device too.
    let (_, d2) = app.join(cid, p.auth(), json!({})).await;
    let cred2 = d2["rtc"]["credential"].as_str().unwrap().to_string();
    let id2 = lk(&app).connect(&cred2).unwrap();
    app.post(
        &format!(
            "/api/v2/conferences/{cid}/participants/{}/remove",
            p.participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    assert!(lk(&app).participant(&room(cid), &p.identity).is_none());
    assert!(
        lk(&app).participant(&room(cid), &id2).is_none(),
        "every device disconnected"
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{}' AND state IN ('PENDING','CONNECTED')", p.participant_id)).await, 0);
}

/// S9. A banned participant cannot obtain a credential (any device, any identity path).
#[tokio::test]
async fn s09_banned_cannot_get_credential() {
    let app = app!();
    let owner = app.user("owner").await;
    let troll = app.user("troll").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "s9", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    let p = join_user(&app, cid, &troll).await;
    app.post(
        &format!(
            "/api/v2/conferences/{cid}/participants/{}/ban",
            p.participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    for _ in 0..3 {
        let (s, v) = app
            .join(
                cid,
                Auth::Bearer(&troll.token),
                json!({ "displayName": "new name" }),
            )
            .await;
        assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PARTICIPANT_BANNED"));
        assert!(v.get("rtc").map(|r| r.is_null()).unwrap_or(true));
    }
    let (_, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/realtime/ticket"),
            Auth::Bearer(&troll.token),
            json!({}),
        )
        .await;
    assert_eq!(
        code(&v),
        "PERMISSION_DENIED",
        "banned participants cannot subscribe either"
    );
}

/// S10/S11. ENDING and ENDED conferences never issue credentials.
#[tokio::test]
async fn s10_s11_ending_and_ended_issue_no_credentials() {
    use std::sync::atomic::Ordering;
    let app = app!();
    let owner = app.user("owner").await;
    let (_, ending) = app
        .live_conference(
            &owner,
            json!({ "title": "ending", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let member = join_guest(&app, ending, "member").await;
    lk(&app).faults.fail_delete.store(true, Ordering::SeqCst);
    app.post(
        &format!("/api/v2/conferences/{ending}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    assert_eq!(conference_status(&app, ending).await, "ENDING");
    for auth in [Auth::None, member.auth()] {
        let (s, v) = app.join(ending, auth, json!({ "displayName": "x" })).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    }
    lk(&app).faults.fail_delete.store(false, Ordering::SeqCst);
    let (_, ended) = app
        .live_conference(
            &owner,
            json!({ "title": "ended", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let m2 = join_guest(&app, ended, "m2").await;
    app.post(
        &format!("/api/v2/conferences/{ended}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    assert_eq!(conference_status(&app, ended).await, "ENDED");
    for auth in [Auth::None, m2.auth()] {
        let (s, v) = app
            .join(
                ended,
                auth,
                json!({ "displayName": "x", "previousSessionId": m2.session_id }),
            )
            .await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    }
    // An old credential for the ended room has nowhere to connect.
    assert!(lk(&app).connect(&m2.credential).is_err());
}

/// S12. Host A cannot provider-control a participant of conference B (fails before
/// any provider call; see also E15).
#[tokio::test]
async fn s12_cross_conference_provider_control() {
    let app = app!();
    let host_a = app.user("a").await;
    let host_b = app.user("b").await;
    let (_, conf_a) = app
        .live_conference(&host_a, json!({ "title": "A", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, conf_b) = app
        .live_conference(&host_b, json!({ "title": "B", "accessPolicy": "PUBLIC" }))
        .await;
    let victim = join_guest(&app, conf_b, "victim").await;
    connect(&app, conf_b, &victim).await;
    let admin_calls = || {
        [
            "RemoveParticipant",
            "UpdateParticipant",
            "MutePublishedTrack",
            "GetParticipant",
        ]
        .iter()
        .map(|m| lk(&app).call_count(m))
        .sum::<usize>()
    };
    let before = admin_calls();
    let (s, v) = app
        .post(
            &format!(
                "/api/v2/conferences/{conf_a}/participants/{}/mute",
                victim.participant_id
            ),
            Auth::Bearer(&host_a.token),
            json!({ "trackKind": "VIDEO" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "PARTICIPANT_NOT_FOUND")
    );
    let (s, v) = app
        .post(
            &format!(
                "/api/v2/conferences/{conf_b}/participants/{}/mute",
                victim.participant_id
            ),
            Auth::Bearer(&host_a.token),
            json!({ "trackKind": "VIDEO" }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    // A guest of B cannot mute a peer either.
    let peer = join_guest(&app, conf_b, "peer").await;
    let (s, _) = app
        .post(
            &format!(
                "/api/v2/conferences/{conf_b}/participants/{}/mute",
                victim.participant_id
            ),
            peer.auth(),
            json!({ "trackKind": "AUDIO" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(admin_calls(), before, "no provider call");
}

/// S13. Internal provider routes cannot be called by public clients.
#[tokio::test]
async fn s13_internal_routes_not_public() {
    let app = app!();
    let owner = app.user("owner").await;
    for token in [None, Some("guess"), Some(owner.token.as_str())] {
        let mut req = axum::http::Request::builder().uri("/internal/rtc/metrics");
        if let Some(t) = token {
            req = req
                .header("x-internal-token", t)
                .header("authorization", format!("Bearer {t}"));
        }
        use tower::ServiceExt;
        let resp = app
            .router
            .clone()
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "metrics hidden without the internal token"
        );
    }
    let (s, _) = app
        .webhook("{}", Some(&format!("Bearer {}", owner.token)))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = app
        .get("/internal/rtc/livekit/webhook", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    // No public API exposes provider-admin operations (only scoped, authorized business APIs).
    let doc: serde_json::Value = serde_json::from_str(&backend::swagger::openapi_json()).unwrap();
    for path in doc["paths"].as_object().unwrap().keys() {
        assert!(
            !path.starts_with("/internal"),
            "internal route {path} must not be published"
        );
        assert!(!path.to_lowercase().contains("livekit"), "{path}");
    }
    // Webhooks are disabled entirely without provider control.
    let off = TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        ..TestOptions {
            flags: backend::config::FeatureFlags::all_v2_enabled(),
            ..TestOptions::default()
        }
    })
    .await
    .unwrap();
    let body = livekit_mock::webhook_body("room_started", "EV_off", "cf-x", None, None);
    let (s, _) = off.signed_webhook(&body).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// S14. No provider secret, admin token or private provider id leaks to clients.
#[tokio::test]
async fn s14_no_provider_secret_leak() {
    use std::sync::atomic::Ordering;
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s14", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    connect(&app, cid, &p).await;
    app.post(
        &format!(
            "/api/v2/conferences/{cid}/participants/{}/remove",
            p.participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let mut bodies = vec![p.body.to_string()];
    bodies.push(
        app.get(
            &format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&owner.token),
        )
        .await
        .1
        .to_string(),
    );
    bodies.push(
        app.get(
            &format!("/api/v2/conferences/{cid}/events?after=0"),
            Auth::Bearer(&owner.token),
        )
        .await
        .1
        .to_string(),
    );
    bodies.push(
        app.get(
            &format!("/api/v2/conferences/{cid}/participants"),
            Auth::Bearer(&owner.token),
        )
        .await
        .1
        .to_string(),
    );
    // Provider outage error responses.
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    bodies.push(
        app.join(cid, Auth::None, json!({ "displayName": "x" }))
            .await
            .1
            .to_string(),
    );
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
    let room_sid = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND rtc_room_id LIKE 'RM_%'"
        ))
        .await;
    assert_eq!(room_sid, 1);
    let room_id: String = {
        use sea_orm::ConnectionTrait;
        app.db
            .conn
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT rtc_room_id FROM conference_sessions WHERE id='{cid}'"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap()
    };
    for b in &bodies {
        for secret in [
            livekit_mock::API_SECRET,
            livekit_mock::API_KEY,
            room_id.as_str(),
            "rtc.room_terminate_requested",
            "rtc.participant_remove_requested",
            "mediaIdentity",
            "rtcRoomName",
            "twirp",
            "injected outage",
        ] {
            assert!(!b.contains(secret), "leaked {secret:?} in {b}");
        }
    }
    // The participant credential itself is short-lived and carries no admin grants.
    let claims = TokenVerifier::with_api_key(livekit_mock::API_KEY, livekit_mock::API_SECRET)
        .verify(&p.credential)
        .unwrap();
    assert!(!claims.video.room_admin && !claims.video.room_list && !claims.video.room_create);
}
