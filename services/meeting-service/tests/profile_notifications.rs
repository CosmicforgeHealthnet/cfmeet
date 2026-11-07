//! Blueprint v1.0 Sections 9 and 11 — personal account profile (ACC-021, ACC-022,
//! DATA-016) and meeting / organization notifications (MTG-006, ACC-020).
//!
//! Every email goes to the in-process `RecordingMail` transport; no SMTP server, provider
//! or real address is ever contacted.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use backend::{
    domain::Role,
    services::notifications::{
        self as notify, NotificationContext, Notifier, RecordingMail, StaticPreferences,
    },
};
use chrono::{DateTime, Duration, Utc};
use common::{contract::*, *};
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

struct Harness {
    app: TestApp,
    mail: Arc<RecordingMail>,
    prefs: Arc<StaticPreferences>,
}

impl Harness {
    async fn new(enabled: bool) -> Option<Self> {
        let config = if enabled {
            TestApp::notifications_config()
        } else {
            backend::config::MeetingV2Config {
                passcode_hash_cost: 4,
                passcode_max_attempts: 5,
                ..Default::default()
            }
        };
        let app = TestApp::with(TestOptions {
            config: Some(config),
            ..TestOptions::default()
        })
        .await?;
        let mail = Arc::new(RecordingMail::default());
        let prefs = Arc::new(StaticPreferences::default());
        let app = app.with_notifier(Arc::new(Notifier::new(mail.clone(), prefs.clone())));
        Some(Self { app, mail, prefs })
    }

    fn ctx(&self) -> NotificationContext {
        NotificationContext::from_state(&self.app.state)
    }

    async fn tick(&self) -> notify::DeliveryTick {
        notify::tick(&self.ctx(), Utc::now()).await.unwrap()
    }
}

