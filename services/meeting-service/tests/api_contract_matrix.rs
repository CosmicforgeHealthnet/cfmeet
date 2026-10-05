//! Part J — `/api/v2` consumer contract certification against the real router.
//!
//! - every documented operation is routed and answers with the standard error contract;
//! - API matrix (authentication, authorization, conference/space scope, invalid state,
//!   malformed input, unknown fields, stable error, requestId) per domain area;
//! - join / waiting / leave / end contracts (J-E2E-01..05) with runtime JSON validated
//!   against the generated OpenAPI schemas (J-E2E-08, J-E2E-09).

mod common;

use axum::http::{Method, StatusCode};
use common::{artifacts as art, collab::*, contract::*, rtc::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    ($ctor:expr) => {
        match $ctor.await {
            Some(a) => a,
            None => return,
        }
    };
}

fn fill(path: &str) -> String {
    path.split('/')
        .map(|seg| match seg {
            "{token}" => "inv_not_a_real_token".to_string(),
            "{identifier}" => "abc-defg-hij".to_string(),
            "{consentType}" => "RECORDING".to_string(),
            s if s.starts_with('{') => Uuid::new_v4().to_string(),
            s => s.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Every documented v2 operation exists in the router (never `ENDPOINT_NOT_FOUND` /
/// `METHOD_NOT_ALLOWED`), and every unauthenticated / unknown-resource failure is the
/// standard error contract with a server-generated requestId.
#[tokio::test]
async fn j_api_01_every_documented_operation_is_routed_with_standard_errors() {
    let app = app!(TestApp::artifacts());
    let doc = openapi();
    let mut probed = 0;
    for (path, item) in doc["paths"].as_object().unwrap() {
        if !path.starts_with("/api/v2") {
            continue;
        }
        for method in item.as_object().unwrap().keys() {
            let m: Method = method.to_uppercase().parse().unwrap();
            let uri = fill(path);
            let body = (m != Method::GET && m != Method::DELETE).then_some("{}");
            let (s, h, v) = raw(&app, m.clone(), &uri, &[], body).await;
            probed += 1;
            assert_ne!(code(&v), "ENDPOINT_NOT_FOUND", "{method} {path} not routed");
            assert_ne!(code(&v), "METHOD_NOT_ALLOWED", "{method} {path} not routed");
            assert_ne!(s, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
            if s.is_client_error() || s.is_server_error() {
                assert_error_contract(&doc, s, &h, &v);
                assert!(!s.is_server_error(), "{method} {path} → {s} {v}");
            }
            assert_no_secrets(&app, &format!("{method} {path}"), &v);
        }
    }
    assert_eq!(probed, 136, "all v2 operations probed");
}

/// Unknown v2 paths and unsupported methods keep the standard envelope.
#[tokio::test]
async fn j_api_02_unknown_routes_and_methods_use_the_error_contract() {
    let app = app!(TestApp::new());
    let doc = openapi();
    let (s, h, v) = raw(&app, Method::GET, "/api/v2/no-such-resource", &[], None).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "ENDPOINT_NOT_FOUND"));
    assert_error_contract(&doc, s, &h, &v);
    let (s, h, v) = raw(&app, Method::PUT, "/api/v2/meeting-spaces", &[], Some("{}")).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::METHOD_NOT_ALLOWED, "METHOD_NOT_ALLOWED")
    );
    assert_error_contract(&doc, s, &h, &v);
    let cid = Uuid::new_v4();
    let (s, h, v) = raw(
        &app,
        Method::DELETE,
        &format!("/api/v2/conferences/{cid}/join"),
        &[],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    assert_error_contract(&doc, s, &h, &v);
    // Malformed query strings are envelope errors too.
    let (s, h, v) = raw(
        &app,
        Method::GET,
        &format!("/api/v2/conferences/{cid}/artifacts?artifactType=NOPE"),
        &[("authorization", "Bearer x")],
        None,
    )
    .await;
    assert_error_contract(&doc, s, &h, &v);
}

/// §26: the request ID is server-generated; a client-supplied `x-request-id` is never
/// adopted as authority (kept only as `x-client-request-id`); the same server ID reaches
/// the audit trail and the outbox (realtime correlationId).
#[tokio::test]
async fn j_api_03_request_id_is_server_authority_and_correlated() {
    let app = app!(TestApp::new());
    let doc = openapi();
    let owner = app.user("owner").await;
    let bearer = format!("Bearer {}", owner.token);
    let (s, h, v) = raw(
        &app,
        Method::POST,
        "/api/v2/meeting-spaces",
        &[
            ("authorization", &bearer),
            ("x-request-id", "client-chosen-id-123"),
        ],
        Some(r#"{"title":"rid"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let rid = h["x-request-id"].to_str().unwrap().to_string();
    assert!(rid.starts_with("req_") && rid != "client-chosen-id-123");
    assert_eq!(h["x-client-request-id"], "client-chosen-id-123");
    let sid = uuid(&v["id"]);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_audit_events WHERE meeting_space_id='{sid}' AND request_id='{rid}'"
        ))
        .await,
        1,
        "audit row carries the server request id"
    );
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM conference_audit_events WHERE request_id='client-chosen-id-123'"
        )
        .await,
        0,
        "client id never becomes authority"
    );
    // A malformed client id is dropped entirely; errors still carry a server id.
    let (s, h, v) = raw(
        &app,
        Method::GET,
        &format!("/api/v2/meeting-spaces/{}", Uuid::new_v4()),
        &[
            ("authorization", &bearer),
            ("x-request-id", "bad id\twith spaces"),
        ],
        None,
    )
    .await;
    assert_error_contract(&doc, s, &h, &v);
    assert!(h.get("x-client-request-id").is_none());
    // Every public error carries a distinct request ID.
    let (_, h2, v2) = raw(
        &app,
        Method::GET,
        &format!("/api/v2/meeting-spaces/{}", Uuid::new_v4()),
        &[("authorization", &bearer)],
        None,
    )
    .await;
    assert_ne!(v["error"]["requestId"], v2["error"]["requestId"]);
    assert_ne!(h["x-request-id"], h2["x-request-id"]);
}

