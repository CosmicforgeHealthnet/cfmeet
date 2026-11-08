//! SEC-020 — tenant-isolation sweep (negative authorization on real PostgreSQL).
//!
//! The meeting domain's "tenants" are: the owning account / members of a meeting space
//! (first-party callers), the owning tenant + application + environment of a
//! machine-created meeting (gateway-forwarded platform callers), the organization (legacy
//! organization directory) and the legacy v1 meeting host. Each test below drives one
//! scope-derivation pattern with a caller from ANOTHER tenant and proves the request is
//! refused (403 / 404 / 401) and the victim tenant's rows are unchanged:
//!
//! * `sec020_single_id_meeting_operations_refuse_foreign_callers` — every `/api/v2`
//!   operation whose only path id is a meeting space or conference (contract-derived),
//!   called by another meeting's owner and by another meeting's guest credential
//!   (subordinate ids are covered by `idor_matrix.rs`).
//! * `sec020_every_machine_operation_is_environment_scoped` — every machine-callable
//!   operation (`services::platform::operations::OPERATIONS`) that names a meeting,
//!   called by another tenant and by a sibling environment of the same application.
//! * `sec020_organization_admin_cannot_manage_another_organization` — legacy directory.
//! * `sec020_legacy_v1_meeting_routes_are_host_scoped` — legacy v1 meeting, participant
//!   and chat routes, including the regression for the legacy chat sender fix.

mod common;

use axum::http::{Method, StatusCode};
use backend::services::platform::operations::{Target, OPERATIONS};
use common::{
    artifacts as art,
    contract::openapi,
    platform::{Harness, Machine},
    *,
};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

/// md5 digest of the rows each query selects (order-independent).
async fn digest(app: &TestApp, queries: &[String]) -> String {
    let mut out = String::new();
    for sql in queries {
        let row = app
            .db
            .conn
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT coalesce(md5(string_agg(row_to_json(t)::text, '|' ORDER BY row_to_json(t)::text)), '-') FROM ({sql}) t"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        out.push_str(&row.try_get_by_index::<String>(0).unwrap());
        out.push(':');
    }
    out
}

fn meeting_rows(sid: Uuid, cid: Uuid) -> Vec<String> {
    vec![
        format!("SELECT * FROM meeting_spaces WHERE id='{sid}'"),
        format!("SELECT * FROM conference_sessions WHERE meeting_space_id='{sid}'"),
        format!("SELECT * FROM conference_participants WHERE conference_id='{cid}'"),
        format!("SELECT * FROM participant_sessions WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_members WHERE meeting_space_id='{sid}'"),
        format!("SELECT * FROM meeting_invitations WHERE meeting_space_id='{sid}'"),
        format!("SELECT * FROM breakout_rooms WHERE conference_id='{cid}'"),
        format!("SELECT * FROM conference_messages WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_polls WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_questions WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_artifacts WHERE conference_id='{cid}'"),
        format!("SELECT * FROM transcription_jobs WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_consents WHERE conference_id='{cid}'"),
    ]
}

/// Operations a caller without authority may legitimately complete: the SUMMARY reads
/// (an id identifies, it never authorizes — A–D contract) and the public pre-join read.
/// They are still called: no victim data may leak and no victim row may change.
fn summary_read(method: &str, path: &str) -> bool {
    method == "get"
        && matches!(
            path,
            "/api/v2/meeting-spaces/{id}"
                | "/api/v2/conferences/{id}"
                | "/api/v2/conferences/{id}/pre-join"
        )
}

/// Operations not called here: admission (`join` — a stranger joining a PUBLIC meeting
/// is the product; identity rules are `security_v2.rs` / `idor_matrix.rs`), webinar
/// self-registration (creates the caller's own registration) and the WebSocket upgrade.
fn not_called(method: &str, path: &str) -> bool {
    matches!(
        (method, path),
        ("post", "/api/v2/conferences/{id}/join")
            | ("post", "/api/v2/meeting-spaces/{id}/registrations")
            | ("get", "/api/v2/conferences/{id}/realtime")
    )
}

