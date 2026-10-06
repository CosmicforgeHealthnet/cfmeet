//! `/api/v2` endpoint coverage across owner / host / co-host / moderator / presenter /
//! participant / viewer / member / guest / invitee, plus feature-flag and provider
//! failure behaviour.

mod common;

use std::collections::HashMap;

use axum::http::{Method, StatusCode};
use backend::{config::FeatureFlags, domain::Role};
use chrono::{Duration, Utc};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::new().await {
            Some(a) => a,
            None => return,
        }
    };
    ($opts:expr) => {
        match TestApp::with($opts).await {
            Some(a) => a,
            None => return,
        }
    };
}

struct Cast {
    owner: User,
    users: HashMap<Role, User>,
    pids: HashMap<Role, Uuid>,
    sid: Uuid,
    cid: Uuid,
}

async fn cast(app: &TestApp) -> Cast {
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(&owner, json!({ "title": "Matrix", "accessPolicy": "AUTHENTICATED", "waitingRoomPolicy": "NON_MEMBERS_ONLY" }))
        .await;
    let mut users = HashMap::new();
    let mut pids = HashMap::new();
    for role in [
        Role::Host,
        Role::CoHost,
        Role::Moderator,
        Role::Presenter,
        Role::Participant,
        Role::Viewer,
    ] {
        let u = app.user(role.as_str()).await;
        app.add_member(sid, &owner, &u, role).await;
        let (s, v) = app.join(cid, Auth::Bearer(&u.token), json!({})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["participant"]["role"], role.as_str());
        assert_eq!(
            v["participant"]["admissionState"], "ADMITTED",
            "members skip NON_MEMBERS_ONLY waiting"
        );
        pids.insert(role, uuid(&v["participant"]["id"]));
        users.insert(role, u);
    }
    Cast {
        owner,
        users,
        pids,
        sid,
        cid,
    }
}

