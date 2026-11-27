//! Server audit — meetings (M2, M3, M4, M9, M10, MF1, MF2) on the real code paths and a
//! real PostgreSQL: legacy auto-end grace and warning, the ending-soon warning and the
//! host extension, unattended waiting rooms, the in-app-only notice path, iTIP calendar
//! invitations, idempotent webinar registration with its join-link email, and the private
//! calendar feed.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use backend::{
    config::MeetingV2Config,
    domain::Role,
    services::{
        notification_fence::MeetingCategory,
        notifications::{self as notify, NoPreferenceStore, NotificationContext, Notifier},
    },
    workers::{ConferenceScheduler, MeetingAutoEndWorker},
};
use chrono::{DateTime, Duration, Utc};
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! some {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => return,
        }
    };
}

fn rfc(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

async fn exec(app: &TestApp, sql: &str) {
    app.db
        .conn
        .execute(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap();
}

async fn text(app: &TestApp, sql: &str) -> Option<String> {
    app.db
        .conn
        .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .and_then(|r| r.try_get_by_index::<Option<String>>(0).unwrap())
}

async fn tick(app: &TestApp) -> backend::workers::conference_scheduler::SchedulerTick {
    ConferenceScheduler::new(app.state.meeting.clone())
        .tick(Utc::now())
        .await
        .unwrap()
}

/// A scheduled conference `[start, end)` that is started now. Returns (space, conference).
async fn scheduled_live(app: &TestApp, owner: &User, end: DateTime<Utc>) -> (Uuid, Uuid) {
    let space = app
        .create_space(
            owner,
            json!({ "title": "Weekly sync", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    let start = Utc::now() - Duration::minutes(1);
    let (s, c) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(start), "scheduledEndAt": rfc(end) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let cid = uuid(&c["id"]);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/start"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    (sid, cid)
}

async fn events(app: &TestApp, cid: Uuid, kind: &str) -> i64 {
    app.scalar(&format!(
        "SELECT count(*) FROM outbox_events WHERE conference_id = '{cid}' AND event_type = '{kind}'"
    ))
    .await
}

// ------------------------------------------------------------------- MF1 / M2

/// MF1: everyone is warned before the automatic end (once per automatic-end instant), the
/// host extends the end, and the extension is announced and warned again later.
#[tokio::test]
async fn ending_soon_warning_and_host_extension() {
    let app = some!(TestApp::new().await);
    let owner = app.user("owner").await;
    let guest = app.user("guest").await;
    let (_, cid) = scheduled_live(&app, &owner, Utc::now() + Duration::minutes(30)).await;

    // Not due yet.
    assert_eq!(tick(&app).await.end_warned, 0);
    // Scheduled end 12 minutes ago: the automatic end (+15 min grace) is 3 minutes away.
    exec(
        &app,
        &format!("UPDATE conference_sessions SET scheduled_start_at = now() - interval '60 minutes', scheduled_end_at = now() - interval '12 minutes' WHERE id = '{cid}'"),
    )
    .await;
    assert_eq!(tick(&app).await.end_warned, 1);
    assert_eq!(tick(&app).await.end_warned, 0, "warned once");
    let payload = text(
        &app,
        &format!("SELECT payload::text FROM outbox_events WHERE conference_id = '{cid}' AND event_type = 'conference.ending_soon'"),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert!(payload["autoEndAt"].is_string() && payload["minutesRemaining"].as_i64() <= Some(3));

    // Validation and authorization.
    let uri = format!("/api/v2/conferences/{cid}/extend");
    let (s, v) = app
        .post(&uri, Auth::Bearer(&owner.token), json!({ "minutes": 0 }))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = app
        .post(&uri, Auth::Bearer(&guest.token), json!({ "minutes": 15 }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");

    // The host extends: from now (the scheduled end already passed) + 15 minutes.
    let (s, v) = app
        .post(&uri, Auth::Bearer(&owner.token), json!({ "minutes": 15 }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let end: DateTime<Utc> = v["conference"]["scheduledEndAt"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(end > Utc::now() + Duration::minutes(14));
    assert_eq!(events(&app, cid, "conference.extended").await, 1);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_audit_events WHERE conference_id = '{cid}' AND action = 'CONFERENCE_EXTEND'"
        ))
        .await,
        1
    );
    // The new automatic end is 30 minutes away: no warning now, and nothing ends.
    let t = tick(&app).await;
    assert_eq!((t.end_warned, t.end_requested), (0, 0));

    // Only a live conference can be extended.
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app
        .post(&uri, Auth::Bearer(&owner.token), json!({ "minutes": 15 }))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
}

/// M2: a legacy meeting ends only after the same 15-minute grace as v2 meetings.
#[tokio::test]
async fn legacy_meetings_get_the_grace_period() {
    let app = some!(TestApp::new().await);
    let host = app.user("host").await;
    let mut ids = vec![];
    for minutes_ago in [5, 20] {
        let id = Uuid::new_v4();
        let code = format!("A{}", &id.simple().to_string()[..7])
            .to_uppercase()
            .replace(['0', '1', 'O', 'I'], "Q");
        exec(
            &app,
            &format!(
                "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
                 VALUES ('{id}', '{code}', '{}', '{}', 'legacy', false, now() - interval '2 hours', now() - interval '{minutes_ago} minutes', 'ongoing')",
                host.id, host.id
            ),
        )
        .await;
        ids.push(id);
    }
    let worker = MeetingAutoEndWorker::new(app.db.conn.clone());
    assert_eq!(
        worker.process_batch().await.unwrap(),
        1,
        "only the one past its grace"
    );
    let status = |id: Uuid| format!("SELECT status::text FROM meetings WHERE id = '{id}'");
    assert_eq!(
        text(&app, &status(ids[0])).await.as_deref(),
        Some("ongoing")
    );
    assert_eq!(text(&app, &status(ids[1])).await.as_deref(), Some("ended"));
    // The one in its grace period is never warned without a live v2 conference to tell.
    assert_eq!(worker.warn_batch().await.unwrap(), 0);
}

// ---------------------------------------------------------------------------- M3

/// M3: nobody who can admit is present: the waiting people are told, and after the grace
/// period their wait ends with NO_HOST_AVAILABLE (nobody can be promoted).
#[tokio::test]
async fn unattended_waiting_room_is_told_then_ended() {
    let app = some!(TestApp::new().await);
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "w", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "Waiting guest" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "WAITING");
    let pid = uuid(&v["participant"]["id"]);

    assert_eq!(tick(&app).await.waiting_rooms_attended, 1);
    assert_eq!(
        events(&app, cid, "conference.waiting_room_unattended").await,
        1
    );
    // Within the grace period nothing more happens.
    assert_eq!(tick(&app).await.waiting_rooms_attended, 0);
    // Past the grace period: the wait ends with a reason the guest is shown.
    exec(
        &app,
        &format!("UPDATE outbox_events SET created_at = created_at - interval '3 minutes' WHERE conference_id = '{cid}' AND event_type = 'conference.waiting_room_unattended'"),
    )
    .await;
    exec(
        &app,
        &format!("UPDATE conference_participants SET requested_at = requested_at - interval '4 minutes' WHERE id = '{pid}'"),
    )
    .await;
    assert_eq!(tick(&app).await.waiting_rooms_attended, 1);
    assert_eq!(
        text(&app, &format!("SELECT admission_state || ':' || denial_reason_code FROM conference_participants WHERE id = '{pid}'")).await.as_deref(),
        Some("DENIED:NO_HOST_AVAILABLE")
    );
    assert!(text(
        &app,
        &format!("SELECT denial_message FROM conference_participants WHERE id = '{pid}'")
    )
    .await
    .is_some_and(|m| m.contains("Nobody who can let you in")));
}

/// M3: an ordinary meeting promotes its earliest present member to co-host instead.
#[tokio::test]
async fn unattended_waiting_room_promotes_a_present_member() {
    let app = some!(TestApp::new().await);
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "w", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY" }),
        )
        .await;
    app.add_member(sid, &owner, &member, Role::Participant)
        .await;
    let (s, v) = app.join(cid, Auth::Bearer(&member.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "ADMITTED");
    let member_pid = uuid(&v["participant"]["id"]);
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "Guest" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "WAITING");

    assert_eq!(tick(&app).await.waiting_rooms_attended, 1);
    exec(
        &app,
        &format!("UPDATE outbox_events SET created_at = created_at - interval '3 minutes' WHERE conference_id = '{cid}' AND event_type = 'conference.waiting_room_unattended'"),
    )
    .await;
    exec(
        &app,
        &format!("UPDATE conference_participants SET requested_at = requested_at - interval '4 minutes' WHERE conference_id = '{cid}' AND admission_state = 'WAITING'"),
    )
    .await;
    assert_eq!(tick(&app).await.waiting_rooms_attended, 1);
    assert_eq!(
        text(
            &app,
            &format!("SELECT role::text FROM conference_participants WHERE id = '{member_pid}'")
        )
        .await
        .as_deref(),
        Some("CO_HOST")
    );
    // The new co-host can admit: the room is attended again (and the guest still waits).
    assert_eq!(tick(&app).await.waiting_rooms_attended, 1);
    assert_eq!(
        events(&app, cid, "conference.waiting_room_attended").await,
        1
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id = '{cid}' AND admission_state = 'WAITING'")).await,
        1
    );
}

// ---------------------------------------------------------------------- M4 / M9

struct Notices {
    app: TestApp,
}

impl Notices {
    async fn new(in_app: bool) -> Option<Self> {
        let mut config = TestApp::notifications_config();
        config.notifications.in_app_delivery = in_app;
        let app = TestApp::with(TestOptions {
            config: Some(config),
            ..TestOptions::default()
        })
        .await?;
        let notifier = Notifier::new(app.state.email_service.clone(), Arc::new(NoPreferenceStore));
        Some(Self {
            app: app.with_notifier(Arc::new(notifier)),
        })
    }

    async fn set_mode(&self, category: MeetingCategory, mode: &str) {
        exec(
            &self.app,
            &format!(
                "UPDATE notification_routing_state SET mode = '{mode}', routing_version = routing_version + 1, \
                 changed_by_run = 'test', changed_at = now() WHERE category = '{}'",
                category.as_str()
            ),
        )
        .await;
    }

    /// Owner, account member and a conference in three days.
    async fn meeting(&self) -> (User, User, Uuid, Uuid) {
        let app = &self.app;
        let owner = app.user("owner").await;
        let member = app.user("member").await;
        let space = app
            .create_space(
                &owner,
                json!({ "title": "Quarterly review", "timezone": "Europe/London" }),
            )
            .await;
        let sid = uuid(&space["id"]);
        let start = Utc::now() + Duration::days(3);
        let (s, c) = app
            .post(
                format!("/api/v2/meeting-spaces/{sid}/conferences"),
                Auth::Bearer(&owner.token),
                json!({ "scheduledStartAt": rfc(start), "scheduledEndAt": rfc(start + Duration::minutes(30)) }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{c}");
        app.add_member(sid, &owner, &member, Role::Participant)
            .await;
        (owner, member, sid, uuid(&c["id"]))
    }

    async fn reschedule(&self, owner: &User, cid: Uuid) {
        let to = Utc::now() + Duration::days(4);
        let (s, v) = self
            .app
            .post(
                format!("/api/v2/conferences/{cid}/reschedule"),
                Auth::Bearer(&owner.token),
                json!({ "scheduledStartAt": rfc(to) }),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
}

/// M4: with the in-app path on, an OLD decision keeps today's legacy email job AND
/// publishes one delivered (non-shadow) event to the account recipients; off, SHADOW keeps
/// publishing its shadow copy only.
#[tokio::test]
async fn in_app_path_publishes_delivered_events_and_keeps_the_email() {
    for in_app in [true, false] {
        let h = some!(Notices::new(in_app).await);
        h.set_mode(MeetingCategory::MeetingNotification, "SHADOW")
            .await;
        let (owner, member, _, cid) = h.meeting().await;
        h.reschedule(&owner, cid).await;
        let app = &h.app;
        assert_eq!(
            app.scalar(&format!("SELECT count(*) FROM meeting_notifications WHERE conference_id = '{cid}' AND kind = 'CONFERENCE_RESCHEDULED'")).await,
            1,
            "the legacy email job is written either way"
        );
        assert_eq!(
            app.scalar("SELECT count(*) FROM meeting_notification_publications")
                .await,
            1,
            "one publication per decision"
        );
        let shadow = app
            .scalar("SELECT count(*) FROM meeting_notification_publications WHERE shadow")
            .await;
        assert_eq!(shadow, i64::from(!in_app));
        let recipients = text(
            app,
            "SELECT envelope -> 'recipients' ->> 0 FROM meeting_notification_publications",
        )
        .await
        .unwrap();
        assert!(recipients.contains(&member.id.to_string()), "{recipients}");
        // The email still goes out from the Meeting Service.
        let t = notify::tick(&NotificationContext::from_state(&app.state), Utc::now())
            .await
            .unwrap();
        let _ = t;
        assert!(
            app.scalar(
                "SELECT count(*) FROM email_deliveries WHERE category = 'MEETING_NOTIFICATION'"
            )
            .await
                >= 1
        );
    }
}

/// M9: invitations and reschedules carry METHOD:REQUEST with ORGANIZER / ATTENDEE, a
/// cancellation METHOD:CANCEL, all with the conference UID and a rising SEQUENCE.
#[tokio::test]
async fn calendar_invitations_are_itip_requests_and_cancellations() {
    let h = some!(Notices::new(false).await);
    let app = &h.app;
    let (owner, member, sid, cid) = h.meeting().await;
    let invitee = app.user("invitee").await;
    let (s, v) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            json!({ "inviteeEmail": invitee.email, "conferenceId": cid, "sendEmail": true }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let uid = format!("UID:conference-{}@cfmeet.com", cid.simple());
    let invite = text(
        app,
        "SELECT ics_body FROM email_deliveries WHERE category = 'MEETING_INVITATION'",
    )
    .await
    .unwrap();
    let unfolded = invite.replace("\r\n ", "");
    assert!(unfolded.contains("METHOD:REQUEST\r\n"), "{unfolded}");
    assert!(unfolded.contains(&uid));
    assert!(unfolded.contains("ORGANIZER;CN="));
    assert!(unfolded.contains(&format!("RSVP=TRUE:mailto:{}", invitee.email)));
    let seq = |ics: &str| -> i64 {
        ics.split("\r\n")
            .find_map(|l| l.strip_prefix("SEQUENCE:"))
            .unwrap()
            .parse()
            .unwrap()
    };
    let first = seq(&unfolded);

    h.reschedule(&owner, cid).await;
    notify::tick(&NotificationContext::from_state(&app.state), Utc::now())
        .await
        .unwrap();
    let update = text(
        app,
        &format!("SELECT ics_body FROM email_deliveries WHERE category = 'MEETING_NOTIFICATION' AND lower(recipient) = lower('{}')", member.email),
    )
    .await
    .expect("the reschedule notice carries the calendar update")
    .replace("\r\n ", "");
    assert!(update.contains("METHOD:REQUEST\r\n") && update.contains(&uid));
    let second = seq(&update);
    assert!(second > first, "{second} > {first}");

    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/cancel"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    notify::tick(&NotificationContext::from_state(&app.state), Utc::now())
        .await
        .unwrap();
    let cancel = text(
        app,
        "SELECT ics_body FROM email_deliveries WHERE ics_body LIKE '%METHOD:CANCEL%'",
    )
    .await
    .expect("the cancellation notice removes the calendar event")
    .replace("\r\n ", "");
    assert!(cancel.contains("STATUS:CANCELLED") && cancel.contains(&uid));
    assert!(seq(&cancel) > second);
}

// --------------------------------------------------------------------------- M10

/// M10: the join link is emailed once; a repeat with the same key returns the same token
/// without a second seat or email; a different request under the key is refused.
#[tokio::test]
async fn webinar_registration_repeats_return_the_same_link_and_email_it_once() {
    // The deployment secret the registration token is derived from (as in production).
    std::env::set_var("JWT_SECRET", "meetings-audit-test-secret-0123456789abcdef");
    let mut config: MeetingV2Config = TestApp::notifications_config();
    config.notifications.email_enabled = true;
    let app = some!(
        TestApp::with(TestOptions {
            config: Some(config),
            flags: TestApp::ghi_flags(),
            ..TestOptions::default()
        })
        .await
    );
    let owner = app.user("owner").await;
    let (sid, _) = app
        .live_conference(
            &owner,
            json!({ "title": "Webinar", "accessPolicy": "PUBLIC", "meetingType": "WEBINAR" }),
        )
        .await;
    let uri = format!("/api/v2/meeting-spaces/{sid}/registrations");
    let body =
        json!({ "displayName": "Eve", "email": "eve@example.com", "idempotencyKey": "reg-eve" });
    let (s, first) = app.post(&uri, Auth::None, body.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{first}");
    assert!(first["admissionToken"].is_string());
    assert_eq!(first["emailDelivery"], "ACCEPTED", "{first}");
    let (s, again) = app.post(&uri, Auth::None, body.clone()).await;
    assert_eq!(s, StatusCode::OK, "{again}");
    assert_eq!(
        again["admissionToken"], first["admissionToken"],
        "same link"
    );
    assert_eq!(again["registrationId"], first["registrationId"]);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM webinar_registrations WHERE meeting_space_id = '{sid}'"
        ))
        .await,
        1,
        "no second seat"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM email_deliveries WHERE category = 'MEETING_INVITATION' AND lower(recipient) = 'eve@example.com'").await,
        1,
        "emailed once"
    );
    let html = text(
        &app,
        "SELECT text_body FROM email_deliveries WHERE lower(recipient) = 'eve@example.com'",
    )
    .await
    .unwrap();
    assert!(html.contains(&format!(
        "/invite#{}",
        first["admissionToken"].as_str().unwrap()
    )));
    let (s, v) = app
        .post(
            &uri,
            Auth::None,
            json!({ "displayName": "Mallory", "email": "eve@example.com", "idempotencyKey": "reg-eve" }),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "IDEMPOTENCY_CONFLICT");
}

// --------------------------------------------------------------------------- MF2

/// MF2: a private, revocable calendar feed of the account's meetings.
#[tokio::test]
async fn calendar_feed_lists_my_meetings_and_is_revocable() {
    let app = some!(TestApp::new().await);
    let owner = app.user("owner").await;
    let (s, v) = app
        .post("/api/v2/me/calendar-feed", Auth::None, json!({}))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let (s, v) = app.get("/api/v2/me/calendar-feed", Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let (s, v) = app.delete("/api/v2/me/calendar-feed", Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let (s, v) = app
        .get("/api/v2/me/calendar-feed", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["active"], false);

    let space = app
        .create_space(&owner, json!({ "title": "Board review" }))
        .await;
    let sid = uuid(&space["id"]);
    let start = Utc::now() + Duration::days(2);
    let (s, c) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(start), "scheduledEndAt": rfc(start + Duration::hours(1)) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");

    let (s, feed) = app
        .post(
            "/api/v2/me/calendar-feed",
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{feed}");
    let path = feed["path"].as_str().unwrap().to_string();
    assert!(path.starts_with("/api/v2/calendar-feeds/calf_") && path.ends_with(".ics"));
    let (s, ics) = app.get(&path, Auth::None).await;
    assert_eq!(s, StatusCode::OK, "{ics}");
    let ics = ics["raw"].as_str().unwrap().replace("\r\n ", "");
    assert!(ics.starts_with("BEGIN:VCALENDAR") && ics.contains("METHOD:PUBLISH"));
    assert!(ics.contains("SUMMARY:Board review"), "{ics}");
    let (s, v) = app
        .get("/api/v2/me/calendar-feed", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["active"], true);
    assert!(v["token"].is_null(), "the URL is shown only once");
    assert!(v["lastUsedAt"].is_string());

    // A new link replaces the old one; deleting revokes it.
    let (s, renewed) = app
        .post(
            "/api/v2/me/calendar-feed",
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = app.get(&path, Auth::None).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "the replaced link stopped working"
    );
    let (s, v) = app
        .delete("/api/v2/me/calendar-feed", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["active"], false);
    let (s, _) = app.get(renewed["path"].as_str().unwrap(), Auth::None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .get("/api/v2/calendar-feeds/not-a-token.ics", Auth::None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
