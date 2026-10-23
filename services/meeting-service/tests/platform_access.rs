//! Part VI §51 — platform-credential access to the Meeting Service (receiver branch).
//!
//! Requests arrive exactly as the API Gateway forwards them: no `Authorization`, a REAL
//! Ed25519 service token of `cfmeet.api-gateway` verified by the Meeting Service's own
//! trust bundle (granted `meeting-request.forward`), and a signed delegated context
//! naming the credential. A scripted Platform Access answers the live
//! `delegated-authority` re-verification. Proves: the machine actor is the live chain,
//! scopes are enforced, every refusal is the normalized 401 (503 when Platform Access is
//! unreachable), meetings are environment-scoped (other environments and user meetings
//! are 404) and user flows are unchanged.

mod common;

use std::sync::atomic::Ordering;

use axum::http::{Method, StatusCode};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use common::{platform::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! harness {
    () => {
        match Harness::new().await {
            Some(h) => h,
            None => return,
        }
    };
}

const RW: &[&str] = &["meetings.read", "meetings.write"];

#[tokio::test]
async fn a_forwarded_credential_runs_its_environments_meetings() {
    let h = harness!();
    let m = Machine::new(RW);
    h.register(&m);
    let (s, v, correlation) = h
        .forwarded_by(
            &h.gateway,
            Method::POST,
            "/api/v2/instant-meetings",
            Some(json!({ "title": "Support call" })),
            Some(m.delegated()),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let space = uuid(&v["meetingSpace"]["id"]);
    let conference = uuid(&v["conference"]["id"]);
    assert_eq!(v["conference"]["status"], "LIVE");
    assert!(
        v["meetingSpace"]["ownerUserId"].is_null(),
        "no CF Meet owner: {v}"
    );
    // Platform Access re-verified the credential live, continuing the gateway's
    // correlation id.
    assert_eq!(h.pa.calls.load(Ordering::SeqCst), 1);
    assert_eq!(h.pa.correlations.lock().unwrap()[0], correlation);
    // Owned by the LIVE chain's environment, created by its service account.
    let a = &m.actor;
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM meeting_spaces WHERE id = '{space}' AND owner_user_id IS NULL \
                 AND platform_tenant_id = '{}' AND platform_application_id = '{}' \
                 AND platform_environment_id = '{}' AND created_by_service_account_id = '{}'",
                a.tenant_id, a.application_id, a.environment_id, a.service_account_id
            ))
            .await,
        1
    );
    // Audited as the service account, never as a user.
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM conference_audit_events WHERE meeting_space_id = '{space}' \
                 AND actor_type = 'PLATFORM' AND actor_ref = 'service_account:{}'",
                a.service_account_id
            ))
            .await,
        2,
        "space + conference creation"
    );

    // List: exactly its environment's meetings (full view).
    let (s, v) = h
        .machine(&m, Method::GET, "/api/v2/meeting-spaces", None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(uuid(&items[0]["id"]), space);
    assert_eq!(items[0]["viewerRole"], "OWNER", "{v}");

    // Read (also by meeting code), update, conference, participants, attendance.
    let (s, v) = h
        .machine(
            &m,
            Method::GET,
            &format!("/api/v2/meeting-spaces/{space}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let code = v["meetingCode"].as_str().unwrap().to_string();
    let version = v["version"].as_i64().unwrap();
    let (s, v) = h
        .machine(
            &m,
            Method::GET,
            &format!("/api/v2/meeting-spaces/{code}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .machine(
            &m,
            Method::PATCH,
            &format!("/api/v2/meeting-spaces/{space}"),
            Some(json!({ "version": version, "title": "Renamed" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["title"], "Renamed");
    let (s, v) = h
        .machine(
            &m,
            Method::GET,
            &format!("/api/v2/conferences/{conference}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // A guest joins through the public meeting; the machine sees the roster.
    let (s, g) = h
        .app
        .join(
            conference,
            Auth::None,
            json!({ "displayName": "Guest Ada" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{g}");
    let guest = uuid(&g["participant"]["id"]);
    let (s, v) = h
        .machine(
            &m,
            Method::GET,
            &format!("/api/v2/conferences/{conference}/participants"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v.to_string().contains(&guest.to_string()), "{v}");
    let (s, v) = h
        .machine(
            &m,
            Method::GET,
            &format!("/api/v2/conferences/{conference}/waiting-room"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .machine(
            &m,
            Method::GET,
            &format!("/api/v2/conferences/{conference}/attendance"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            &format!("/api/v2/conferences/{conference}/participants/{guest}/remove"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Invitations: create (no CF Meet inviter), list, revoke.
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            &format!("/api/v2/meeting-spaces/{space}/invitations"),
            Some(json!({ "useLimit": 1 })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let invitation = uuid(&v["invitation"]["id"]);
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM meeting_invitations WHERE id = '{invitation}' AND inviter_user_id IS NULL"
            ))
            .await,
        1
    );
    let (s, v) = h
        .machine(
            &m,
            Method::GET,
            &format!("/api/v2/meeting-spaces/{space}/invitations"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            &format!("/api/v2/meeting-spaces/{space}/invitations/{invitation}/revoke"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // End the conference.
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            &format!("/api/v2/conferences/{conference}/end"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        matches!(v["conference"]["status"].as_str(), Some("ENDING" | "ENDED"))
            || matches!(v["status"].as_str(), Some("ENDING" | "ENDED")),
        "{v}"
    );

    // Scheduled meeting: create, schedule a conference, list it, cancel it, archive.
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            "/api/v2/meeting-spaces",
            Some(json!({ "title": "Weekly sync", "accessPolicy": "PUBLIC" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let scheduled = uuid(&v["id"]);
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            &format!("/api/v2/meeting-spaces/{scheduled}/conferences"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let occurrence = uuid(&v["id"]);
    let (s, v) = h
        .machine(
            &m,
            Method::GET,
            &format!("/api/v2/meeting-spaces/{scheduled}/conferences"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v.to_string().contains(&occurrence.to_string()));
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            &format!("/api/v2/conferences/{occurrence}/cancel"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            &format!("/api/v2/meeting-spaces/{scheduled}/archive"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "ARCHIVED");

    // Another credential of the SAME environment sees the same meetings.
    let peer = m.same_environment(&["meetings.read"]);
    h.register(&peer);
    let (s, v) = h
        .machine(&peer, Method::GET, "/api/v2/meeting-spaces", None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["items"].as_array().unwrap().len(), 2);

    // Default destination `none`: nothing is staged for the Developer Platform.
    assert_eq!(
        h.app
            .scalar("SELECT count(*) FROM outbox_events WHERE channel = 'PUBLIC_EVENT'")
            .await,
        0
    );
}

#[tokio::test]
async fn other_environments_and_user_meetings_are_not_found() {
    let h = harness!();
    let a = Machine::new(RW);
    let other_tenant = Machine::new(RW);
    let sibling = a.sibling_environment();
    for m in [&a, &other_tenant, &sibling] {
        h.register(m);
    }
    let (s, v) = h
        .machine(
            &a,
            Method::POST,
            "/api/v2/instant-meetings",
            Some(json!({ "title": "A only" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let space = uuid(&v["meetingSpace"]["id"]);
    let conference = uuid(&v["conference"]["id"]);
    let code = v["meetingSpace"]["meetingCode"]
        .as_str()
        .unwrap()
        .to_string();

    for outsider in [&other_tenant, &sibling] {
        for (method, path, body) in [
            (Method::GET, format!("/api/v2/meeting-spaces/{space}"), None),
            (Method::GET, format!("/api/v2/meeting-spaces/{code}"), None),
            (
                Method::PATCH,
                format!("/api/v2/meeting-spaces/{space}"),
                Some(json!({ "version": 1, "title": "x" })),
            ),
            (
                Method::POST,
                format!("/api/v2/meeting-spaces/{space}/invitations"),
                Some(json!({})),
            ),
            (
                Method::GET,
                format!("/api/v2/meeting-spaces/{space}/conferences"),
                None,
            ),
        ] {
            let (s, v) = h.machine(outsider, method.clone(), &path, body).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {v}");
            assert_eq!(code_of(&v), "MEETING_NOT_FOUND", "{v}");
        }
        for (method, path) in [
            (Method::GET, format!("/api/v2/conferences/{conference}")),
            (
                Method::POST,
                format!("/api/v2/conferences/{conference}/end"),
            ),
            (
                Method::GET,
                format!("/api/v2/conferences/{conference}/participants"),
            ),
        ] {
            let body = (method == Method::POST).then(|| json!({}));
            let (s, v) = h.machine(outsider, method.clone(), &path, body).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {v}");
            assert_eq!(code_of(&v), "CONFERENCE_NOT_FOUND", "{v}");
        }
        let (s, v) = h
            .machine(outsider, Method::GET, "/api/v2/meeting-spaces", None)
            .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["items"], json!([]), "{v}");
    }
    // Unknown ids look the same.
    let (s, _) = h
        .machine(
            &a,
            Method::GET,
            &format!("/api/v2/conferences/{}", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // A CF Meet user's meeting does not exist for any machine caller.
    let user = h.app.user("owner").await;
    let (user_space, user_conference) = h
        .app
        .live_conference(&user, json!({ "title": "Mine", "accessPolicy": "PUBLIC" }))
        .await;
    for (method, path) in [
        (Method::GET, format!("/api/v2/meeting-spaces/{user_space}")),
        (
            Method::GET,
            format!("/api/v2/conferences/{user_conference}"),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{user_conference}/end"),
        ),
    ] {
        let body = (method == Method::POST).then(|| json!({}));
        let (s, v) = h.machine(&a, method.clone(), &path, body).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {v}");
    }
    let (_, v) = h
        .machine(&a, Method::GET, "/api/v2/meeting-spaces", None)
        .await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert!(!v.to_string().contains(&user_space.to_string()));

    // ...and an environment's meeting is not the user's: never listed, summary view only,
    // never manageable.
    let (s, v) = h
        .app
        .get("/api/v2/meeting-spaces", Auth::Bearer(&user.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(!v.to_string().contains(&space.to_string()), "{v}");
    let (s, v) = h
        .app
        .get(
            format!("/api/v2/meeting-spaces/{space}"),
            Auth::Bearer(&user.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["viewerRole"].is_null(), "{v}");
    assert!(v["ownerUserId"].is_null(), "{v}");
    let (s, v) = h
        .app
        .patch(
            format!("/api/v2/meeting-spaces/{space}"),
            Auth::Bearer(&user.token),
            json!({ "version": 1, "title": "hijack" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let (s, v) = h
        .app
        .post(
            format!("/api/v2/conferences/{conference}/end"),
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    // A platform administrator cannot move it to a CF Meet account either.
    let admin = h.app.admin("ops").await;
    let (s, v) = h
        .app
        .post(
            format!("/api/v2/meeting-spaces/{space}/owner-transfer"),
            Auth::Bearer(&admin.token),
            json!({ "newOwnerUserId": user.id, "version": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
}

fn code_of(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn scopes_are_enforced_per_operation() {
    let h = harness!();
    let reader = Machine::new(&["meetings.read"]);
    let writer = reader.same_environment(&["meetings.write"]);
    let none = reader.same_environment(&["calls.read"]);
    for m in [&reader, &writer, &none] {
        h.register(m);
    }
    let (s, v) = h
        .machine(
            &reader,
            Method::POST,
            "/api/v2/meeting-spaces",
            Some(json!({ "title": "x" })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code_of(&v), "PERMISSION_DENIED");
    let (s, v) = h
        .machine(
            &writer,
            Method::POST,
            "/api/v2/meeting-spaces",
            Some(json!({ "title": "x" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let space = uuid(&v["id"]);
    // meetings.write never implies meetings.read.
    let (s, _) = h
        .machine(&writer, Method::GET, "/api/v2/meeting-spaces", None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h
        .machine(
            &reader,
            Method::GET,
            &format!("/api/v2/meeting-spaces/{space}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .machine(&none, Method::GET, "/api/v2/meeting-spaces", None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn every_refusal_is_the_normalized_401_and_an_unreachable_authority_503() {
    let h = harness!();
    let m = Machine::new(RW);
    h.register(&m);
    let user = h.app.user("someone").await;
    let assert_401 = |s: StatusCode, v: &Value, what: &str| {
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{what}: {v}");
        assert_eq!(code_of(v), "AUTHENTICATION_REQUIRED", "{what}: {v}");
        assert_eq!(v["error"]["message"], "Authentication failed", "{what}");
    };
    let path = "/api/v2/meeting-spaces";
    // No credential named in the delegated context / no delegated context at all.
    let a = &m.actor;
    let unnamed = DelegatedPlatformContext::new(
        a.tenant_id,
        a.application_id,
        a.environment_id,
        ApplicationEnvironmentKind::Sandbox,
        a.service_account_id,
    );
    let (s, v, _) = h
        .forwarded_by(&h.gateway, Method::GET, path, None, Some(unnamed), &[])
        .await;
    assert_401(s, &v, "no credential id");
    let (s, v, _) = h
        .forwarded_by(&h.gateway, Method::GET, path, None, None, &[])
        .await;
    assert_401(s, &v, "no delegated context");
    // Wrong issuer (trusted, but not granted meeting-request.forward) and untrusted key.
    let (s, v, _) = h
        .forwarded_by(
            &h.developer_platform,
            Method::GET,
            path,
            None,
            Some(m.delegated()),
            &[],
        )
        .await;
    assert_401(s, &v, "wrong issuer");
    let (s, v, _) = h
        .forwarded_by(
            &h.rogue_gateway,
            Method::GET,
            path,
            None,
            Some(m.delegated()),
            &[],
        )
        .await;
    assert_401(s, &v, "untrusted key");
    // Both headers: never mixed (user session or participant token next to the token).
    let bearer = format!("Bearer {}", user.token);
    let (s, v, _) = h
        .forwarded_by(
            &h.gateway,
            Method::GET,
            path,
            None,
            Some(m.delegated()),
            &[("authorization", &bearer)],
        )
        .await;
    assert_401(s, &v, "both headers");
    let (s, v, _) = h
        .forwarded_by(
            &h.gateway,
            Method::GET,
            path,
            None,
            Some(m.delegated()),
            &[("x-participant-token", "pt_abc")],
        )
        .await;
    assert_401(s, &v, "participant token next to service token");
    // A route that is not machine-callable.
    let (s, v, _) = h
        .forwarded_by(
            &h.gateway,
            Method::GET,
            "/api/v2/me/organizations",
            None,
            Some(m.delegated()),
            &[],
        )
        .await;
    assert_401(s, &v, "not a platform operation");
    assert_eq!(
        h.pa.calls.load(Ordering::SeqCst),
        0,
        "no authority call yet"
    );

    // Platform Access refuses (revoked / unknown), names another credential, or answers
    // SAFETY_ONLY.
    let unknown = Machine::new(RW);
    let (s, v) = h.machine(&unknown, Method::GET, path, None).await;
    assert_401(s, &v, "rejected by Platform Access");
    let liar = Machine::new(RW);
    h.pa.answers
        .lock()
        .unwrap()
        .insert(liar.actor.credential_id, Machine::new(RW).chain(true));
    let (s, v) = h.machine(&liar, Method::GET, path, None).await;
    assert_401(s, &v, "chain of another credential");
    let suspended = Machine::new(RW);
    h.pa.answers
        .lock()
        .unwrap()
        .insert(suspended.actor.credential_id, suspended.chain(false));
    let (s, v) = h.machine(&suspended, Method::GET, path, None).await;
    assert_401(s, &v, "safety-only authority");

    // Platform Access unreachable: fail closed with 503.
    h.pa.unavailable.store(true, Ordering::SeqCst);
    let (s, v) = h.machine(&m, Method::GET, path, None).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    h.pa.unavailable.store(false, Ordering::SeqCst);
    let (s, _) = h.machine(&m, Method::GET, path, None).await;
    assert_eq!(s, StatusCode::OK);

    // User flows are unchanged by the receiver branch.
    let (s, v) = h.app.get(path, Auth::Bearer(&user.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = h.app.get(path, Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn service_tokens_are_refused_when_platform_access_is_off() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let (_, _, gateway, _, _) = receiver();
    let m = Machine::new(RW);
    let signed = gateway
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::meeting_service())
                .with_delegated_context(m.delegated()),
        )
        .unwrap();
    let headers: Vec<(String, String)> = signed
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    for (method, path) in [
        (Method::GET, "/api/v2/meeting-spaces"),
        (Method::POST, "/api/v2/instant-meetings"),
    ] {
        let (s, v) = app
            .call_with_headers(method, path, &refs, "{}".into())
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{path}: {v}");
    }
    // A public read that is anonymous for users is not reachable with a token either.
    let user = app.user("u").await;
    let space = app
        .create_space(&user, json!({ "title": "x", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, _) = app
        .call_with_headers(
            Method::GET,
            &format!("/api/v2/meeting-spaces/{}", space["id"].as_str().unwrap()),
            &refs,
            String::new(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