#[tokio::test]
async fn r01_permission_matrix_over_http() {
    let app = app!();
    let c = cast(&app).await;
    let lock = |u: &User| {
        let app = &app;
        let token = u.token.clone();
        let cid = c.cid;
        async move {
            let (s, _) = app
                .post(
                    &format!("/api/v2/conferences/{cid}/lock"),
                    Auth::Bearer(&token),
                    json!({}),
                )
                .await;
            app.post(
                &format!("/api/v2/conferences/{cid}/unlock"),
                Auth::Bearer(&token),
                json!({}),
            )
            .await;
            s
        }
    };
    assert_eq!(lock(&c.owner).await, StatusCode::OK);
    for (role, expected) in [
        (Role::Host, StatusCode::OK),
        (Role::CoHost, StatusCode::OK),
        (Role::Moderator, StatusCode::FORBIDDEN),
        (Role::Presenter, StatusCode::FORBIDDEN),
        (Role::Participant, StatusCode::FORBIDDEN),
        (Role::Viewer, StatusCode::FORBIDDEN),
    ] {
        assert_eq!(lock(&c.users[&role]).await, expected, "lock as {role}");
    }
    // Waiting room access follows participant.admit (MODERATOR = policy default allow).
    for (role, expected) in [
        (Role::Host, StatusCode::OK),
        (Role::CoHost, StatusCode::OK),
        (Role::Moderator, StatusCode::OK),
        (Role::Presenter, StatusCode::FORBIDDEN),
        (Role::Participant, StatusCode::FORBIDDEN),
        (Role::Viewer, StatusCode::FORBIDDEN),
    ] {
        let (s, _) = app
            .get(
                &format!("/api/v2/conferences/{}/waiting-room", c.cid),
                Auth::Bearer(&c.users[&role].token),
            )
            .await;
        assert_eq!(s, expected, "waiting room as {role}");
    }
    // Ban: co-host policy default deny; remove: moderator allowed on lower roles.
    let (s, _) = app
        .post(
            &format!(
                "/api/v2/conferences/{}/participants/{}/ban",
                c.cid,
                c.pids[&Role::Viewer]
            ),
            Auth::Bearer(&c.users[&Role::CoHost].token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app
        .post(
            &format!(
                "/api/v2/conferences/{}/participants/{}/remove",
                c.cid,
                c.pids[&Role::Host]
            ),
            Auth::Bearer(&c.users[&Role::Moderator].token),
            json!({}),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "cannot moderate a more privileged role"
    );
    let (s, v) = app
        .post(
            &format!(
                "/api/v2/conferences/{}/participants/{}/remove",
                c.cid,
                c.pids[&Role::Viewer]
            ),
            Auth::Bearer(&c.users[&Role::Moderator].token),
            json!({ "reason": "spam" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "REMOVED");
    // Members list: owner / host / co-host only.
    for (role, expected) in [
        (Role::Host, StatusCode::OK),
        (Role::CoHost, StatusCode::OK),
        (Role::Moderator, StatusCode::FORBIDDEN),
        (Role::Participant, StatusCode::FORBIDDEN),
    ] {
        let (s, _) = app
            .get(
                &format!("/api/v2/meeting-spaces/{}/members", c.sid),
                Auth::Bearer(&c.users[&role].token),
            )
            .await;
        assert_eq!(s, expected, "members as {role}");
    }
    // End: owner/host only (co-host policy default deny).
    let (s, _) = app
        .post(
            &format!("/api/v2/conferences/{}/end", c.cid),
            Auth::Bearer(&c.users[&Role::CoHost].token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{}/end", c.cid),
            Auth::Bearer(&c.users[&Role::Host].token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn r02_media_permission_projection() {
    let app = app!();
    let owner = app.user("owner").await;
    let viewer = app.user("viewer").await;
    let presenter = app.user("presenter").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "m", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    app.add_member(sid, &owner, &viewer, Role::Viewer).await;
    app.add_member(sid, &owner, &presenter, Role::Presenter)
        .await;
    let (_, v) = app.join(cid, Auth::Bearer(&viewer.token), json!({})).await;
    assert_eq!(
        v["permissions"],
        json!({ "audioPublish": false, "videoPublish": false, "screenSharePublish": false, "dataPublish": true, "subscribe": true })
    );
    let last = app.rtc.last_permissions.lock().unwrap().unwrap();
    assert!(
        !last.can_publish_anything(),
        "viewer credential carries no publish grant"
    );
    let (_, p) = app
        .join(cid, Auth::Bearer(&presenter.token), json!({}))
        .await;
    assert_eq!(p["permissions"]["screenSharePublish"], true);
    assert_eq!(p["policy"]["recordingActive"], false);
}

#[tokio::test]
async fn r03_space_lifecycle_endpoints() {
    let app = app!();
    let owner = app.user("owner").await;
    let host = app.user("host").await;
    let stranger = app.user("stranger").await;
    let space = app.create_space(&owner, json!({ "title": "Life", "description": "d", "accessPolicy": "PUBLIC_PASSCODE", "passcode": "pass-1234" })).await;
    let sid = uuid(&space["id"]);
    app.add_member(sid, &owner, &host, Role::Host).await;
    let (_, list) = app
        .get("/api/v2/meeting-spaces", Auth::Bearer(&host.token))
        .await;
    assert_eq!(
        list["items"].as_array().unwrap().len(),
        1,
        "member spaces listed"
    );
    // Duplicate copies config, not history; new owner = caller, new code.
    let (_, c) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let _ = c;
    let (s, dup) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/duplicate"),
            Auth::Bearer(&host.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{dup}");
    let did = uuid(&dup["id"]);
    assert_ne!(dup["meetingCode"], space["meetingCode"]);
    assert_eq!(uuid(&dup["ownerUserId"]), host.id);
    assert_eq!(dup["accessPolicy"], "PUBLIC_PASSCODE");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{did}'"
        ))
        .await,
        0
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_members WHERE meeting_space_id='{did}'"
        ))
        .await,
        0
    );
    assert_eq!(
        app.post(
            &format!("/api/v2/meeting-spaces/{sid}/duplicate"),
            Auth::Bearer(&stranger.token),
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // Owner transfer: dedicated, protected, versioned.
    let (s, _) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/owner-transfer"),
            Auth::Bearer(&host.token),
            json!({ "newOwnerUserId": host.id, "version": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "host cannot take ownership");
    let (s, t) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/owner-transfer"),
            Auth::Bearer(&owner.token),
            json!({ "newOwnerUserId": host.id, "version": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(uuid(&t["ownerUserId"]), host.id);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_members WHERE meeting_space_id='{sid}' AND user_id='{}' AND status='ACTIVE' AND role='HOST'", owner.id)).await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_members WHERE meeting_space_id='{sid}' AND user_id='{}' AND status='ACTIVE'", host.id)).await, 0);
    // Archive: owner only, soft, cancels unstarted future occurrences, blocks writes.
    assert_eq!(
        app.post(
            &format!("/api/v2/meeting-spaces/{sid}/archive"),
            Auth::Bearer(&owner.token),
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (s, a) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/archive"),
            Auth::Bearer(&host.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{a}");
    assert_eq!(a["status"], "ARCHIVED");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}' AND status='CANCELLED'")).await, 1);
    let (s, v) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&host.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT")
    );
    assert!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_audit_events WHERE meeting_space_id='{sid}'"
        ))
        .await
            >= 5
    );
}

#[tokio::test]
async fn r04_instant_meeting_and_conference_views() {
    let app = app!();
    let owner = app.user("owner").await;
    let other = app.user("other").await;
    let (s, v) = app
        .post(
            "/api/v2/instant-meetings",
            Auth::Bearer(&owner.token),
            json!({ "title": "Quick" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["conference"]["status"], "LIVE");
    assert_eq!(v["conference"]["kind"], "INSTANT");
    assert_eq!(v["meetingSpace"]["accessPolicy"], "PUBLIC");
    let cid = uuid(&v["conference"]["id"]);
    assert!(
        !v.to_string().contains("rtcRoom") && !v.to_string().contains("cf-"),
        "no provider ids exposed"
    );
    for e in [
        "conference.created",
        "conference.preparing",
        "conference.started",
    ] {
        assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='{e}'")).await, 1, "{e}");
    }
    let (_, summary) = app
        .get(
            &format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&other.token),
        )
        .await;
    assert_eq!(summary["view"], "SUMMARY");
    assert!(summary.get("startedAt").is_none());
    let (_, full) = app
        .get(
            &format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(full["view"], "FULL");
    // Roster and me.
    let (_, j) = app.join(cid, Auth::Bearer(&other.token), json!({})).await;
    let (_, me) = app
        .get(
            &format!("/api/v2/conferences/{cid}/participants/me"),
            Auth::Bearer(&other.token),
        )
        .await;
    assert_eq!(me["id"], j["participant"]["id"]);
    let (s, roster) = app
        .get(
            &format!("/api/v2/conferences/{cid}/participants"),
            Auth::Bearer(&other.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(roster.as_array().unwrap().len(), 1);
    let (s, _) = app
        .get(
            &format!("/api/v2/conferences/{cid}/participants"),
            Auth::None,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn r05_deny_return_to_waiting_and_messages() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "w", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    let (_, g) = app
        .join(cid, Auth::None, json!({ "displayName": "g" }))
        .await;
    let token = g["participantToken"].as_str().unwrap().to_string();
    let pid = uuid(&g["participant"]["id"]);
    let (s, d) = app
        .post(
            &format!("/api/v2/conferences/{cid}/waiting-room/deny"),
            Auth::Bearer(&owner.token),
            json!({ "participantIds": [pid], "reasonCode": "NOT_SCHEDULED", "message": "Please book an appointment.", "internalNote": "repeat caller" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    let (_, me) = app
        .get(
            &format!("/api/v2/conferences/{cid}/participants/me"),
            Auth::Participant(&token),
        )
        .await;
    assert_eq!(me["admissionState"], "DENIED");
    assert_eq!(me["denialMessage"], "Please book an appointment.");
    assert!(
        !me.to_string().contains("repeat caller"),
        "internal note never exposed"
    );
    // Re-request after denial goes back to WAITING (host may ban instead).
    let (_, again) = app.join(cid, Auth::Participant(&token), json!({})).await;
    assert_eq!(again["participant"]["admissionState"], "WAITING");
    app.post(
        &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
        Auth::Bearer(&owner.token),
        json!({ "all": true }),
    )
    .await;
    app.join(cid, Auth::Participant(&token), json!({})).await;
    let (s, r) = app
        .post(
            &format!("/api/v2/conferences/{cid}/participants/{pid}/return-to-waiting"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["participant"]["admissionState"], "WAITING");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{pid}' AND state IN ('PENDING','CONNECTED')")).await, 0);
    let (_, w) = app.join(cid, Auth::Participant(&token), json!({})).await;
    assert_eq!(w["rtcStatus"], "NOT_ISSUED_WAITING");
}

#[tokio::test]
async fn r06_invitee_roles_and_listing() {
    let app = app!();
    let owner = app.user("owner").await;
    let invitee = app.user("invitee").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "i", "accessPolicy": "INVITE_ONLY" }),
        )
        .await;
    let (_, inv) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            json!({ "role": "PRESENTER", "inviteeEmail": invitee.email, "conferenceId": cid }),
        )
        .await;
    let (_, list) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert!(
        !list.to_string().contains(inv["token"].as_str().unwrap()),
        "no token in listings"
    );
    // Email-bound invitation: anonymous holder cannot use it.
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "x", "invitationToken": inv["token"] }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::GONE, "INVITATION_INVALID"));
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&invitee.token),
            json!({ "invitationToken": inv["token"] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "PRESENTER");
    // Reconnect without the token keeps access and the invitation role.
    let (s, v) = app.join(cid, Auth::Bearer(&invitee.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "PRESENTER");
    assert_eq!(
        app.scalar(&format!(
            "SELECT usage_count::bigint FROM meeting_invitations WHERE id='{}'",
            inv["invitation"]["id"].as_str().unwrap()
        ))
        .await,
        1,
        "reconnect consumes nothing"
    );
}

#[tokio::test]
async fn r07_recurrence_endpoints_and_timezone_follow() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app
        .create_space(&owner, json!({ "title": "r", "timezone": "UTC" }))
        .await;
    let sid = uuid(&space["id"]);
    let start = (Utc::now() + Duration::days(1))
        .date_naive()
        .and_hms_opt(14, 0, 0)
        .unwrap();
    let url = format!("/api/v2/meeting-spaces/{sid}/recurrence");
    let (s, v) = app
        .call(
            Method::PUT,
            &url,
            Auth::Bearer(&owner.token),
            Some(json!({ "rrule": "FREQ=MINUTELY", "startLocal": start, "durationMinutes": 30 })),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    let (s, _) = app.call(Method::PUT, &url, Auth::Bearer(&owner.token), Some(json!({ "rrule": "FREQ=DAILY;COUNT=3", "startLocal": start, "durationMinutes": 30 }))).await;
    assert_eq!(s, StatusCode::OK);
    let (_, rule) = app.get(&url, Auth::Bearer(&owner.token)).await;
    assert_eq!(rule["rrule"], "FREQ=DAILY;COUNT=3");
    // Timezone change re-anchors the series at the same wall-clock time.
    let (s, v) = app
        .patch(
            &format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
            json!({ "version": 1, "timezone": "America/New_York" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, list) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let created: Vec<&Value> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "CREATED")
        .collect();
    assert_eq!(created.len(), 3);
    assert!(
        created.iter().all(|c| !c["scheduledStartAt"]
            .as_str()
            .unwrap()
            .contains("T14:00:00")),
        "now 14:00 New York, not UTC"
    );
    let (s, _) = app
        .call(Method::DELETE, &url, Auth::Bearer(&owner.token), None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = app.get(&url, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}' AND status='CREATED'")).await, 0);
}

#[tokio::test]
async fn r08_member_lifecycle() {
    let app = app!();
    let owner = app.user("owner").await;
    let u = app.user("u").await;
    let space = app.create_space(&owner, json!({ "title": "m" })).await;
    let sid = uuid(&space["id"]);
    let (s, m) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&owner.token),
            json!({ "userId": u.id, "role": "PARTICIPANT" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let mid = m["id"].as_str().unwrap();
    let (s, upd) = app
        .patch(
            &format!("/api/v2/meeting-spaces/{sid}/members/{mid}"),
            Auth::Bearer(&owner.token),
            json!({ "role": "CO_HOST", "version": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{upd}");
    let (s, v) = app
        .patch(
            &format!("/api/v2/meeting-spaces/{sid}/members/{mid}"),
            Auth::Bearer(&owner.token),
            json!({ "role": "HOST", "version": 1 }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "MEETING_VERSION_CONFLICT")
    );
    let (s, r) = app
        .call(
            Method::DELETE,
            &format!("/api/v2/meeting-spaces/{sid}/members/{mid}"),
            Auth::Bearer(&owner.token),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["status"], "REVOKED");
    let (s, _) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&owner.token),
            json!({ "email": "Someone@Example.com", "role": "VIEWER" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&owner.token),
            json!({ "userId": owner.id, "role": "HOST" }),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "owner cannot add themself as a member"
    );
}

#[tokio::test]
async fn r09_feature_flags() {
    let app = app!(TestOptions {
        flags: FeatureFlags::default(),
        ..TestOptions::default()
    });
    let owner = app.user("owner").await;
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&owner.token),
            json!({ "title": "x" }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "FEATURE_DISABLED"));
    // Legacy API is unaffected.
    let (s, _) = app.get("/health", Auth::None).await;
    assert_eq!(s, StatusCode::OK);

    let allowed = Uuid::new_v4();
    let flags = FeatureFlags {
        meeting_v2_enabled: true,
        v2_write_allowlist: Some([allowed].into_iter().collect()),
        ..FeatureFlags::default()
    };
    let app = app!(TestOptions {
        flags,
        ..TestOptions::default()
    });
    let outsider = app.user("outsider").await;
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&outsider.token),
            json!({ "title": "x" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "FEATURE_DISABLED"),
        "controlled v2 writes"
    );
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&outsider.token),
            json!({ "title": "x", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    assert_eq!(code(&v), "FEATURE_DISABLED", "{s}");
}

#[tokio::test]
async fn r10_rtc_provider_gating_and_outage() {
    // Part E flag off: business admission commits, no credential is fabricated.
    let flags = FeatureFlags {
        rtc_provider_control_v2: false,
        ..FeatureFlags::all_v2_enabled()
    };
    let app = app!(TestOptions {
        flags,
        ..TestOptions::default()
    });
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "x", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "g" }))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["rtcStatus"], "PROVIDER_CONTROL_DISABLED");
    assert!(v["rtc"].is_null());
    assert_eq!(v["participant"]["admissionState"], "ADMITTED");
    assert_eq!(app.rtc.issued.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Provider outage: authorized business state preserved, session not usable, 503.
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "x", "accessPolicy": "PUBLIC" }))
        .await;
    app.rtc
        .fail_issue
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "g" }))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE")
    );
    assert!(
        !v.to_string().contains("fake outage"),
        "provider errors never exposed"
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND admission_state='ADMITTED'")).await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state='TERMINATED' AND end_reason='RTC_PROVIDER_UNAVAILABLE'")).await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state IN ('PENDING','CONNECTED')")).await, 0);
}

