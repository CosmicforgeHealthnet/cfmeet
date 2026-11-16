//! Document 4 §3.4 — the Notification Service owner APIs served by Meeting / Account:
//! X1 `POST /internal/v1/accounts/notification-contacts/resolve` (`account-contacts.read`)
//! and X2 `POST /internal/v1/conferences/{conferenceId}/notification-recipients/resolve`
//! (`meeting-notification-recipients.read`).
//!
//! Only `cfmeet.notifications` holding the permission in this receiver's trust bundle is
//! answered (no token 401, another peer or the other permission 403); batches are bounded
//! and strict; X1 maps the Account user and its ADR 0060 devices, and unknown ids are
//! `UNKNOWN`; X2 answers exactly the recipients and presented title the Meeting
//! notification worker uses, and whether the notice is still true.

mod common;

use std::{collections::BTreeSet, sync::Arc};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use backend::{
    domain::Role,
    services::{
        notification_owner as owner,
        notifications::{
            self as notify, NotificationContext, Notifier, RecordingMail, StaticPreferences,
        },
    },
};
use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Duration, Utc};
use common::*;
use migration::MigratorTrait;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const CONTACTS: &str = "/internal/v1/accounts/notification-contacts/resolve";

fn signer(id: ServiceId, kid: &str) -> (ServiceTokenSigner, ServiceSigningKey) {
    let key = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    let public =
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse(kid).unwrap(), &key.to_pkcs8_pem()).unwrap();
    (
        ServiceTokenSigner::new(
            id,
            RuntimeEnvironment::Test,
            key,
            TokenTtl::default(),
            Arc::new(SystemClock),
        ),
        public,
    )
}

/// Receiver trust bundle: the Notification Service with `notifications`, and the
/// Developer Platform with an unrelated permission.
fn verifier(
    notifications: &ServiceSigningKey,
    notification_permissions: &[&str],
    peer: &ServiceSigningKey,
) -> Arc<ServiceTokenVerifier> {
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str]| {
        json!({ "issuer": id, "audience": "cfmeet.meeting-service", "runtime_environment": "test",
                "keys": [k.public_jwk()], "permissions": perms })
    };
    let bundle = json!({ "bundle_version": 1, "receiver": "cfmeet.meeting-service", "runtime_environment": "test",
    "trusted_issuers": [
        issuer("cfmeet.notifications", notifications, notification_permissions),
        issuer("cfmeet.developer-platform", peer, &["user-session.verify"]),
    ]});
    let policy = TrustPolicy::from_json(
        &bundle.to_string(),
        &ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
    )
    .unwrap();
    Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)))
}

struct Owner {
    app: TestApp,
    /// Trusts the Notification Service with both owner permissions.
    router: Router,
    /// Trusts the Notification Service with `account-contacts.read` only.
    x1_only: Router,
    notifications: ServiceTokenSigner,
    peer: ServiceTokenSigner,
    mail: Arc<RecordingMail>,
}

impl Owner {
    async fn start() -> Option<Self> {
        Self::on(TestApp::new().await?)
    }

    /// With Section 11 notification email on (the worker the X2 answer must agree with).
    async fn with_notifications() -> Option<Self> {
        let app = TestApp::with(TestOptions {
            config: Some(TestApp::notifications_config()),
            ..TestOptions::default()
        })
        .await?;
        Self::on(app)
    }

    fn on(app: TestApp) -> Option<Self> {
        let mail = Arc::new(RecordingMail::default());
        let app = app.with_notifier(Arc::new(Notifier::new(
            mail.clone(),
            Arc::new(StaticPreferences::default()),
        )));
        let (notifications, n_pub) = signer(ServiceId::notifications(), "notifications-test-1");
        let (peer, p_pub) = signer(ServiceId::developer_platform(), "developer-platform-test-1");
        let router = owner::router(
            app.state.clone(),
            verifier(
                &n_pub,
                &[
                    "account-contacts.read",
                    "meeting-notification-recipients.read",
                ],
                &p_pub,
            ),
        );
        let x1_only = owner::router(
            app.state.clone(),
            verifier(&n_pub, &["account-contacts.read"], &p_pub),
        );
        Some(Self {
            app,
            router,
            x1_only,
            notifications,
            peer,
            mail,
        })
    }
}