#[tokio::test]
async fn sec020_single_id_meeting_operations_refuse_foreign_callers() {
    let Some(app) = TestApp::artifacts().await else {
        return;
    };
    // Tenant A: a live meeting with a connected guest and a chat message carrying a
    // marker that must never reach another tenant.
    let (a_owner, a_sid, a_cid, a_guests) = art::meeting_with(
        &app,
        json!({ "title": "A", "accessPolicy": "PUBLIC", "transcriptionPolicy": "HOST_OPTIONAL" }),
        1,
    )
    .await;
    let marker = format!("sec020-secret-{}", Uuid::new_v4().simple());
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{a_cid}/messages"),
            a_guests[0].auth(),
            json!({ "body": marker }),
        )
        .await;
    assert!(s.is_success(), "seed message: {s} {v}");
    // Tenant B: another owner with its own meeting and guest.
    let (b_owner, _b_sid, _b_cid, b_guests) =
        art::meeting_with(&app, json!({ "title": "B", "accessPolicy": "PUBLIC" }), 1).await;

    let doc = openapi();
    let params = regex::Regex::new(r"\{(\w+)\}").unwrap();
    let callers: [(&str, Auth<'_>); 2] = [
        ("foreign owner", Auth::Bearer(&b_owner.token)),
        ("foreign guest", b_guests[0].auth()),
    ];
    let mut attempts = 0;
    let mut failures = vec![];
    for (path, item) in doc["paths"].as_object().unwrap() {
        let names: Vec<String> = params
            .captures_iter(path)
            .map(|c| c[1].to_string())
            .collect();
        let target = if path.starts_with("/api/v2/conferences/{id}") {
            a_cid
        } else if path.starts_with("/api/v2/meeting-spaces/{id}") {
            a_sid
        } else {
            continue;
        };
        if names != ["id"] {
            continue;
        }
        let uri = path.replace("{id}", &target.to_string());
        for method in item.as_object().unwrap().keys() {
            if not_called(method, path) {
                continue;
            }
            let m: Method = method.to_uppercase().parse().unwrap();
            let body = matches!(method.as_str(), "post" | "patch" | "put").then(|| json!({}));
            for (who, auth) in &callers {
                let before = digest(&app, &meeting_rows(a_sid, a_cid)).await;
                let (s, v) = app.call(m.clone(), &uri, *auth, body.clone()).await;
                attempts += 1;
                let what = format!("{who}: {} {path}", method.to_uppercase());
                if s.is_success() && !summary_read(method, path) {
                    failures.push(format!("{what} → {s} {v}"));
                } else if !s.is_success() && !s.is_client_error() {
                    failures.push(format!("{what} → unexpected {s} {v}"));
                }
                if v.to_string().contains(&marker) {
                    failures.push(format!("{what} leaked tenant A's chat: {v}"));
                }
                if s.is_success() && v.get("ownerUserId").is_some_and(|o| !o.is_null()) {
                    failures.push(format!("{what} disclosed the owner: {v}"));
                }
                if digest(&app, &meeting_rows(a_sid, a_cid)).await != before {
                    failures.push(format!("{what} changed tenant A's rows"));
                }
            }
        }
    }
    // Lists are derived from the caller: A's meeting is never listed for B.
    let (s, v) = app
        .get("/api/v2/meeting-spaces", Auth::Bearer(&b_owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(!v.to_string().contains(&a_sid.to_string()), "{v}");
    let (s, v) = app
        .get("/api/v2/me/conferences", Auth::Bearer(&b_owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(!v.to_string().contains(&a_cid.to_string()), "{v}");
    // Positive control: A's owner reads A's chat through the same router.
    let (s, v) = app
        .get(
            format!("/api/v2/conferences/{a_cid}/messages"),
            Auth::Bearer(&a_owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v.to_string().contains(&marker), "control: {v}");

    println!("SEC-020 foreign-caller attempts: {attempts}");
    assert!(attempts >= 120, "sweep too small: {attempts}");
    assert!(
        failures.is_empty(),
        "tenant-isolation failures:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn sec020_every_machine_operation_is_environment_scoped() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = Machine::new(&["meetings.read", "meetings.write"]);
    let other_tenant = Machine::new(&["meetings.read", "meetings.write"]);
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
    let sid = uuid(&v["meetingSpace"]["id"]);
    let cid = uuid(&v["conference"]["id"]);
    let rows = vec![
        format!("SELECT * FROM meeting_spaces WHERE id='{sid}'"),
        format!("SELECT * FROM conference_sessions WHERE meeting_space_id='{sid}'"),
        format!("SELECT * FROM conference_participants WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_invitations WHERE meeting_space_id='{sid}'"),
    ];

    let mut attempts = 0;
    for outsider in [&other_tenant, &sibling] {
        for op in OPERATIONS {
            let (id, code) = match op.target {
                Target::None => continue,
                Target::Space => (sid, "MEETING_NOT_FOUND"),
                Target::Conference => (cid, "CONFERENCE_NOT_FOUND"),
            };
            let path = op
                .route
                .split('/')
                .map(|seg| match seg {
                    ":id" => id.to_string(),
                    s if s.starts_with(':') => Uuid::new_v4().to_string(),
                    s => s.to_string(),
                })
                .collect::<Vec<_>>()
                .join("/");
            let method: Method = op.method.parse().unwrap();
            let body = (method != Method::GET).then(|| json!({}));
            let before = digest(&h.app, &rows).await;
            let (s, v) = h.machine(outsider, method.clone(), &path, body).await;
            attempts += 1;
            assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {v}");
            assert_eq!(v["error"]["code"], code, "{method} {path}: {v}");
            assert_eq!(
                digest(&h.app, &rows).await,
                before,
                "{method} {path} changed tenant A's rows"
            );
        }
    }
    assert!(attempts >= 30, "machine sweep too small: {attempts}");
    // Positive control: the owning environment reads its own meeting.
    let (s, v) = h
        .machine(
            &a,
            Method::GET,
            &format!("/api/v2/conferences/{cid}/participants"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

async fn create_org(app: &TestApp, admin: &User, slug: &str, owner: &User) -> Uuid {
    let (s, v) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&admin.token),
            json!({ "name": slug, "slug": slug, "ownerUserId": owner.id }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    uuid(&v["id"])
}

#[tokio::test]
async fn sec020_organization_admin_cannot_manage_another_organization() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let admin = app.admin("platform").await;
    let a_owner = app.user("org-a-owner").await;
    let b_owner = app.user("org-b-owner").await;
    let b_member = app.user("org-b-member").await;
    let suffix = &Uuid::new_v4().simple().to_string()[..8];
    let a = create_org(&app, &admin, &format!("sec020a{suffix}"), &a_owner).await;
    let b = create_org(&app, &admin, &format!("sec020b{suffix}"), &b_owner).await;
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/organizations/{b}/members/{}", b_member.id),
            Auth::Bearer(&b_owner.token),
            Some(json!({ "role": "MEMBER", "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let b_rows = || {
        vec![
            format!("SELECT * FROM organizations WHERE id='{b}'"),
            format!("SELECT * FROM organization_memberships WHERE organization_id='{b}'"),
        ]
    };
    let before = digest(&app, &b_rows()).await;

    // A's OWNER (full authority in A) has none in B.
    let (s, v) = app
        .get(
            format!("/api/v2/organizations/{b}/members"),
            Auth::Bearer(&a_owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "list B's members: {v}");
    assert!(!v.to_string().contains(&b_member.id.to_string()), "{v}");
    for (target, role, status) in [
        (a_owner.id, "OWNER", "ACTIVE"),
        (b_member.id, "ADMIN", "ACTIVE"),
        (b_member.id, "MEMBER", "REMOVED"),
        (b_owner.id, "MEMBER", "SUSPENDED"),
    ] {
        let (s, v) = app
            .call(
                Method::PUT,
                &format!("/api/v2/organizations/{b}/members/{target}"),
                Auth::Bearer(&a_owner.token),
                Some(json!({ "role": role, "status": status })),
            )
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{target} {role} {status}: {v}");
    }
    // The platform-administrator facade is not an organization owner's.
    let (s, v) = app
        .call(
            Method::PATCH,
            &format!("/api/v2/admin/organizations/{b}"),
            Auth::Bearer(&a_owner.token),
            Some(json!({ "status": "SUSPENDED" })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "suspend B: {v}");
    assert_eq!(digest(&app, &b_rows()).await, before, "B's rows changed");
    // Organization lists come from the caller's own memberships.
    let (s, v) = app
        .get("/api/v2/me/organizations", Auth::Bearer(&a_owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v.to_string().contains(&a.to_string()), "{v}");
    assert!(!v.to_string().contains(&b.to_string()), "{v}");
}

/// A live legacy (v1) meeting of `host` with one joined guest participant.
async fn legacy_meeting_with_guest(app: &TestApp, host: &User) -> (Uuid, Uuid) {
    let id = Uuid::new_v4();
    let participant = Uuid::new_v4();
    let code: String = format!("S{}", &id.simple().to_string()[..7])
        .to_uppercase()
        .replace(['0', '1', 'O', 'I'], "Q");
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
             VALUES ('{id}', '{code}', '{h}', '{h}', 'legacy', false, now() - interval '5 minutes', now() + interval '1 hour', 'ongoing'); \
             INSERT INTO participants (id, meeting_id, user_id, role, join_time, status, is_muted, is_video_on, is_screen_sharing, display_name) \
             VALUES ('{participant}', '{id}', NULL, 'participant', now(), 'joined', false, false, false, 'Guest')",
            h = host.id
        ))
        .await
        .unwrap();
    (id, participant)
}

fn guest_token(app: &TestApp, participant: Uuid) -> String {
    app.state
        .jwt_service
        .generate_access_token(participant, "guest")
        .unwrap()
}

#[tokio::test]
async fn sec020_legacy_v1_meeting_routes_are_host_scoped() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let a_host = app.user("legacy-a").await;
    let b_host = app.user("legacy-b").await;
    let (a_meeting, a_guest) = legacy_meeting_with_guest(&app, &a_host).await;
    let (_b_meeting, b_guest) = legacy_meeting_with_guest(&app, &b_host).await;
    let a_guest_token = guest_token(&app, a_guest);
    let b_guest_token = guest_token(&app, b_guest);
    let rows = vec![
        format!("SELECT * FROM meetings WHERE id='{a_meeting}'"),
        format!("SELECT * FROM participants WHERE meeting_id='{a_meeting}'"),
        format!("SELECT * FROM chat_messages WHERE meeting_id='{a_meeting}'"),
    ];
    let before = digest(&app, &rows).await;

    let m = format!("/api/v1/meetings/{a_meeting}");
    let p = format!("/api/v1/participants/{a_guest}");
    let attempts: Vec<(Method, String, Option<Value>)> = vec![
        (Method::GET, m.clone(), None),
        (Method::PUT, m.clone(), Some(json!({ "title": "hijack" }))),
        (Method::DELETE, m.clone(), None),
        (Method::POST, format!("{m}/end"), Some(json!({}))),
        (Method::GET, format!("{m}/participants"), None),
        (Method::GET, format!("{m}/waiting"), None),
        (
            Method::POST,
            format!("{m}/waiting/{a_guest}/admit"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("{m}/waiting/{a_guest}/deny"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("{m}/screen-share/start"),
            Some(json!({})),
        ),
        (Method::GET, format!("{m}/chat"), None),
        (Method::POST, format!("{p}/kick"), Some(json!({}))),
        (
            Method::PATCH,
            format!("{p}/audio"),
            Some(json!({ "is_muted": true })),
        ),
        (
            Method::PATCH,
            format!("{p}/video"),
            Some(json!({ "is_video_on": true })),
        ),
        // SEC-020 defect regression: posting into A's chat AS A's guest participant.
        (
            Method::POST,
            format!("{m}/chat"),
            Some(json!({ "participant_id": a_guest, "message": "impersonated" })),
        ),
    ];
    for (who, token) in [
        ("another host", b_host.token.as_str()),
        ("another meeting's guest", b_guest_token.as_str()),
    ] {
        for (method, uri, body) in &attempts {
            let (s, v) = app
                .call(method.clone(), uri, Auth::Bearer(token), body.clone())
                .await;
            assert!(
                matches!(s, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
                "{who}: {method} {uri} → {s} {v}"
            );
        }
    }
    // The host is not its guests either: a host cannot post as a guest participant.
    let (s, v) = app
        .post(
            format!("{m}/chat"),
            Auth::Bearer(&a_host.token),
            json!({ "participant_id": a_guest, "message": "as the guest" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "host as guest: {v}");
    assert_eq!(digest(&app, &rows).await, before, "tenant A's rows changed");

    // Positive controls: the guest posts as itself; the host reads its own meeting.
    let (s, v) = app
        .post(
            format!("{m}/chat"),
            Auth::Bearer(&a_guest_token),
            json!({ "participant_id": a_guest, "message": "hello" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "guest as itself: {v}");
    let (s, v) = app.get(&m, Auth::Bearer(&a_host.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .get(format!("{m}/chat"), Auth::Bearer(&a_host.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(!v.to_string().contains("impersonated"), "{v}");
}