#[tokio::test]
async fn r11_scheduler_auto_end_and_outbox_relay() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "x", "accessPolicy": "PUBLIC" }))
        .await;
    use sea_orm::ConnectionTrait;
    app.db
        .conn
        .execute_unprepared(&format!("UPDATE conference_sessions SET scheduled_start_at = now() - interval '3 hours', scheduled_end_at = now() - interval '2 hours' WHERE id='{cid}'"))
        .await
        .unwrap();
    let scheduler = backend::workers::ConferenceScheduler::new(app.state.meeting.clone());
    let t1 = scheduler.tick(Utc::now()).await.unwrap();
    let t2 = scheduler.tick(Utc::now()).await.unwrap();
    assert_eq!(
        (t1.end_requested, t2.end_requested),
        (1, 0),
        "idempotent across ticks"
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND status='ENDING' AND termination_reason='SCHEDULED_END'")).await, 1);
    // Outbox relay delivers committed events exactly once per claim.
    let relay = backend::workers::outbox_relay::OutboxRelay::new(
        app.db.conn.clone(),
        std::sync::Arc::new(backend::workers::outbox_relay::LogPublisher),
    );
    let pending = app
        .scalar("SELECT count(*) FROM outbox_events WHERE status='PENDING'")
        .await;
    let (published, _) = relay.tick(1000).await.unwrap();
    assert_eq!(published as i64, pending);
    assert_eq!(relay.tick(1000).await.unwrap(), (0, 0));
}