/// The signed headers of one request-bound token.
fn signed(signer: &ServiceTokenSigner, path: &str) -> Vec<(String, String)> {
    signer
        .sign_request(
            &OutboundServiceRequest::new_operation(ServiceId::meeting_service()),
            &Method::POST,
            path,
        )
        .unwrap()
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
        .collect()
}

async fn post_with(
    router: &Router,
    headers: &[(String, String)],
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    for (k, v) in headers {
        b = b.header(k, v);
    }
    let r = router
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = r.status();
    assert_eq!(
        r.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn post(
    router: &Router,
    signer: Option<&ServiceTokenSigner>,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let headers = signer.map(|s| signed(s, path)).unwrap_or_default();
    post_with(router, &headers, path, body).await
}

fn account_ref(id: Uuid) -> String {
    format!("cfmeet-user:{id}")
}

async fn exec(app: &TestApp, sql: &str, values: Vec<sea_orm::Value>) {
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await
        .unwrap();
}

fn hex64() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// An ADR 0060 account device in `state` (REVOKED rows carry their revocation).
async fn device(app: &TestApp, user: Uuid, class: &str, state: &str, trust: &str) -> Uuid {
    let id = Uuid::new_v4();
    let platform = if class == "WEB" { "WEB" } else { "IOS" };
    let trusted = trust == "TRUSTED";
    let revoked = state == "REVOKED";
    exec(
        app,
        "INSERT INTO account_devices (id, user_id, installation_digest, device_class, platform, public_key, \
            key_thumbprint, trust, trusted_via, trusted_at, state, revoked_at, revoke_reason) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, \
            CASE WHEN $9 THEN 'BOOTSTRAP' END, CASE WHEN $9 THEN now() END, $10, \
            CASE WHEN $11 THEN now() END, CASE WHEN $11 THEN 'USER_REVOKED' END)",
        vec![
            id.into(),
            user.into(),
            hex64().into(),
            class.into(),
            platform.into(),
            vec![7u8; 32].into(),
            hex64().into(),
            trust.into(),
            trusted.into(),
            state.into(),
            revoked.into(),
        ],
    )
    .await;
    id
}

#[tokio::test]
async fn x1_answers_only_the_notification_service() {
    let Some(o) = Owner::start().await else {
        return;
    };
    assert_eq!(owner::CONTACTS_PATH, CONTACTS);
    let alice = o.app.user("alice").await;
    let body = json!({ "accountRefs": [account_ref(alice.id)] });

    // No service token.
    let (s, v) = post(&o.router, None, CONTACTS, &body).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
    assert!(v["error"]["requestId"].is_string());
    // A trusted peer without the permission.
    let (s, v) = post(&o.router, Some(&o.peer), CONTACTS, &body).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "FORBIDDEN");
    // The Notification Service.
    let headers = signed(&o.notifications, CONTACTS);
    let (s, v) = post_with(&o.router, &headers, CONTACTS, &body).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["results"][0]["accountRef"], account_ref(alice.id));
    assert_eq!(v["results"][0]["status"], "ACTIVE");
    assert_eq!(v["results"][0]["email"], alice.email.as_str());
    // A replayed token is single-use (POST).
    let (s, _) = post_with(&o.router, &headers, CONTACTS, &body).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "replay refused");
}