/// Meeting space, membership and invitation contract matrix (§7–§10).
#[tokio::test]
async fn j_api_04_space_membership_invitation_matrix() {
    let app = app!(TestApp::new());
    let doc = openapi();
    let owner = app.user("owner").await;
    let stranger = app.user("stranger").await;
    let member = app.user("member").await;
    let other_owner = app.user("other").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "J", "accessPolicy": "INVITE_ONLY" }),
        )
        .await;
    assert_conforms(&doc, "MeetingSpaceResponse", &space);
    assert_no_secrets(&app, "space", &space);
    for f in [
        "passcodeHash",
        "legacyMeetingId",
        "rtcRoomName",
        "profilePolicy",
    ] {
        assert!(space.get(f).is_none(), "space exposes {f}: {space}");
    }
    let sid = uuid(&space["id"]);
    let other = app
        .create_space(&other_owner, json!({ "title": "other" }))
        .await;
    let other_sid = uuid(&other["id"]);
    app.add_member(sid, &owner, &member, backend::domain::Role::Participant)
        .await;
    let (_, members) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_conforms(&doc, "MemberListResponse", &members);
    let member_id = members["items"]
        .as_array()
        .or_else(|| members["members"].as_array())
        .unwrap()
        .iter()
        .find(|m| m["userId"] == json!(member.id))
        .map(|m| uuid(&m["id"]))
        .expect("member listed");
    let (_, conf) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let cid = uuid(&conf["id"]);

    // (method, uri, valid body or None) — owner-authorized space operations.
    let ops: Vec<(Method, String, Option<Value>)> = vec![
        (Method::GET, format!("/api/v2/meeting-spaces/{sid}"), None),
        (
            Method::PATCH,
            format!("/api/v2/meeting-spaces/{sid}"),
            Some(json!({ "version": 1, "title": "J2" })),
        ),
        (
            Method::GET,
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            None,
        ),
        (
            Method::GET,
            format!("/api/v2/meeting-spaces/{sid}/members"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v2/meeting-spaces/{sid}/members"),
            Some(json!({ "userId": stranger.id, "role": "PARTICIPANT" })),
        ),
        (
            Method::PATCH,
            format!("/api/v2/meeting-spaces/{sid}/members/{member_id}"),
            Some(json!({ "role": "PRESENTER", "version": 1 })),
        ),
        (
            Method::POST,
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Some(json!({ "role": "PARTICIPANT" })),
        ),
        (
            Method::POST,
            format!("/api/v2/meeting-spaces/{sid}/owner-transfer"),
            Some(json!({ "newOwnerUserId": member.id, "version": 1 })),
        ),
        (
            Method::POST,
            format!("/api/v2/meeting-spaces/{sid}/archive"),
            None,
        ),
    ];
    // `GET /meeting-spaces/{id}` answers callers without authority with the SUMMARY
    // view only (A–D contract: an id/link identifies, it never authorizes).
    let summary_only = |v: &Value| {
        v["view"] == "SUMMARY"
            && v.get("members").is_none()
            && v.get("ownerUserId").is_none()
            && v.get("accessConfig").is_none()
    };
    for (m, uri, body) in &ops {
        let is_space_read = *m == Method::GET && uri.ends_with(&sid.to_string());
        // Authentication.
        let (s, v) = app.call(m.clone(), uri, Auth::None, body.clone()).await;
        if is_space_read && s == StatusCode::OK {
            assert!(summary_only(&v), "anonymous read is SUMMARY only: {v}");
        } else {
            assert!(
                matches!(
                    s,
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
                ),
                "unauth {m} {uri}: {s} {v}"
            );
            assert!(v["error"]["requestId"].is_string(), "{v}");
        }
        // Authorization + safe existence disclosure.
        let (s, v) = app
            .call(m.clone(), uri, Auth::Bearer(&stranger.token), body.clone())
            .await;
        if is_space_read && s == StatusCode::OK {
            assert!(summary_only(&v), "stranger read is SUMMARY only: {v}");
        } else {
            assert!(
                matches!(s, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
                "stranger {m} {uri}: {s} {v}"
            );
        }
        assert_no_secrets(&app, uri, &v);
        // Unknown fields and malformed JSON are rejected before any mutation.
        if let Some(b) = body {
            let mut extra = b.clone();
            extra["ownerUserId"] = json!(stranger.id);
            let (s, v) = app
                .call(m.clone(), uri, Auth::Bearer(&owner.token), Some(extra))
                .await;
            assert_eq!(
                (s, code(&v)),
                (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
                "unknown field {m} {uri}: {v}"
            );
        }
    }
    let (s, v) = app
        .post_raw(
            &format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&owner.token),
            "{nope",
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));

    // PATCH is version-protected.
    let (s, v) = app
        .patch(
            format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
            json!({ "version": 1, "title": "first" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .patch(
            format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
            json!({ "version": 1, "title": "stale" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "MEETING_VERSION_CONFLICT")
    );

    // Member scope: a member id of space A under space B is not found and untouched.
    let (s, v) = app
        .patch(
            format!("/api/v2/meeting-spaces/{other_sid}/members/{member_id}"),
            Auth::Bearer(&other_owner.token),
            json!({ "role": "HOST", "version": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "foreign member: {v}");
    let (s, _) = app
        .delete(
            format!("/api/v2/meeting-spaces/{other_sid}/members/{member_id}"),
            Auth::Bearer(&other_owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Invitations: token returned once, never its hash; resolve exposes no member data
    // and implies no authority; foreign revoke is not found; revoked tokens die.
    let (s, inv) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            json!({ "role": "PARTICIPANT", "conferenceId": cid }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    assert_conforms(&doc, "CreateInvitationResponse", &inv);
    let token = inv["token"].as_str().unwrap().to_string();
    let inv_id = uuid(&inv["invitation"]["id"]);
    assert!(!inv.to_string().contains("tokenHash"));
    let (s, resolved) = app
        .post(
            format!("/api/v2/invitations/{token}/resolve"),
            Auth::None,
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{resolved}");
    assert_conforms(&doc, "ResolveInvitationResponse", &resolved);
    for leak in [
        owner.email.as_str(),
        member.email.as_str(),
        "tokenHash",
        "usageCount",
        "createdBy",
    ] {
        assert!(
            !resolved.to_string().contains(leak),
            "resolve leaks {leak}: {resolved}"
        );
    }
    // A foreign invitation id is indistinguishable from a non-existent one.
    let (s_foreign, v_foreign) = app
        .post(
            format!("/api/v2/meeting-spaces/{other_sid}/invitations/{inv_id}/revoke"),
            Auth::Bearer(&other_owner.token),
            json!({}),
        )
        .await;
    let (s_missing, v_missing) = app
        .post(
            format!(
                "/api/v2/meeting-spaces/{other_sid}/invitations/{}/revoke",
                Uuid::new_v4()
            ),
            Auth::Bearer(&other_owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s_foreign, code(&v_foreign), &v_foreign["error"]["message"]),
        (s_missing, code(&v_missing), &v_missing["error"]["message"]),
        "foreign invitation revoke discloses nothing"
    );
    assert!(s_foreign.is_client_error());
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_invitations WHERE id='{inv_id}' AND status='ACTIVE'"
        ))
        .await,
        1,
        "foreign revoke did not mutate"
    );
    let (s, _) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations/{inv_id}/revoke"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app
        .post(
            format!("/api/v2/invitations/{token}/resolve"),
            Auth::None,
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::GONE, "revoked invitation: {v}");

    // Owner transfer needs elevated authority and is audited; archive keeps history.
    let (s, v) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/owner-transfer"),
            Auth::Bearer(&member.token),
            json!({ "newOwnerUserId": member.id, "version": 2 }),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "member cannot take ownership: {v}"
    );
    let (s, v) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, t) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/owner-transfer"),
            Auth::Bearer(&owner.token),
            json!({ "newOwnerUserId": member.id, "version": v["version"] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_audit_events WHERE meeting_space_id='{sid}' AND action LIKE '%OWNER%'"
        ))
        .await
            >= 1,
        "owner transfer audited"
    );
    let (s, a) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/archive"),
            Auth::Bearer(&member.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{a}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await,
        1,
        "archive never deletes conference history"
    );
}

/// Conference lifecycle, waiting room and moderation matrix (§11, §16–§19): every
/// moderation target is resolved under the route conference before any provider call.
#[tokio::test]
async fn j_api_05_conference_waiting_room_moderation_matrix() {
    let app = app!(TestApp::livekit());
    let doc = openapi();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "A", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY" }),
        )
        .await;
    let other_owner = app.user("other").await;
    let (_, other_cid) = app
        .live_conference(
            &other_owner,
            json!({ "title": "B", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let foreign = join_guest(&app, other_cid, "victim").await;
    let (s, waiting) = app
        .join(cid, Auth::None, json!({ "displayName": "w" }))
        .await;
    assert_eq!(s, StatusCode::OK);
    let waiting_pid = uuid(&waiting["participant"]["id"]);
    let stranger = app.user("stranger").await;

    let moderation: Vec<(Method, String, Value)> = vec![
        (
            Method::POST,
            "participants/{p}/return-to-waiting".into(),
            json!({}),
        ),
        (
            Method::PATCH,
            "participants/{p}/role".into(),
            json!({ "role": "HOST" }),
        ),
        (Method::POST, "participants/{p}/remove".into(), json!({})),
        (Method::POST, "participants/{p}/ban".into(), json!({})),
        (
            Method::POST,
            "participants/{p}/mute".into(),
            json!({ "trackKind": "AUDIO" }),
        ),
        (
            Method::POST,
            "waiting-room/admit".into(),
            json!({ "participantIds": ["{p}"] }),
        ),
        (
            Method::POST,
            "waiting-room/deny".into(),
            json!({ "participantIds": ["{p}"] }),
        ),
    ];
    let calls_before = lk(&app).calls.lock().unwrap().len();
    for (m, suffix, body) in &moderation {
        let target = |p: Uuid| {
            (
                format!(
                    "/api/v2/conferences/{cid}/{}",
                    suffix.replace("{p}", &p.to_string())
                ),
                serde_json::from_str::<Value>(&body.to_string().replace("{p}", &p.to_string()))
                    .unwrap(),
            )
        };
        let (uri, b) = target(foreign.participant_id);
        // Unauthenticated: no moderation authority (stable 401/403, never a mutation).
        let (s, v) = app.call(m.clone(), &uri, Auth::None, Some(b.clone())).await;
        assert!(
            matches!(
                (s, code(&v)),
                (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
                    | (StatusCode::FORBIDDEN, "PERMISSION_DENIED")
            ),
            "{uri}: {s} {v}"
        );
        assert!(v["error"]["requestId"].is_string());
        // Unauthorized caller.
        let (s, v) = app
            .call(
                m.clone(),
                &uri,
                Auth::Bearer(&stranger.token),
                Some(b.clone()),
            )
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "stranger {uri}: {v}");
        // Foreign-conference participant under the host's own conference.
        let (s, v) = app
            .call(m.clone(), &uri, Auth::Bearer(&owner.token), Some(b.clone()))
            .await;
        // Single and bulk targets alike: any id outside the route conference rejects the
        // request before any mutation or provider call.
        assert_eq!(
            (s, code(&v)),
            (StatusCode::NOT_FOUND, "PARTICIPANT_NOT_FOUND"),
            "foreign {uri}: {v}"
        );
        let s_h = raw(&app, m.clone(), &uri, &[], Some("{\"x\":")).await;
        assert_error_contract(&doc, s_h.0, &s_h.1, &s_h.2);
        // Unknown field on the real target (operations with a request DTO).
        if suffix.ends_with("return-to-waiting") {
            continue;
        }
        let (uri, mut b) = target(waiting_pid);
        b["userId"] = json!(owner.id);
        let (s, v) = app
            .call(m.clone(), &uri, Auth::Bearer(&owner.token), Some(b))
            .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "unknown field {uri}: {v}"
        );
    }
    let victim = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE id='{}' AND admission_state='ADMITTED' AND role='PARTICIPANT'",
            foreign.participant_id
        ))
        .await;
    assert_eq!(victim, 1, "foreign participant untouched");
    let provider_calls: Vec<_> = lk(&app).calls.lock().unwrap()[calls_before..]
        .iter()
        .map(|(m, _, _)| m.clone())
        .collect();
    assert!(
        !provider_calls
            .iter()
            .any(|m| m.contains("Remove") || m.contains("Update") || m.contains("Mute")),
        "no provider mutation for rejected moderation: {provider_calls:?}"
    );

    // Waiting room read and admission are conference-scoped and audited.
    let (s, wr) = app
        .get(
            format!("/api/v2/conferences/{cid}/waiting-room"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_conforms(&doc, "WaitingRoomResponse", &wr);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&owner.token),
            json!({ "participantIds": [waiting_pid] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_conforms(&doc, "BulkAdmissionResponse", &v);

    // Invalid state: lock/unlock/start on an ENDED conference are stable conflicts.
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_conforms(&doc, "ConferenceActionResponse", &v);
    for action in ["lock", "start"] {
        let (s, v) = app
            .post(
                format!("/api/v2/conferences/{cid}/{action}"),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::CONFLICT, "{action} after end: {v}");
        assert!(v["error"]["requestId"].is_string());
    }
}

/// Collaboration, recording, artefact and attendance contracts (§20–§21).
#[tokio::test]
async fn j_api_06_collaboration_recording_artifact_attendance_matrix() {
    let app = app!(TestApp::artifacts());
    let doc = openapi();
    let (owner, _sid, cid, g) = art::meeting(&app, 2).await;
    let (other_owner, _, other_cid, _) = art::meeting(&app, 0).await;
    let (rid, _) = art::recording_ready(&app, other_cid, &other_owner).await;

    // Representative collaboration routes are published and enforce auth/role.
    let host_only: Vec<(String, Value)> = vec![
        ("breakouts".into(), json!({ "count": 1 })),
        ("polls".into(), simple_poll("AFTER_VOTE")),
        ("recordings".into(), json!({})),
        (
            "artifacts/reports".into(),
            json!({ "artifactType": "ATTENDANCE_REPORT" }),
        ),
    ];
    for (suffix, body) in &host_only {
        let uri = format!("/api/v2/conferences/{cid}/{suffix}");
        let (s, v) = app.post(&uri, Auth::None, body.clone()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri}: {v}");
        let (s, v) = app.post(&uri, g[0].auth(), body.clone()).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "participant {uri}: {v}");
        let mut extra = body.clone();
        extra["storageKey"] = json!("x");
        let (s, v) = app.post(&uri, Auth::Bearer(&owner.token), extra).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{uri}: {v}"
        );
    }
    let (s, q) = app
        .post(
            format!("/api/v2/conferences/{cid}/questions"),
            g[1].auth(),
            json!({ "body": "Q?" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{q}");

    // Artefacts of conference B are invisible under conference A, even to A's host.
    for (m, suffix) in [
        (Method::GET, format!("artifacts/{rid}")),
        (Method::POST, format!("artifacts/{rid}/access")),
        (Method::DELETE, format!("artifacts/{rid}")),
        (Method::GET, format!("recordings/{rid}")),
    ] {
        let uri = format!("/api/v2/conferences/{cid}/{suffix}");
        let (s, v) = app
            .call(m.clone(), &uri, Auth::Bearer(&owner.token), Some(json!({})))
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "foreign artefact {m} {uri}: {v}");
        assert_no_secrets(&app, &uri, &v);
    }
    let (s, list) = app
        .get(
            format!("/api/v2/conferences/{other_cid}/artifacts"),
            Auth::Bearer(&other_owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_conforms(&doc, "ArtifactListResponse", &list);
    let (s, access) = app
        .post(
            format!("/api/v2/conferences/{other_cid}/artifacts/{rid}/access"),
            Auth::Bearer(&other_owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{access}");
    assert_conforms(&doc, "ArtifactAccess", &access);

    // Attendance: thin read over the same aggregation as the attendance export.
    let uri = format!("/api/v2/conferences/{cid}/attendance");
    let (s, v) = app.get(&uri, Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let (s, v) = app.get(&uri, g[0].auth()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "participant attendance: {v}");
    let (s, v) = app.get(&uri, Auth::Bearer(&other_owner.token)).await;
    assert!(
        matches!(s, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "foreign host attendance: {s} {v}"
    );
    let (s, att) = app.get(&uri, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{att}");
    assert_conforms(&doc, "AttendanceReport", &att);
    assert_eq!(uuid(&att["conferenceId"]), cid);
    let ids: Vec<Uuid> = att["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| uuid(&p["participantId"]))
        .collect();
    assert!(ids.contains(&g[0].participant_id) && ids.contains(&g[1].participant_id));
    let artifacts_before = app
        .scalar(&format!(
            "SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}'"
        ))
        .await;
    assert_eq!(artifacts_before, 0, "attendance read stores nothing");
    // The export path uses the same computation.
    let (s, report) = app
        .post(
            format!("/api/v2/conferences/{cid}/artifacts/reports"),
            Auth::Bearer(&owner.token),
            json!({ "artifactType": "ATTENDANCE_REPORT" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{report}");
    assert_eq!(report["metadata"]["participantCount"], json!(ids.len()));
}

/// J-E2E-01 + J-E2E-08: an admitted join returns the canonical structure and the
/// runtime JSON matches the published schema exactly (no undocumented field).
#[tokio::test]
async fn j_e2e_01_08_admitted_join_contract() {
    let app = app!(TestApp::livekit());
    let doc = openapi();
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(&owner, json!({ "title": "join", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "Ada" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_conforms(&doc, "JoinConferenceResponse", &v);
    assert_eq!(v["conference"]["id"], json!(cid));
    assert_eq!(v["conference"]["status"], "LIVE");
    assert_eq!(v["conference"]["meetingSpaceId"], json!(sid));
    assert_eq!(v["participant"]["role"], "PARTICIPANT");
    assert_eq!(v["participant"]["admissionState"], "ADMITTED");
    assert!(v["participantSession"]["id"].is_string());
    assert!(v["participantSession"]["expiresAt"].is_string());
    assert_eq!(v["rtc"]["provider"], "LIVEKIT");
    assert!(v["rtc"]["url"].is_string() && v["rtc"]["credential"].is_string());
    assert_eq!(v["rtcStatus"], "ISSUED");
    assert_eq!(v["permissions"]["audioPublish"], true);
    assert_eq!(v["permissions"]["videoPublish"], true);
    assert_eq!(v["permissions"]["subscribe"], true);
    assert_eq!(v["policy"]["recordingActive"], false);
    assert_eq!(v["policy"]["transcriptionActive"], false);
    assert_eq!(
        v["realtime"]["conferencePath"],
        json!(format!("/api/v2/conferences/{cid}"))
    );
    assert_eq!(v["realtime"]["enabled"], true);
    assert!(v["realtime"]["ticketPath"].is_string());
    // Bounded credential lifetime.
    let exp =
        chrono::DateTime::parse_from_rfc3339(v["rtc"]["expiresAt"].as_str().unwrap()).unwrap();
    let ttl = exp.signed_duration_since(chrono::Utc::now());
    assert!(ttl <= chrono::Duration::minutes(11) && ttl > chrono::Duration::zero());
    assert_no_secrets(&app, "join", &v);
    // GET conference is the canonical recovery resource for realtime clients.
    let token = v["participantToken"].as_str().unwrap();
    let (s, c) = app
        .get(
            format!("/api/v2/conferences/{cid}"),
            Auth::Participant(token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_conforms(&doc, "ConferenceResponse", &c);
    assert_eq!(c["status"], "LIVE");
    let (s, me) = app
        .get(
            format!("/api/v2/conferences/{cid}/participants/me"),
            Auth::Participant(token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{me}");
}

/// J-E2E-02: a WAITING participant gets no credential of any kind, and the response
/// carries what it needs to follow admission / denial (SELF_ONLY realtime + REST).
#[tokio::test]
async fn j_e2e_02_waiting_join_has_no_publish_credential() {
    let app = app!(TestApp::collab());
    let doc = openapi();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "w", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    let issued_before = lk(&app).calls.lock().unwrap().len();
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "w" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_conforms(&doc, "JoinConferenceResponse", &v);
    assert_eq!(v["participant"]["admissionState"], "WAITING");
    assert!(v["rtc"].is_null(), "no credential while waiting: {v}");
    assert_eq!(v["rtcStatus"], "NOT_ISSUED_WAITING");
    assert!(v["participantSession"].is_null());
    for p in [
        "audioPublish",
        "videoPublish",
        "screenSharePublish",
        "dataPublish",
    ] {
        assert_eq!(v["permissions"][p], false, "{p} while waiting");
    }
    assert!(v["participantToken"].is_string(), "waiting identity issued");
    assert_eq!(
        v["realtime"]["participantPath"],
        json!(format!("/api/v2/conferences/{cid}/participants/me"))
    );
    // The waiting identity can open its SELF_ONLY realtime subscription.
    let token = v["participantToken"].as_str().unwrap();
    let (s, t) = app
        .post(
            format!("/api/v2/conferences/{cid}/realtime/ticket"),
            Auth::Participant(token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "waiting ticket: {t}");
    // Media credential refresh is refused before admission.
    let (s, r) = app
        .post(
            format!("/api/v2/conferences/{cid}/media-credential"),
            Auth::Participant(token),
            json!({ "participantSessionId": Uuid::new_v4() }),
        )
        .await;
    assert!(s.is_client_error(), "waiting media-credential: {s} {r}");
    assert!(
        !matches!(code(&r), "VALIDATION_ERROR" | "FEATURE_DISABLED"),
        "refused by authority, not by shape: {r}"
    );
    assert!(r.get("credential").is_none() && r.get("rtc").is_none());
    let new_calls: Vec<_> = lk(&app).calls.lock().unwrap()[issued_before..].to_vec();
    assert!(
        new_calls.iter().all(|(m, _, _)| !m.contains("Participant")),
        "no provider participant action: {new_calls:?}"
    );
}

/// J-E2E-03: VIEWER receives subscribe-only media permissions and grants.
#[tokio::test]
async fn j_e2e_03_viewer_receives_subscribe_only_media() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "v", "accessPolicy": "PUBLIC" }))
        .await;
    let g = join_guest(&app, cid, "viewer").await;
    let (s, v) = change_role(&app, cid, &owner, g.participant_id, "VIEWER").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .join(
            cid,
            g.auth(),
            json!({ "displayName": "viewer", "previousSessionId": g.session_id }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "VIEWER");
    for p in ["audioPublish", "videoPublish", "screenSharePublish"] {
        assert_eq!(v["permissions"][p], false, "{p}: {v}");
    }
    assert_eq!(v["permissions"]["subscribe"], true);
    let identity = lk(&app)
        .connect(v["rtc"]["credential"].as_str().unwrap())
        .unwrap();
    let p = lk(&app).participant(&room(cid), &identity).unwrap();
    assert!(
        !p.permission.can_publish,
        "provider grant is subscribe-only"
    );
    assert!(p.permission.can_subscribe);
}

/// J-E2E-04: `policy.recordingActive` / `transcriptionActive` reflect canonical server
/// state at join time.
#[tokio::test]
async fn j_e2e_04_join_policy_reflects_canonical_capture_state() {
    let app = app!(TestApp::artifacts());
    let (owner, _sid, cid, _g) = art::meeting(&app, 1).await;
    let (_, v) = app
        .join(cid, Auth::None, json!({ "displayName": "a" }))
        .await;
    assert_eq!(v["policy"]["recordingActive"], false);
    let (rid, _) = art::recording_active(&app, cid, &owner).await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "b" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["policy"]["recordingActive"], true, "{v}");
    assert_eq!(v["policy"]["transcriptionActive"], false);
    let (rid_eg, eg) = (rid, art::provider_ref(&app, rid).await);
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/recordings/{rid_eg}/stop"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    // STOPPING: the provider has not confirmed the capture ended → still active.
    let (_, v) = app
        .join(cid, Auth::None, json!({ "displayName": "c" }))
        .await;
    assert_eq!(v["policy"]["recordingActive"], true, "{v}");
    let bytes = b"\x00\x00\x00\x18ftypmp42 contract".repeat(32);
    let body = lk(&app).egress_complete(&eg, Some(&app.storage), &bytes, None);
    let (s, v) = art::egress_event(&app, &body).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = app
        .join(cid, Auth::None, json!({ "displayName": "d" }))
        .await;
    assert_eq!(v["policy"]["recordingActive"], false, "{v}");
}

/// J-E2E-05: removed / banned participants and an ENDING conference cannot recover a
/// media grant through join, reconnect or credential refresh.
#[tokio::test]
async fn j_e2e_05_removed_banned_ending_cannot_recover_media() {
    let app = app!(TestApp::collab());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "r", "accessPolicy": "PUBLIC" }))
        .await;
    let removed = join_guest(&app, cid, "removed").await;
    let banned = join_guest(&app, cid, "banned").await;
    let survivor = join_guest(&app, cid, "survivor").await;
    for (j, action) in [(&removed, "remove"), (&banned, "ban")] {
        let (s, v) = app
            .post(
                format!(
                    "/api/v2/conferences/{cid}/participants/{}/{action}",
                    j.participant_id
                ),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{action}: {v}");
    }
    for j in [&removed, &banned] {
        let (s, v) = app
            .join(
                cid,
                j.auth(),
                json!({ "displayName": "again", "previousSessionId": j.session_id }),
            )
            .await;
        assert!(
            s.is_client_error() || v["rtc"].is_null(),
            "rejoin must not yield media: {s} {v}"
        );
        let (s, v) = app
            .post(
                format!("/api/v2/conferences/{cid}/media-credential"),
                j.auth(),
                json!({ "participantSessionId": j.session_id }),
            )
            .await;
        assert!(s.is_client_error(), "refresh after removal: {s} {v}");
        assert!(
            !matches!(code(&v), "VALIDATION_ERROR" | "FEATURE_DISABLED"),
            "{v}"
        );
    }
    let (s, v) = app
        .join(cid, banned.auth(), json!({ "displayName": "again" }))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PARTICIPANT_BANNED"));
    // ENDING: the provider keeps the room (termination unconfirmed) → stays ENDING.
    lk(&app)
        .faults
        .delete_noop
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["conference"]["status"], "ENDING",
        "never ENDED before the provider confirms termination: {v}"
    );
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "late" }))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"),
        "{v}"
    );
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/media-credential"),
            survivor.auth(),
            json!({ "participantSessionId": survivor.session_id }),
        )
        .await;
    assert!(s.is_client_error(), "ENDING refresh: {s} {v}");
    assert!(
        !matches!(code(&v), "VALIDATION_ERROR" | "FEATURE_DISABLED"),
        "{v}"
    );
}

/// Leave closes the current ParticipantSession only; the logical participant (and its
/// attendance history) persists and reconnect opens a new session.
#[tokio::test]
async fn j_leave_closes_session_and_keeps_logical_participant() {
    let app = app!(TestApp::livekit());
    let doc = openapi();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "l", "accessPolicy": "PUBLIC" }))
        .await;
    let g = join_guest(&app, cid, "leaver").await;
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/leave"),
            g.auth(),
            json!({ "participantSessionId": g.session_id }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_conforms(&doc, "LeaveConferenceResponse", &v);
    assert_eq!(v["closedSessions"], 1);
    assert_ne!(session_state(&app, g.session_id).await, "CONNECTED");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE id='{}' AND admission_state='ADMITTED'",
            g.participant_id
        ))
        .await,
        1,
        "logical participant kept"
    );
    let (s, v) = app
        .join(cid, g.auth(), json!({ "displayName": "leaver" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        uuid(&v["participant"]["id"]),
        g.participant_id,
        "same participant"
    );
    assert_ne!(
        uuid(&v["participantSession"]["id"]),
        g.session_id,
        "new session"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{}'",
            g.participant_id
        ))
        .await,
        2
    );
}

/// J-E2E-09: runtime errors from every layer (extractor, auth, domain, provider)
/// match the published error schema and carry the server request ID.
#[tokio::test]
async fn j_e2e_09_runtime_errors_match_the_error_schema() {
    let app = app!(TestApp::livekit());
    let doc = openapi();
    let owner = app.user("owner").await;
    let bearer = format!("Bearer {}", owner.token);
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "e", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    type Case<'a> = (Method, String, Vec<(&'a str, &'a str)>, Option<&'a str>);
    let cases: Vec<Case<'_>> = vec![
        // Path extractor.
        (
            Method::GET,
            "/api/v2/conferences/not-a-uuid".into(),
            vec![],
            None,
        ),
        // JSON syntax / unknown field.
        (
            Method::POST,
            "/api/v2/meeting-spaces".into(),
            vec![("authorization", bearer.as_str())],
            Some("{broken"),
        ),
        (
            Method::POST,
            "/api/v2/meeting-spaces".into(),
            vec![("authorization", bearer.as_str())],
            Some(r#"{"title":"x","ownerUserId":"00000000-0000-0000-0000-000000000000"}"#),
        ),
        // Authentication.
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            vec![],
            Some("{}"),
        ),
        (
            Method::POST,
            "/api/v2/meeting-spaces".into(),
            vec![("authorization", "Bearer forged.jwt.token")],
            Some(r#"{"title":"x"}"#),
        ),
        // Domain.
        (
            Method::GET,
            format!("/api/v2/conferences/{}", Uuid::new_v4()),
            vec![("authorization", bearer.as_str())],
            None,
        ),
        // Feature-gated route (collaboration disabled in this app).
        (
            Method::GET,
            format!("/api/v2/conferences/{cid}/polls"),
            vec![("authorization", bearer.as_str())],
            None,
        ),
    ];
    for (m, uri, headers, body) in cases {
        let (s, h, v) = raw(&app, m.clone(), &uri, &headers, body).await;
        assert_error_contract(&doc, s, &h, &v);
        assert_no_secrets(&app, &uri, &v);
    }
    // Provider failure surfaces as the stable retryable code with no raw provider text.
    lk(&app)
        .faults
        .fail_all
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let guest_space = app.user("g").await;
    let (_, pub_cid) = {
        lk(&app)
            .faults
            .fail_all
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let r = app
            .live_conference(
                &guest_space,
                json!({ "title": "p", "accessPolicy": "PUBLIC" }),
            )
            .await;
        lk(&app)
            .faults
            .fail_all
            .store(true, std::sync::atomic::Ordering::SeqCst);
        r
    };
    let (s, h, v) = raw(
        &app,
        Method::POST,
        &format!("/api/v2/conferences/{pub_cid}/join"),
        &[],
        Some(r#"{"displayName":"p"}"#),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE"),
        "{v}"
    );
    assert_error_contract(&doc, s, &h, &v);
    assert_no_secrets(&app, "provider outage", &v);
}