fn rfc(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn unique_email(tag: &str) -> String {
    format!(
        "{tag}-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..10]
    )
}

async fn profile(app: &TestApp, user: &User) -> Value {
    let (s, v) = app
        .get("/api/v2/me/profile", Auth::Bearer(&user.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_conforms(&openapi(), "AccountProfile", &v);
    v
}

async fn patch_profile(app: &TestApp, user: &User, body: Value) -> (StatusCode, Value) {
    app.patch("/api/v2/me/profile", Auth::Bearer(&user.token), body)
        .await
}

async fn scheduled(
    app: &TestApp,
    owner: &User,
    title: &str,
    start: DateTime<Utc>,
) -> (Uuid, Value) {
    let space = app
        .create_space(
            owner,
            json!({ "title": title, "timezone": "Europe/London" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    let (s, c) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(start), "scheduledEndAt": rfc(start + Duration::minutes(30)) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    (sid, c)
}

async fn invite(app: &TestApp, owner: &User, sid: Uuid, body: Value) -> (StatusCode, Value) {
    app.post(
        format!("/api/v2/meeting-spaces/{sid}/invitations"),
        Auth::Bearer(&owner.token),
        body,
    )
    .await
}

// ---------------------------------------------------------------------------
// Section 9 — profile
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s9_profile_reads_updates_clears_and_validates() {
    let app = some!(TestApp::new().await);
    let ada = app.user("ada").await;
    let other = app.user("other").await;

    let p = profile(&app, &ada).await;
    assert_eq!(p["userId"], json!(ada.id));
    assert_eq!(p["displayName"], Value::Null);
    assert_eq!(p["effectiveDisplayName"], p["username"]);
    assert_eq!(p["status"], "active", "canonical status on v2");
    assert_eq!(p["phoneVerification"]["status"], "PROVIDER_BLOCKED");
    assert_eq!(p["phoneVerification"]["required"], false);

    let (s, v) = patch_profile(
        &app,
        &ada,
        json!({ "displayName": "  Ada Lovelace ", "timezone": "Africa/Lagos", "locale": "en_gb",
                "avatarUrl": "https://cdn.example.com/a.png" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_conforms(&openapi(), "AccountProfile", &v);
    assert_eq!(v["displayName"], "Ada Lovelace");
    assert_eq!(v["effectiveDisplayName"], "Ada Lovelace");
    assert_eq!(v["timezone"], "Africa/Lagos");
    assert_eq!(v["locale"], "en-GB");
    assert_eq!(v["avatarUrl"], "https://cdn.example.com/a.png");
    assert!(v["profileUpdatedAt"].is_string());
    assert_eq!(profile(&app, &ada).await["displayName"], "Ada Lovelace");

    // null clears; omitted fields stay.
    let (s, v) = patch_profile(&app, &ada, json!({ "avatarUrl": null })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["avatarUrl"], Value::Null);
    assert_eq!(v["timezone"], "Africa/Lagos");

    // One validation rule set (VALIDATION_ERROR, nothing written).
    for bad in [
        json!({}),
        json!({ "displayName": "   " }),
        json!({ "displayName": "evil\u{202E}name" }),
        json!({ "displayName": "x".repeat(65) }),
        json!({ "avatarUrl": "http://cdn.example.com/a.png" }),
        json!({ "avatarUrl": "https://user@evil.example/a.png" }),
        json!({ "timezone": "Mars/Olympus" }),
        json!({ "locale": "english" }),
        json!({ "username": "  ab " }),
        json!({ "username": "bad name" }),
        json!({ "username": null }),
        json!({ "email": "new@example.com" }),
    ] {
        let (s, v) = patch_profile(&app, &ada, bad.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {v}");
        assert_eq!(code(&v), "VALIDATION_ERROR", "{bad}");
    }
    assert_eq!(profile(&app, &ada).await["displayName"], "Ada Lovelace");

    // Username: unique, trimmed, same rule as registration.
    let taken = profile(&app, &other).await["username"].clone();
    let (s, v) = patch_profile(&app, &ada, json!({ "username": taken })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "USERNAME_TAKEN"));
    let fresh = format!("ada_{}", &Uuid::new_v4().simple().to_string()[..8]);
    let (s, v) = patch_profile(&app, &ada, json!({ "username": format!("  {fresh} ") })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["username"], fresh);

    // Audit trail records the field names only.
    assert!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{}' AND event_type = 'PROFILE_UPDATED' \
             AND detail::text NOT LIKE '%Lovelace%'",
            ada.id
        ))
        .await
            >= 3
    );

    // Signed-out callers.
    let (s, v) = app.get("/api/v2/me/profile", Auth::None).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
    );
    let (s, _) = app
        .patch(
            "/api/v2/me/profile",
            Auth::None,
            json!({ "displayName": "x" }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn s9_v1_profile_uses_the_registration_rule_and_one_status_rendering() {
    let app = some!(TestApp::new().await);
    let u = app.user("legacy").await;
    let (s, v) = app
        .patch(
            "/api/v1/users/me",
            Auth::Bearer(&u.token),
            json!({ "username": "  ab " }),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "length is checked after trimming: {v}"
    );
    let (s, _) = app
        .patch(
            "/api/v1/users/me",
            Auth::Bearer(&u.token),
            json!({ "username": "bell\u{0007}name" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "control characters are refused");
    let name = format!("legacy_{}", &Uuid::new_v4().simple().to_string()[..8]);
    let (s, v) = app
        .patch(
            "/api/v1/users/me",
            Auth::Bearer(&u.token),
            json!({ "username": format!(" {name} ") }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["username"], name);
    assert_eq!(v["status"], "active");
    let (s, v) = app.get("/api/v1/users/me", Auth::Bearer(&u.token)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["username"], name);
}

#[tokio::test]
async fn s9_display_name_is_what_other_people_see() {
    let app = some!(TestApp::new().await);
    let host = app.user("host").await;
    let (s, _) = patch_profile(&app, &host, json!({ "displayName": "Dr Ada" })).await;
    assert_eq!(s, StatusCode::OK);
    let space = app
        .create_space(
            &host,
            json!({ "title": "Clinic prep", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    let (s, c) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&host.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let cid = uuid(&c["id"]);
    let (s, v) = app
        .get(
            format!("/api/v2/conferences/{cid}/pre-join"),
            Auth::Bearer(&host.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["organizer"]["displayName"], "Dr Ada", "{v}");
}

// ---------------------------------------------------------------------------
// Section 11 — invitation emails
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s11_meeting_invitation_email_only_on_request_with_the_token_in_the_fragment() {
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    let owner = app.user("owner").await;
    let (sid, _) = scheduled(
        app,
        &owner,
        "Quarterly review",
        Utc::now() + Duration::days(2),
    )
    .await;
    let guest = unique_email("guest");

    // Not requested: no email, no emailDelivery field.
    let (s, v) = invite(app, &owner, sid, json!({ "inviteeEmail": guest })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert!(v.get("emailDelivery").is_none());
    assert!(h.mail.sent().is_empty());

    // Requested.
    let (s, v) = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": guest, "sendEmail": true }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_conforms(&openapi(), "CreateInvitationResponse", &v);
    assert_eq!(v["emailDelivery"], "ACCEPTED");
    let sent = h.mail.sent();
    assert_eq!(sent.len(), 1);
    let token = v["token"].as_str().unwrap();
    assert_eq!(sent[0].to, guest);
    assert!(sent[0].subject.contains("Quarterly review"));
    assert!(
        sent[0].text.contains(&format!("/invite#{token}")),
        "{}",
        sent[0].text
    );
    assert!(sent[0].html.contains(&format!("/invite#{token}")));

    // sendEmail needs an address.
    let (s, v) = invite(app, &owner, sid, json!({ "sendEmail": true })).await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));

    // An account that turned meeting notifications off gets nothing; the inviter sees the
    // same answer (no account or preference disclosure).
    let quiet = app.user_with_email("quiet", &unique_email("quiet")).await;
    h.prefs.opt_out(quiet.id);
    let (s, v) = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": quiet.email, "sendEmail": true }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["emailDelivery"], "ACCEPTED");
    assert_eq!(h.mail.sent().len(), 1);

    // Unknown preferences or a queue outage: the invitation exists, the email is FAILED.
    let known = app.user_with_email("known", &unique_email("known")).await;
    h.prefs.set_unavailable(true);
    let (s, v) = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": known.email, "sendEmail": true }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["emailDelivery"], "FAILED");
    h.prefs.set_unavailable(false);
    h.mail.set_failing(true);
    let (s, v) = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": unique_email("x"), "sendEmail": true }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["emailDelivery"], "FAILED");
    h.mail.set_failing(false);
    assert_eq!(h.mail.sent().len(), 1);
}

#[tokio::test]
async fn s11_invitation_email_is_disabled_by_default_and_never_queued() {
    let h = some!(Harness::new(false).await);
    let app = &h.app;
    let owner = app.user("owner").await;
    let (sid, c) = scheduled(
        app,
        &owner,
        "Off by default",
        Utc::now() + Duration::days(1),
    )
    .await;
    let (s, v) = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": unique_email("g"), "sendEmail": true }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["emailDelivery"], "DISABLED");
    // Changes and reminders produce no jobs either.
    let cid = uuid(&c["id"]);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(Utc::now() + Duration::minutes(10)) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(h.tick().await, notify::DeliveryTick::default());
    assert_eq!(
        app.scalar("SELECT count(*) FROM meeting_notifications")
            .await,
        0
    );
    assert!(h.mail.sent().is_empty());
}

// ---------------------------------------------------------------------------
// Section 11 — reschedule / cancellation notices and reminders
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s11_reschedule_and_cancel_notices_are_durable_once_and_exclude_the_actor() {
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    let owner = app.user("owner").await;
    let member = app.user_with_email("member", &unique_email("member")).await;
    let (s, _) = patch_profile(
        app,
        &member,
        json!({ "timezone": "Africa/Lagos", "displayName": "Bola" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let start = Utc::now() + Duration::days(3);
    let (sid, c) = scheduled(app, &owner, "Board <sync>", start).await;
    let cid = uuid(&c["id"]);
    app.add_member(sid, &owner, &member, Role::Participant)
        .await;
    let external = unique_email("external");
    let (s, _) = invite(app, &owner, sid, json!({ "inviteeEmail": external })).await;
    assert_eq!(s, StatusCode::CREATED);

    let moved = start + Duration::hours(2);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(moved) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_notifications WHERE conference_id = '{cid}' AND kind = 'CONFERENCE_RESCHEDULED' AND status = 'PENDING'"
        ))
        .await,
        1,
        "written with the change"
    );
    let t = h.tick().await;
    assert_eq!(t.sent, 1, "{t:?}");
    let sent = h.mail.sent();
    let mut to: Vec<_> = sent.iter().map(|e| e.to.clone()).collect();
    to.sort();
    let mut expected = vec![member.email.to_ascii_lowercase(), external.clone()];
    expected.sort();
    assert_eq!(to, expected, "owner made the change and is not notified");
    let mine = sent
        .iter()
        .find(|e| e.to == member.email.to_ascii_lowercase())
        .unwrap();
    assert!(mine.subject.starts_with("Rescheduled: Board <sync>"));
    assert!(
        mine.text.contains("(Africa/Lagos)"),
        "recipient zone: {}",
        mine.text
    );
    assert!(mine.text.contains("Hi Bola"));
    assert!(!mine.html.contains("Board <sync>"), "escaped in HTML");
    let theirs = sent.iter().find(|e| e.to == external).unwrap();
    assert!(
        theirs.text.contains("(Europe/London)"),
        "meeting zone: {}",
        theirs.text
    );

    // A second pass sends nothing more.
    assert_eq!(h.tick().await.sent, 0);
    assert_eq!(h.mail.sent().len(), 2);

    // Cancellation (by the owner again: everyone else is told).
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/cancel"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let t = h.tick().await;
    assert_eq!(t.sent, 1, "{t:?}");
    let cancelled: Vec<_> = h
        .mail
        .sent()
        .into_iter()
        .filter(|e| e.subject.starts_with("Cancelled:"))
        .collect();
    assert_eq!(cancelled.len(), 2);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_notification_recipients r JOIN meeting_notifications n ON n.id = r.notification_id \
              WHERE n.conference_id = '{cid}' AND r.outcome = 'QUEUED' AND length(r.recipient_digest) = 64"
        ))
        .await,
        4,
        "ledger holds digests only"
    );
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM meeting_notification_recipients WHERE recipient_digest LIKE '%@%'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn s11_protected_meeting_titles_never_reach_an_email() {
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    let owner = app.user("clinician").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "Patient: J. Smith follow-up", "securityProfile": "HEALTHCARE", "accessPolicy": "INVITE_ONLY" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    let start = Utc::now() + Duration::days(1);
    let (s, c) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(start) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let (s, v) = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": unique_email("p"), "sendEmail": true }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let cid = uuid(&c["id"]);
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(start + Duration::hours(1)) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    h.tick().await;
    let sent = h.mail.sent();
    assert_eq!(sent.len(), 2, "invitation + reschedule notice");
    for e in sent {
        assert!(
            !e.subject.contains("Smith") && !e.text.contains("Smith") && !e.html.contains("Smith")
        );
        assert!(e.subject.contains("CF Meet meeting"));
    }
}

#[tokio::test]
async fn s11_reminders_once_per_start_respect_preferences_and_retry_on_unknown_preferences() {
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    let owner = app.user("owner").await;
    let member = app.user_with_email("member", &unique_email("member")).await;
    let quiet = app.user_with_email("quiet", &unique_email("quiet")).await;
    h.prefs.opt_out(quiet.id);
    let (sid, c) = scheduled(app, &owner, "Standup", Utc::now() + Duration::minutes(10)).await;
    let cid = uuid(&c["id"]);
    app.add_member(sid, &owner, &member, Role::Participant)
        .await;
    app.add_member(sid, &owner, &quiet, Role::Participant).await;
    // Too far ahead for a reminder.
    let (_, far) = scheduled(app, &owner, "Later", Utc::now() + Duration::hours(5)).await;

    let t = h.tick().await;
    assert_eq!(t.reminders_created, 1, "{t:?}");
    assert_eq!(t.sent, 1, "{t:?}");
    let reminders: Vec<_> = h
        .mail
        .sent()
        .into_iter()
        .filter(|e| e.subject.starts_with("Reminder: Standup"))
        .collect();
    let mut to: Vec<_> = reminders.iter().map(|e| e.to.clone()).collect();
    to.sort();
    let mut expected = vec![
        owner.email.to_ascii_lowercase(),
        member.email.to_ascii_lowercase(),
    ];
    expected.sort();
    assert_eq!(to, expected, "opted-out member suppressed");
    assert!(reminders[0].text.contains("Join meeting:"));
    assert_eq!(
        app.scalar(&format!(
            "SELECT recipients_suppressed::bigint FROM meeting_notifications WHERE conference_id = '{cid}' AND kind = 'CONFERENCE_REMINDER'"
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_notifications WHERE conference_id = '{}'",
            uuid(&far["id"])
        ))
        .await,
        0
    );

    // Repeated and concurrent passes never duplicate the reminder.
    let ctx = h.ctx();
    let now = Utc::now();
    let (a, b) = tokio::join!(notify::tick(&ctx, now), notify::tick(&ctx, now));
    assert_eq!(
        a.unwrap().reminders_created + b.unwrap().reminders_created,
        0
    );
    assert_eq!(h.mail.sent().len(), 2);

    // A new start gets a new reminder; with preferences unknown the job waits (retry).
    h.prefs.set_unavailable(true);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(Utc::now() + Duration::minutes(12)) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let t = h.tick().await;
    assert_eq!(t.reminders_created, 1, "{t:?}");
    assert_eq!(t.sent, 0, "{t:?}");
    assert!(t.retried >= 1, "{t:?}");
    assert_eq!(
        h.mail.sent().len(),
        2,
        "nothing sent against unknown preferences"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_notifications WHERE conference_id = '{cid}' AND status = 'PENDING' \
               AND last_error = 'PREFERENCES_UNAVAILABLE'"
        ))
        .await,
        2,
        "reschedule notice and new reminder wait"
    );
    // Preferences back: the retry delivers once its backoff elapsed.
    h.prefs.set_unavailable(false);
    sea_orm::ConnectionTrait::execute_unprepared(
        &app.db.conn,
        "UPDATE meeting_notifications SET available_at = now() WHERE status = 'PENDING'",
    )
    .await
    .unwrap();
    let t = h.tick().await;
    assert_eq!(t.sent, 2, "{t:?}");
    // Cancelled before it starts: a stale reminder is skipped, never sent.
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/cancel"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    sea_orm::ConnectionTrait::execute_unprepared(
        &app.db.conn,
        &format!(
            "INSERT INTO meeting_notifications (id, kind, meeting_space_id, conference_id, dedupe_key, payload) \
             VALUES ('{}', 'CONFERENCE_REMINDER', '{sid}', '{cid}', 'reminder:{cid}:stale', '{{\"scheduledStartAt\":\"2030-01-01T00:00:00Z\"}}')",
            Uuid::new_v4()
        ),
    )
    .await
    .unwrap();
    let before = h.mail.sent().len();
    let t = h.tick().await;
    assert!(t.skipped >= 1, "{t:?}");
    assert_eq!(
        h.mail
            .sent()
            .iter()
            .skip(before)
            .filter(|e| e.subject.starts_with("Reminder"))
            .count(),
        0
    );
}

#[tokio::test]
async fn s11_a_queue_outage_retries_without_emailing_anyone_twice() {
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    let owner = app.user("owner").await;
    let start = Utc::now() + Duration::days(2);
    let (sid, c) = scheduled(app, &owner, "Retry me", start).await;
    let cid = uuid(&c["id"]);
    for i in 0..3 {
        let (s, _) = invite(
            app,
            &owner,
            sid,
            json!({ "inviteeEmail": unique_email(&format!("r{i}")) }),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
    }
    h.mail.set_failing(true);
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(start + Duration::hours(1)) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let t = h.tick().await;
    assert_eq!((t.sent, t.retried), (0, 1), "{t:?}");
    assert_eq!(
        app.scalar("SELECT count(*) FROM meeting_notification_recipients")
            .await,
        0,
        "nothing recorded as queued when the queue refused it"
    );
    h.mail.set_failing(false);
    sea_orm::ConnectionTrait::execute_unprepared(
        &app.db.conn,
        "UPDATE meeting_notifications SET available_at = now() WHERE status = 'PENDING'",
    )
    .await
    .unwrap();
    assert_eq!(h.tick().await.sent, 1);
    assert_eq!(h.mail.sent().len(), 3);
    assert_eq!(h.tick().await.sent, 0);
    assert_eq!(h.mail.sent().len(), 3);
}