#[tokio::test]
async fn x1_batches_are_bounded_and_strict() {
    let Some(o) = Owner::start().await else {
        return;
    };
    let n = Some(&o.notifications);
    let id = Uuid::new_v4();
    for (body, why) in [
        (json!({ "accountRefs": [] }), "empty"),
        (
            json!({ "accountRefs": (0..101).map(|_| account_ref(Uuid::new_v4())).collect::<Vec<_>>() }),
            "101 refs",
        ),
        (
            json!({ "accountRefs": [account_ref(id), account_ref(id)] }),
            "duplicate",
        ),
        (
            json!({ "accountRefs": [format!("cfmeet-user:{}", id.to_string().to_uppercase())] }),
            "non-canonical",
        ),
        (
            json!({ "accountRefs": [format!("meeting-email-principal:{id}")] }),
            "not an account",
        ),
        (
            json!({ "accountRefs": [account_ref(id)], "extra": true }),
            "unknown field",
        ),
        (json!({}), "missing field"),
    ] {
        let (s, v) = post(&o.router, n, CONTACTS, &body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{why}: {v}");
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR", "{why}");
    }
    // Exactly 100 is accepted, answered in request order.
    let refs: Vec<String> = (0..100).map(|_| account_ref(Uuid::new_v4())).collect();
    let (s, v) = post(&o.router, n, CONTACTS, &json!({ "accountRefs": refs })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let got: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["accountRef"].as_str().unwrap())
        .collect();
    assert_eq!(got, refs.iter().map(String::as_str).collect::<Vec<_>>());
}

#[tokio::test]
async fn x1_maps_accounts_devices_and_unknown_ids() {
    let Some(o) = Owner::start().await else {
        return;
    };
    let app = &o.app;
    // The `suspended` account status belongs to the Account communications history.
    migration::CommunicationsMigrator::up(&app.db.conn, None)
        .await
        .expect("communications migrations apply");
    let ada = app.user("ada").await;
    exec(
        app,
        "UPDATE users SET display_name = 'Ada Lovelace', locale = 'en-GB', timezone = 'Europe/London' WHERE id = $1",
        vec![ada.id.into()],
    )
    .await;
    let web = device(app, ada.id, "WEB", "ACTIVE", "UNTRUSTED").await;
    let revoked = device(app, ada.id, "WEB", "REVOKED", "TRUSTED").await;
    let pending = device(app, ada.id, "MOBILE", "REPLACEMENT_PENDING", "UNTRUSTED").await;

    let bo = app.user("bo").await; // no display name: the username is shown
    let gone = app.user("gone").await;
    exec(
        app,
        "UPDATE users SET status = 'inactive' WHERE id = $1",
        vec![gone.id.into()],
    )
    .await;
    let held = app.user("held").await;
    exec(
        app,
        "UPDATE users SET status = 'suspended' WHERE id = $1",
        vec![held.id.into()],
    )
    .await;
    let fresh = app.user("fresh").await;
    exec(
        app,
        "UPDATE users SET status = 'pending_verification' WHERE id = $1",
        vec![fresh.id.into()],
    )
    .await;
    let guest = app.user("guest").await;
    exec(
        app,
        "UPDATE users SET role = 'guest' WHERE id = $1",
        vec![guest.id.into()],
    )
    .await;
    let stranger = Uuid::new_v4();

    let ids = [
        ada.id, bo.id, gone.id, held.id, fresh.id, guest.id, stranger,
    ];
    let (s, v) = post(
        &o.router,
        Some(&o.notifications),
        CONTACTS,
        &json!({ "accountRefs": ids.iter().map(|i| account_ref(*i)).collect::<Vec<_>>() }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let r = v["results"].as_array().unwrap();
    assert_eq!(r.len(), ids.len());

    assert_eq!(
        r[0],
        json!({
            "accountRef": account_ref(ada.id), "status": "ACTIVE", "email": ada.email,
            "displayName": "Ada Lovelace", "locale": "en-GB", "timeZone": "Europe/London",
            "devices": r[0]["devices"].clone(),
        })
    );
    let devices = r[0]["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 3);
    let state_of = |id: Uuid| {
        devices
            .iter()
            .find(|d| d["accountDeviceRef"] == id.to_string())
            .map(|d| d["state"].as_str().unwrap().to_string())
    };
    assert_eq!(state_of(web).as_deref(), Some("ACTIVE"));
    assert_eq!(state_of(pending).as_deref(), Some("PENDING"));
    assert_eq!(state_of(revoked).as_deref(), Some("REVOKED"));
    assert_eq!(devices[2]["state"], "REVOKED", "live devices first");

    assert_eq!(r[1]["status"], "ACTIVE");
    assert!(r[1]["displayName"].as_str().unwrap().starts_with("bo-"));
    assert_eq!(r[1]["locale"], Value::Null);
    assert_eq!(r[1]["devices"], json!([]));
    // Deactivated and suspended accounts keep their contact data (the Notification
    // Service decides; a security notice may still be sent to the address).
    assert_eq!(r[2]["status"], "DEACTIVATED");
    assert_eq!(r[2]["email"], gone.email.as_str());
    assert_eq!(r[3]["status"], "SUSPENDED");
    assert_eq!(
        r[4]["status"], "ACTIVE",
        "pending verification is in good standing"
    );
    // A guest identity and an unknown id are UNKNOWN, with nulls.
    for (i, id) in [(5, guest.id), (6, stranger)] {
        assert_eq!(
            r[i],
            json!({ "accountRef": account_ref(id), "status": "UNKNOWN", "email": null,
                    "displayName": null, "locale": null, "timeZone": null, "devices": [] })
        );
    }

    // Addresses only for the Notification Service caller.
    let without = owner::account_contacts(&app.db.conn, &[ada.id], false)
        .await
        .unwrap();
    assert_eq!(without[0].email, None);
    assert_eq!(without[0].display_name.as_deref(), Some("Ada Lovelace"));
}

// ---------------------------------------------------------------------------
// X2 resolveConferenceRecipients
// ---------------------------------------------------------------------------

fn recipients_path(conference: Uuid) -> String {
    format!("/internal/v1/conferences/{conference}/notification-recipients/resolve")
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

/// A scheduled conference of a new space (`space` overrides the space fields).
async fn scheduled(
    app: &TestApp,
    owner: &User,
    space: Value,
    start: DateTime<Utc>,
) -> (Value, Uuid) {
    let space = app.create_space(owner, space).await;
    let sid = uuid(&space["id"]);
    let (s, c) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(start), "scheduledEndAt": rfc(start + Duration::minutes(30)) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    (space, uuid(&c["id"]))
}

async fn invite(app: &TestApp, owner: &User, sid: Uuid, email: &str) {
    let (s, v) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            json!({ "inviteeEmail": email }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

async fn x2(o: &Owner, conference: Uuid, body: Value) -> (StatusCode, Value) {
    post(
        &o.router,
        Some(&o.notifications),
        &recipients_path(conference),
        &body,
    )
    .await
}

fn refs(v: &Value) -> BTreeSet<String> {
    v["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ref"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn x2_answers_only_the_notification_service_with_its_permission() {
    let Some(o) = Owner::start().await else {
        return;
    };
    let owner_user = o.app.user("owner").await;
    let (_, cid) = scheduled(
        &o.app,
        &owner_user,
        json!({ "title": "Weekly" }),
        Utc::now() + Duration::days(2),
    )
    .await;
    let path = recipients_path(cid);
    assert_eq!(
        path,
        format!("/internal/v1/conferences/{cid}/notification-recipients/resolve")
    );
    let body = json!({ "kind": "SCHEDULED" });
    let (s, v) = post(&o.router, None, &path, &body).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
    let (s, v) = post(&o.router, Some(&o.peer), &path, &body).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    // The Notification Service holding only X1's permission.
    let (s, v) = post(&o.x1_only, Some(&o.notifications), &path, &body).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "FORBIDDEN");
    // That receiver still answers X1.
    let (s, _) = post(
        &o.x1_only,
        Some(&o.notifications),
        "/internal/v1/accounts/notification-contacts/resolve",
        &json!({ "accountRefs": [account_ref(owner_user.id)] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = x2(&o, cid, body.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["conferenceId"], cid.to_string());
    assert_eq!(v["stillTrue"], true);

    // Unknown conference, malformed id and malformed bodies.
    let (s, v) = x2(&o, Uuid::new_v4(), body.clone()).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "NOT_FOUND");
    let (s, v) = post(
        &o.router,
        Some(&o.notifications),
        "/internal/v1/conferences/not-a-uuid/notification-recipients/resolve",
        &body,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    for bad in [
        json!({ "kind": "LATER" }),
        json!({}),
        json!({ "kind": "REMINDER", "startsAt": "tomorrow" }),
        json!({ "kind": "REMINDER", "actor": "x" }),
    ] {
        let (s, v) = x2(&o, cid, bad.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {v}");
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR");
    }
}

/// X2 and the Meeting notification worker resolve the same people: a reschedule notice the
/// worker emails (everyone but the owner who made the change) is exactly X2's recipients
/// (accounts by reference, addressed through X1; email-only participants with their
/// address) minus the actor, whom only the producer's event names.
#[tokio::test]
async fn x2_recipients_match_the_notification_worker() {
    let Some(o) = Owner::with_notifications().await else {
        return;
    };
    let app = &o.app;
    let owner_user = app.user("owner").await;
    let member = app.user_with_email("member", &unique_email("member")).await;
    let invited_account = app.user_with_email("acct", &unique_email("acct")).await;
    let gone = app.user_with_email("gone", &unique_email("gone")).await;
    let start = Utc::now() + Duration::days(3);
    let (space, cid) = scheduled(
        app,
        &owner_user,
        json!({ "title": "Board <sync>", "timezone": "Europe/London" }),
        start,
    )
    .await;
    let sid = uuid(&space["id"]);
    app.add_member(sid, &owner_user, &member, Role::Participant)
        .await;
    app.add_member(sid, &owner_user, &gone, Role::Participant)
        .await;
    exec(
        app,
        "UPDATE users SET status = 'inactive' WHERE id = $1",
        vec![gone.id.into()],
    )
    .await;
    let external = unique_email("External");
    invite(app, &owner_user, sid, &external).await;
    invite(app, &owner_user, sid, &invited_account.email).await;

    let moved = start + Duration::hours(2);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&owner_user.token),
            json!({ "scheduledStartAt": rfc(moved) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ctx = NotificationContext::from_state(&app.state);
    let t = notify::tick(&ctx, Utc::now()).await.unwrap();
    assert_eq!(t.sent, 1, "{t:?}");
    let worker: BTreeSet<String> = o.mail.sent().into_iter().map(|e| e.to).collect();

    let (s, v) = x2(
        &o,
        cid,
        json!({ "kind": "RESCHEDULED", "startsAt": rfc(moved) }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["stillTrue"], true);
    assert_eq!(v["presentedTitle"], "Board <sync>");
    assert_eq!(v["timeZone"], "Europe/London");
    assert_eq!(
        v["joinPath"],
        format!("/{}", space["meetingCode"].as_str().unwrap())
    );
    assert_eq!(
        DateTime::parse_from_rfc3339(v["startsAt"].as_str().unwrap()).unwrap(),
        DateTime::parse_from_rfc3339(&rfc(moved)).unwrap()
    );
    assert!(v["endsAt"].is_string());

    let expected_refs: BTreeSet<String> = [
        account_ref(owner_user.id),
        account_ref(member.id),
        account_ref(invited_account.id),
        owner::email_principal_ref(&external),
    ]
    .into_iter()
    .collect();
    assert_eq!(refs(&v), expected_refs, "inactive accounts never listed");
    for r in v["recipients"].as_array().unwrap() {
        let reference = r["ref"].as_str().unwrap();
        if reference.starts_with("cfmeet-user:") {
            assert_eq!(r["email"], Value::Null, "account addresses come from X1");
        } else {
            assert_eq!(r["email"], external.to_ascii_lowercase());
        }
    }

    // Addresses X2 implies (accounts through X1) minus the actor == what the worker sent.
    let email_of = |reference: &str| -> String {
        [&owner_user, &member, &invited_account]
            .iter()
            .find(|u| account_ref(u.id) == reference)
            .map(|u| u.email.to_ascii_lowercase())
            .unwrap_or_else(|| external.to_ascii_lowercase())
    };
    let implied: BTreeSet<String> = refs(&v)
        .iter()
        .filter(|r| **r != account_ref(owner_user.id))
        .map(|r| email_of(r))
        .collect();
    assert_eq!(implied, worker);

    // The same shared query, without an actor.
    let (rows, truncated) = notify::conference_recipients(&app.db.conn, sid, cid, 200, None)
        .await
        .unwrap();
    assert!(!truncated);
    assert_eq!(rows.len(), expected_refs.len());
}

#[tokio::test]
async fn x2_applies_the_protected_title_rule() {
    let Some(o) = Owner::with_notifications().await else {
        return;
    };
    let app = &o.app;
    let clinician = app.user("clinician").await;
    let start = Utc::now() + Duration::days(1);
    let (space, cid) = scheduled(
        app,
        &clinician,
        json!({ "title": "Patient: J. Smith follow-up", "securityProfile": "HEALTHCARE", "accessPolicy": "INVITE_ONLY" }),
        start,
    )
    .await;
    invite(app, &clinician, uuid(&space["id"]), &unique_email("p")).await;
    let (s, v) = x2(&o, cid, json!({ "kind": "SCHEDULED" })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["presentedTitle"], "CF Meet meeting");
    assert!(!v.to_string().contains("Smith"));

    // The worker presents the same title.
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&clinician.token),
            json!({ "scheduledStartAt": rfc(start + Duration::hours(1)) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    notify::tick(&NotificationContext::from_state(&app.state), Utc::now())
        .await
        .unwrap();
    let sent = o.mail.sent();
    assert!(!sent.is_empty());
    for e in sent {
        assert!(e.subject.contains("CF Meet meeting") && !e.subject.contains("Smith"));
    }
}

#[tokio::test]
async fn x2_still_true_follows_cancellation_and_moved_reminders() {
    let Some(o) = Owner::start().await else {
        return;
    };
    let app = &o.app;
    let owner_user = app.user("owner").await;
    let start = Utc::now() + Duration::days(1);
    let (_, cid) = scheduled(app, &owner_user, json!({ "title": "Standup" }), start).await;

    let still = |v: &Value| v["stillTrue"].as_bool().unwrap();
    // A reminder for the scheduled start is due; for another start it is not.
    let (_, v) = x2(
        &o,
        cid,
        json!({ "kind": "REMINDER", "startsAt": rfc(start) }),
    )
    .await;
    assert!(still(&v), "{v}");
    let (_, v) = x2(&o, cid, json!({ "kind": "REMINDER" })).await;
    assert!(still(&v), "defaults to the current start");
    let (_, v) = x2(&o, cid, json!({ "kind": "CANCELLED" })).await;
    assert!(!still(&v), "not cancelled");

    // Moved: the old reminder is superseded, the new one is due.
    let moved = start + Duration::hours(3);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&owner_user.token),
            json!({ "scheduledStartAt": rfc(moved) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = x2(
        &o,
        cid,
        json!({ "kind": "REMINDER", "startsAt": rfc(start) }),
    )
    .await;
    assert!(!still(&v), "moved reminder: {v}");
    let (_, v) = x2(
        &o,
        cid,
        json!({ "kind": "REMINDER", "startsAt": rfc(moved) }),
    )
    .await;
    assert!(still(&v));
    let (_, v) = x2(
        &o,
        cid,
        json!({ "kind": "RESCHEDULED", "startsAt": rfc(moved) }),
    )
    .await;
    assert!(still(&v));
    let (_, v) = x2(
        &o,
        cid,
        json!({ "kind": "RESCHEDULED", "startsAt": rfc(start) }),
    )
    .await;
    assert!(!still(&v), "an older reschedule is superseded");

    // Cancelled: only the cancellation is still true.
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/cancel"),
            Auth::Bearer(&owner_user.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = x2(&o, cid, json!({ "kind": "CANCELLED" })).await;
    assert!(still(&v), "{v}");
    for kind in ["REMINDER", "RESCHEDULED", "SCHEDULED"] {
        let (_, v) = x2(&o, cid, json!({ "kind": kind, "startsAt": rfc(moved) })).await;
        assert!(!still(&v), "{kind} after cancellation: {v}");
    }
}
