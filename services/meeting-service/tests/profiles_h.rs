//! Phase 2 Part H — meeting profiles: webinar / town hall (stage, registration,
//! backstage) and healthcare (regulated admission, external role mapping, restrictive
//! processing defaults). Test IDs follow the Part H specification (H-S security, H-C
//! concurrency, H-F failure, H-E2E end-to-end).

mod common;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{http::StatusCode, routing::post, Json, Router};
use backend::{
    models::meeting_spaces,
    services::{
        actor::ActorContext,
        appointment_authorization::{
            AppointmentAuthorizationAdapter, AppointmentDecision, ExternalAuthorization,
            HttpAppointmentAuthorizationAdapter,
        },
    },
};
use chrono::{DateTime, Utc};
use common::{
    artifacts::{consent, set_profile, start_recording},
    code,
    collab::refresh,
    rtc::{change_role, connect, join_guest, join_user, lk, room, Joined},
    uuid, Auth, RtcMode, TestApp, TestOptions, User,
};
use livekit_protocol as proto;
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::artifacts().await {
            Some(a) => a,
            None => return,
        }
    };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn webinar() -> Value {
    json!({ "title": "Webinar", "accessPolicy": "PUBLIC", "meetingType": "WEBINAR" })
}

fn town_hall() -> Value {
    json!({ "title": "Town hall", "accessPolicy": "PUBLIC", "meetingType": "TOWN_HALL" })
}

fn healthcare() -> Value {
    json!({
        "title": "Consultation",
        "meetingType": "HEALTHCARE",
        "securityProfile": "HEALTHCARE",
        "accessPolicy": "HEALTHCARE_APPOINTMENT",
        "accessConfig": { "appointmentRef": "APPT-1" },
    })
}

async fn live(app: &TestApp, owner: &User, body: Value) -> (Uuid, Uuid) {
    app.live_conference(owner, body).await
}

async fn register(app: &TestApp, sid: Uuid, body: Value) -> (StatusCode, Value) {
    app.post(
        format!("/api/v2/meeting-spaces/{sid}/registrations"),
        Auth::None,
        body,
    )
    .await
}

async fn backstage(app: &TestApp, cid: Uuid, auth: Auth<'_>, enter: bool) -> (StatusCode, Value) {
    let uri = format!("/api/v2/conferences/{cid}/backstage");
    if enter {
        app.post(uri, auth, json!({})).await
    } else {
        app.delete(uri, auth).await
    }
}

fn publish(
    app: &TestApp,
    cid: Uuid,
    j: &Joined,
    source: proto::TrackSource,
) -> Result<String, String> {
    lk(app).publish(&room(cid), &j.identity, source)
}

/// Guest attendee of a live webinar, connected to the main room.
async fn attendee(app: &TestApp, cid: Uuid, name: &str) -> Joined {
    let g = join_guest(app, cid, name).await;
    connect(app, cid, &g).await;
    g
}

async fn role_of(app: &TestApp, pid: Uuid) -> String {
    use sea_orm::ConnectionTrait;
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT role FROM conference_participants WHERE id='{pid}'"),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get_by_index::<String>(0).unwrap()
}

/// Appointment authority test double returning an external (clinical) role, an
/// opaque reference and a validity window — never clinical details.
#[derive(Default)]
struct Clinical {
    grants: Mutex<HashMap<Uuid, (String, DateTime<Utc>)>>,
    down: AtomicBool,
    calls: AtomicU64,
}

impl Clinical {
    fn grant(&self, user: Uuid, role: &str, minutes: i64) {
        self.grants.lock().unwrap().insert(
            user,
            (role.into(), Utc::now() + chrono::Duration::minutes(minutes)),
        );
    }
}

struct ClinicalAdapter(Arc<Clinical>);

#[async_trait]
impl AppointmentAuthorizationAdapter for ClinicalAdapter {
    async fn authorize(
        &self,
        actor: &ActorContext,
        _space: &meeting_spaces::Model,
        _conference: Option<Uuid>,
        external_context: Option<&str>,
    ) -> AppointmentDecision {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        let denied = |r: &str| AppointmentDecision::Denied { reason: r.into() };
        if self.0.down.load(Ordering::SeqCst) {
            return denied("authority unavailable");
        }
        if external_context != Some("appt-token") {
            return denied("bad context");
        }
        let Some(user) = actor.user_id() else {
            return denied("not registered");
        };
        match self.0.grants.lock().unwrap().get(&user) {
            Some((role, until)) if *until > Utc::now() => AppointmentDecision::Authorized {
                role: None,
                context: Some(ExternalAuthorization {
                    external_role: Some(role.clone()),
                    reference: Some(format!("authz-{}", user.simple())),
                    valid_until: Some(*until),
                }),
            },
            _ => denied("no active appointment"),
        }
    }
}

async fn healthcare_app(clinical: Arc<Clinical>) -> Option<TestApp> {
    TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        appointments: Some(Arc::new(ClinicalAdapter(clinical))),
        ..Default::default()
    })
    .await
}

async fn clinical_join(app: &TestApp, cid: Uuid, user: &User) -> (StatusCode, Value) {
    app.join(
        cid,
        Auth::Bearer(&user.token),
        json!({ "externalContext": "appt-token" }),
    )
    .await
}

async fn admit(app: &TestApp, cid: Uuid, host: &User, pid: Uuid) {
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&host.token),
            json!({ "participantIds": [pid] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// Patient (or other clinical role) admitted through waiting room; returns the join body.
async fn clinical_admitted(app: &TestApp, cid: Uuid, clinician: &User, user: &User) -> Value {
    let (s, v) = clinical_join(app, cid, user).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Host-hierarchy roles are waiting-room exempt; everyone else waits.
    if matches!(v["participant"]["role"].as_str(), Some("HOST" | "CO_HOST")) {
        assert_eq!(v["participant"]["admissionState"], "ADMITTED");
        return v;
    }
    assert_eq!(
        v["participant"]["admissionState"], "WAITING",
        "healthcare waiting room: {v}"
    );
    admit(app, cid, clinician, uuid(&v["participant"]["id"])).await;
    let (s, v) = clinical_join(app, cid, user).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "ADMITTED");
    v
}

// ===========================================================================
// Security H-S1 … H-S15
// ===========================================================================

#[tokio::test]
async fn h_s01_public_uncontrolled_join_rejected_for_healthcare() {
    let app = app!();
    let owner = app.user("clinician").await;
    let (_, cid) = live(
        &app,
        &owner,
        json!({ "title": "x", "meetingType": "HEALTHCARE", "accessPolicy": "PUBLIC" }),
    )
    .await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "walk-in" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "PROFILE_POLICY_DENIED");
    let anyone = app.user("anyone").await;
    let (s, _) = app.join(cid, Auth::Bearer(&anyone.token), json!({})).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "authenticated but uncontrolled entry"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await,
        0
    );
    // A space override cannot reopen public access on a healthcare profile.
    let (_, sid2) = (
        0,
        uuid(
            &app.create_space(
                &owner,
                json!({ "title": "y", "meetingType": "HEALTHCARE", "accessPolicy": "PUBLIC" }),
            )
            .await["id"],
        ),
    );
    let (s, v) = set_profile(
        &app,
        &owner,
        sid2,
        json!({ "access": { "publicAccessAllowed": true } }),
    )
    .await;
    assert!(s.is_client_error(), "{v}");
}

#[tokio::test]
async fn h_s02_registration_cannot_select_privileged_role() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = live(&app, &owner, webinar()).await;
    for extra in [
        json!({ "role": "HOST" }),
        json!({ "requestedRole": "OWNER" }),
        json!({ "userId": owner.id }),
    ] {
        let mut body = json!({ "displayName": "Eve" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let (s, v) = register(&app, sid, body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    }
    let (s, v) = register(&app, sid, json!({ "displayName": "Eve" })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let token = v["admissionToken"].as_str().unwrap();
    let (s, j) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "Eve", "invitationToken": token }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["participant"]["role"], "VIEWER");
}

#[tokio::test]
async fn h_s03_registration_token_cannot_mint_rtc_grant() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = live(&app, &owner, webinar()).await;
    let (_, v) = register(&app, sid, json!({ "displayName": "Eve" })).await;
    assert!(
        v.get("rtc").is_none() && v.get("credential").is_none(),
        "{v}"
    );
    let token = v["admissionToken"].as_str().unwrap();
    // Not an SFU credential, not a participant token, not a bearer token.
    assert!(lk(&app).connect(token).is_err());
    let (s, _) = app
        .get(
            format!("/api/v2/conferences/{cid}/participants/me"),
            Auth::Participant(token),
        )
        .await;
    assert!(s.is_client_error());
    let (s, _) = app
        .get(
            format!("/api/v2/conferences/{cid}/participants/me"),
            Auth::Bearer(token),
        )
        .await;
    assert!(s.is_client_error());
    // Only the normal admission path turns it into a session + credential.
    let (_, j) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "Eve", "invitationToken": token }),
        )
        .await;
    assert_eq!(j["rtcStatus"], "ISSUED");
}

#[tokio::test]
async fn h_s04_registration_token_scoped() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid_a, _cid_a) = live(&app, &owner, webinar()).await;
    let (_sid_b, cid_b) = live(&app, &owner, webinar()).await;
    let (_, v) = register(&app, sid_a, json!({ "displayName": "Eve" })).await;
    let token = v["admissionToken"].as_str().unwrap().to_string();
    let (s, j) = app
        .join(
            cid_b,
            Auth::None,
            json!({ "displayName": "Eve", "invitationToken": token }),
        )
        .await;
    assert!(s.is_client_error(), "token of space A used on B: {j}");
    // Cancelled registration revokes the token.
    let (sid_c, cid_c) = live(&app, &owner, webinar()).await;
    let (_, v) = register(&app, sid_c, json!({ "displayName": "Mallory" })).await;
    let rid = uuid(&v["registrationId"]);
    let t2 = v["admissionToken"].as_str().unwrap().to_string();
    let (s, _) = app
        .delete(
            format!("/api/v2/meeting-spaces/{sid_c}/registrations/{rid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, j) = app
        .join(
            cid_c,
            Auth::None,
            json!({ "displayName": "Mallory", "invitationToken": t2 }),
        )
        .await;
    assert!(s.is_client_error(), "{j}");
}

#[tokio::test]
async fn h_s05_attendee_cannot_publish_with_modified_client() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "viewer").await;
    assert_eq!(a.body["participant"]["role"], "VIEWER");
    for src in [
        proto::TrackSource::Microphone,
        proto::TrackSource::Camera,
        proto::TrackSource::ScreenShare,
    ] {
        assert!(
            publish(&app, cid, &a, src).is_err(),
            "{src:?} must be denied by the SFU"
        );
    }
}

#[tokio::test]
async fn h_s06_attendee_cannot_self_promote() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "viewer").await;
    let (s, v) = app
        .patch(
            format!(
                "/api/v2/conferences/{cid}/participants/{}/role",
                a.participant_id
            ),
            a.auth(),
            json!({ "role": "PRESENTER" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(role_of(&app, a.participant_id).await, "VIEWER");
    // Media credential refresh never escalates.
    let (_, r) = refresh(&app, cid, &a, a.auth()).await;
    let fresh = r["rtc"]["credential"].as_str().unwrap();
    assert!(lk(&app).connect(fresh).is_ok());
    assert!(publish(&app, cid, &a, proto::TrackSource::Camera).is_err());
}

#[tokio::test]
async fn h_s07_host_a_cannot_promote_b_attendee() {
    let app = app!();
    let owner_a = app.user("a").await;
    let owner_b = app.user("b").await;
    let (_, cid_a) = live(&app, &owner_a, webinar()).await;
    let (_, cid_b) = live(&app, &owner_b, webinar()).await;
    let b_attendee = attendee(&app, cid_b, "viewer").await;
    let (s, _) = change_role(
        &app,
        cid_b,
        &owner_a,
        b_attendee.participant_id,
        "PRESENTER",
    )
    .await;
    assert!(matches!(s, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND));
    let (s, _) = change_role(
        &app,
        cid_a,
        &owner_a,
        b_attendee.participant_id,
        "PRESENTER",
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(role_of(&app, b_attendee.participant_id).await, "VIEWER");
}

#[tokio::test]
async fn h_s08_attendee_cannot_enter_backstage() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "viewer").await;
    let (s, v) = backstage(&app, cid, a.auth(), true).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "BACKSTAGE_ACCESS_DENIED");
    let (s, _) = backstage(&app, cid, Auth::None, true).await;
    assert!(s.is_client_error());
    assert!(!lk(&app).has_room(&backend::services::specialized::backstage_room_name(cid)));
}

#[tokio::test]
async fn h_s09_stale_backstage_credential_rejected_after_demotion() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let p = attendee(&app, cid, "panelist").await;
    change_role(&app, cid, &owner, p.participant_id, "PRESENTER").await;
    let (s, v) = backstage(&app, cid, p.auth(), true).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let bs_room = backend::services::specialized::backstage_room_name(cid);
    let (_, r) = refresh(&app, cid, &p, p.auth()).await;
    let cred = r["rtc"]["credential"].as_str().unwrap().to_string();
    assert_eq!(lk(&app).connect(&cred).unwrap(), p.identity);
    assert!(lk(&app).identities(&bs_room).contains(&p.identity));
    // Demotion evicts from the backstage room and clears the context.
    let (s, v) = change_role(&app, cid, &owner, p.participant_id, "VIEWER").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        !lk(&app).identities(&bs_room).contains(&p.identity),
        "evicted from backstage"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE id='{}' AND backstage_at IS NULL",
            p.participant_id
        ))
        .await,
        1
    );
    // The stale credential reconnecting is evicted on the verified provider callback.
    lk(&app).connect(&cred).unwrap();
    let body = common::livekit_mock::webhook_body(
        "participant_joined",
        &format!("EV_{}", Uuid::new_v4().simple()),
        &bs_room,
        None,
        Some(&p.identity),
    );
    app.signed_webhook(&body).await;
    assert!(
        !lk(&app).identities(&bs_room).contains(&p.identity),
        "stale backstage credential evicted"
    );
}

#[tokio::test]
async fn h_s10_healthcare_external_role_never_owner() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical.clone()).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (sid, cid) = live(&app, &clinician, healthcare()).await;
    // An OWNER mapping is either rejected or clamped by the ceiling — never effective.
    let (s, v) = set_profile(
        &app,
        &clinician,
        sid,
        json!({ "roles": { "externalRoleMapping": { "CHIEF": "OWNER" } } }),
    )
    .await;
    assert!(s == StatusCode::BAD_REQUEST || s == StatusCode::OK, "{v}");
    if s == StatusCode::OK {
        assert_ne!(
            v["effective"]["policy"]["roles"]["externalRoleMapping"]["CHIEF"], "OWNER",
            "{v}"
        );
    }
    for (who, role) in [
        ("owner-claim", "OWNER"),
        ("chief", "CHIEF"),
        ("doc", "DOCTOR"),
    ] {
        let u = app.user(who).await;
        clinical.grant(u.id, role, 30);
        let v = clinical_admitted(&app, cid, &clinician, &u).await;
        assert_ne!(v["participant"]["role"], "OWNER", "{v}");
        match role {
            "DOCTOR" => assert_eq!(v["participant"]["role"], "HOST"),
            "OWNER" => assert_eq!(
                v["participant"]["role"], "PARTICIPANT",
                "unknown external role → default"
            ),
            _ => {}
        }
    }
}

#[tokio::test]
async fn h_s11_appointment_cannot_bypass_final_admission() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical.clone()).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid) = live(&app, &clinician, healthcare()).await;
    let patient = app.user("patient").await;
    clinical.grant(patient.id, "PATIENT", 30);
    let (s, v) = clinical_join(&app, cid, &patient).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "WAITING");
    assert_eq!(
        v["rtcStatus"], "NOT_ISSUED_WAITING",
        "no media before CF Meet admits: {v}"
    );
    // A ban by the meeting wins over a valid appointment.
    let pid = uuid(&v["participant"]["id"]);
    admit(&app, cid, &clinician, pid).await;
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/participants/{pid}/ban"),
            Auth::Bearer(&clinician.token),
            json!({}),
        )
        .await;
    assert!(s.is_success());
    let (s, v) = clinical_join(&app, cid, &patient).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
}

#[tokio::test]
async fn h_s12_healthcare_guest_token_scoped_to_context() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical.clone()).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid_a) = live(&app, &clinician, healthcare()).await;
    let (_, cid_b) = live(&app, &clinician, healthcare()).await;
    let patient = app.user("patient").await;
    clinical.grant(patient.id, "PATIENT", 30);
    let v = clinical_admitted(&app, cid_a, &clinician, &patient).await;
    let session = &v["rtc"]["credential"];
    // Participant tokens / credentials of A grant nothing in B.
    if let Some(tok) = v["participantToken"].as_str() {
        let (s, _) = app
            .get(
                format!("/api/v2/conferences/{cid_b}/participants/me"),
                Auth::Participant(tok),
            )
            .await;
        assert!(s.is_client_error());
    }
    let claims = livekit_api::access_token::TokenVerifier::with_api_key(
        common::livekit_mock::API_KEY,
        common::livekit_mock::API_SECRET,
    )
    .verify(session.as_str().unwrap())
    .unwrap();
    assert_eq!(
        claims.video.room,
        room(cid_a),
        "credential scoped to A's room"
    );
    // Without the external context the same account is denied.
    let (s, _) = app
        .join(cid_b, Auth::Bearer(&patient.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn h_s13_healthcare_recording_disabled_by_default() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (sid, cid) = live(&app, &clinician, healthcare()).await;
    let (_, p) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/profile-policy"),
            Auth::Bearer(&clinician.token),
        )
        .await;
    assert_eq!(
        p["effective"]["policy"]["recording"]["allowed"], false,
        "{p}"
    );
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&clinician.token)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "RECORDING_NOT_ALLOWED");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
}

#[tokio::test]
async fn h_s14_prohibited_external_intelligence_zero_provider_calls() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid) = live(&app, &clinician, healthcare()).await;
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/intelligence/requests"),
            Auth::Bearer(&clinician.token),
            json!({ "sourceArtifactId": Uuid::new_v4(), "kind": "SUMMARY" }),
        )
        .await;
    assert_eq!(code(&v), "INTELLIGENCE_NOT_ALLOWED", "{v}");
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/transcription"),
            Auth::Bearer(&clinician.token),
            json!({ "mode": "LIVE" }),
        )
        .await;
    assert!(
        matches!(code(&v), "TRANSCRIPTION_NOT_ALLOWED" | "PERMISSION_DENIED"),
        "{v}"
    );
    assert_eq!(app.intelligence.calls.load(Ordering::SeqCst), 0);
    assert_eq!(app.transcription.count(""), 0);
}

async fn clinical_payload_authority() -> String {
    async fn handler(Json(body): Json<Value>) -> Json<Value> {
        let _ = body;
        Json(json!({
            "authorized": true,
            "clinicalRole": "PATIENT",
            "authorizationRef": "authz-ref-77",
            "validUntil": Utc::now() + chrono::Duration::hours(1),
            "diagnosis": "SECRET-DIAGNOSIS-XYZ",
            "medications": ["SECRET-MED-ABC"],
            "notes": { "free": "SECRET-NOTE" },
        }))
    }
    let app = Router::new().route("/authorize", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/authorize")
}

async fn no_clinical_payload(app: &TestApp) {
    for needle in ["SECRET-DIAGNOSIS-XYZ", "SECRET-MED-ABC", "SECRET-NOTE"] {
        let n = app
            .scalar(&format!(
                "SELECT count(*) FROM (
                   SELECT row_to_json(t)::text AS j FROM conference_participants t
                   UNION ALL SELECT row_to_json(t)::text FROM participant_sessions t
                   UNION ALL SELECT row_to_json(t)::text FROM conference_audit_events t
                   UNION ALL SELECT row_to_json(t)::text FROM outbox_events t
                   UNION ALL SELECT row_to_json(t)::text FROM conference_sessions t
                   UNION ALL SELECT row_to_json(t)::text FROM meeting_artifacts t
                   UNION ALL SELECT row_to_json(t)::text FROM rtc_webhook_events t
                   UNION ALL SELECT row_to_json(t)::text FROM rtc_drift_events t) x
                 WHERE j LIKE '%{needle}%'"
            ))
            .await;
        assert_eq!(n, 0, "{needle} persisted");
    }
}

#[tokio::test]
async fn h_s15_clinical_payload_not_duplicated() {
    let url = clinical_payload_authority().await;
    let Some(app) = TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        appointments: Some(Arc::new(HttpAppointmentAuthorizationAdapter::new(
            url,
            None,
            Duration::from_secs(2),
        ))),
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid) = live(&app, &clinician, healthcare()).await;
    let patient = app.user("patient").await;
    let v = clinical_admitted(&app, cid, &clinician, &patient).await;
    assert_eq!(v["participant"]["role"], "PARTICIPANT");
    let pid = uuid(&v["participant"]["id"]);
    // Only references are kept.
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{pid}' AND external_role='PATIENT' AND external_authz_ref='authz-ref-77' AND external_authz_valid_until IS NOT NULL")).await, 1);
    no_clinical_payload(&app).await;
}

// ===========================================================================
// Concurrency H-C1 … H-C8
// ===========================================================================

#[tokio::test]
async fn h_c01_same_idempotency_key_registers_once() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, _) = live(&app, &owner, webinar()).await;
    let body = json!({ "displayName": "Eve", "idempotencyKey": "reg-1" });
    let (a, b) = tokio::join!(
        register(&app, sid, body.clone()),
        register(&app, sid, body.clone())
    );
    let created = [&a, &b]
        .iter()
        .filter(|r| r.0 == StatusCode::CREATED)
        .count();
    assert_eq!(created, 1, "{a:?} {b:?}");
    let other = if a.0 == StatusCode::CREATED { &b } else { &a };
    assert!(
        other.0 == StatusCode::OK || other.0 == StatusCode::CONFLICT,
        "{other:?}"
    );
    if other.0 == StatusCode::OK {
        assert!(other.1["admissionToken"].is_null(), "token shown once");
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM webinar_registrations WHERE meeting_space_id='{sid}'"
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_invitations WHERE meeting_space_id='{sid}'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn h_c02_token_consume_race() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = live(&app, &owner, webinar()).await;
    let (s, v) = set_profile(
        &app,
        &owner,
        sid,
        json!({ "registration": { "tokenMaxUses": 1 } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = register(&app, sid, json!({ "displayName": "Eve" })).await;
    let token = v["admissionToken"].as_str().unwrap().to_string();
    let (a, b) = tokio::join!(
        app.join(
            cid,
            Auth::None,
            json!({ "displayName": "Eve-1", "invitationToken": token })
        ),
        app.join(
            cid,
            Auth::None,
            json!({ "displayName": "Eve-2", "invitationToken": token })
        )
    );
    let ok = [&a, &b].iter().filter(|r| r.0 == StatusCode::OK).count();
    assert_eq!(ok, 1, "{a:?} {b:?}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT usage_count::bigint FROM meeting_invitations WHERE meeting_space_id='{sid}'"
        ))
        .await,
        1
    );
}

async fn grants_match_role(app: &TestApp, cid: Uuid, j: &Joined) {
    let role = role_of(app, j.participant_id).await;
    let can = publish(app, cid, j, proto::TrackSource::Camera).is_ok();
    assert_eq!(can, role != "VIEWER", "role {role} vs SFU grant {can}");
}

#[tokio::test]
async fn h_c03_promotion_vs_demotion() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "a").await;
    change_role(&app, cid, &owner, a.participant_id, "PRESENTER").await;
    let (x, y) = tokio::join!(
        change_role(&app, cid, &owner, a.participant_id, "VIEWER"),
        change_role(&app, cid, &owner, a.participant_id, "PARTICIPANT")
    );
    assert!(x.0.is_success() && y.0.is_success(), "{x:?} {y:?}");
    common::artifacts::dispatch(&app, cid).await;
    grants_match_role(&app, cid, &a).await;
}

#[tokio::test]
async fn h_c04_promotion_vs_removal() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "a").await;
    let remove = app.post(
        format!(
            "/api/v2/conferences/{cid}/participants/{}/remove",
            a.participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    );
    let (p, r) = tokio::join!(
        change_role(&app, cid, &owner, a.participant_id, "PRESENTER"),
        remove
    );
    assert!(r.0.is_success(), "{r:?}");
    let _ = p;
    common::artifacts::dispatch(&app, cid).await;
    assert!(
        !lk(&app).identities(&room(cid)).contains(&a.identity),
        "removed participant has no media"
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND admission_state='ADMITTED'", a.participant_id)).await, 0);
}

#[tokio::test]
async fn h_c05_backstage_entry_vs_demotion() {
    for _ in 0..3 {
        let app = app!();
        let owner = app.user("owner").await;
        let (_, cid) = live(&app, &owner, webinar()).await;
        let p = attendee(&app, cid, "p").await;
        change_role(&app, cid, &owner, p.participant_id, "PRESENTER").await;
        let (_b, d) = tokio::join!(
            backstage(&app, cid, p.auth(), true),
            change_role(&app, cid, &owner, p.participant_id, "VIEWER")
        );
        assert!(d.0.is_success(), "{d:?}");
        assert_eq!(
            app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND backstage_at IS NOT NULL", p.participant_id)).await,
            0,
            "a VIEWER never remains backstage"
        );
    }
}

#[tokio::test]
async fn h_c06_healthcare_authorization_expiry_vs_join() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical.clone()).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid) = live(&app, &clinician, healthcare()).await;
    let patient = app.user("patient").await;
    clinical.grant(patient.id, "PATIENT", 30);
    clinical_admitted(&app, cid, &clinician, &patient).await;
    // Authorization window shortened to 1 minute: the session is capped by it.
    clinical.grant(patient.id, "PATIENT", 1);
    let until = clinical.grants.lock().unwrap()[&patient.id].1;
    let expire = async {
        clinical.grants.lock().unwrap().insert(
            patient.id,
            ("PATIENT".into(), Utc::now() - chrono::Duration::seconds(1)),
        );
    };
    let (j, _) = tokio::join!(clinical_join(&app, cid, &patient), expire);
    if j.0 == StatusCode::OK {
        let exp: DateTime<Utc> = j.1["participantSession"]["expiresAt"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            exp <= until + chrono::Duration::seconds(1),
            "session outlives authorization: {exp} > {until}"
        );
    } else {
        assert_eq!(j.0, StatusCode::FORBIDDEN, "{j:?}");
    }
    let (s, _) = clinical_join(&app, cid, &patient).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "expired authorization fails closed"
    );
}

#[tokio::test]
async fn h_c07_recording_enable_vs_profile_restriction() {
    for _ in 0..3 {
        let app = app!();
        let owner = app.user("owner").await;
        let (sid, cid) = live(
            &app,
            &owner,
            json!({ "title": "x", "accessPolicy": "PUBLIC" }),
        )
        .await;
        let (_r, p) = tokio::join!(
            start_recording(&app, cid, Auth::Bearer(&owner.token)),
            set_profile(
                &app,
                &owner,
                sid,
                json!({ "recording": { "allowed": false } })
            )
        );
        assert_eq!(p.0, StatusCode::OK, "{p:?}");
        let active = app
            .scalar(&format!("SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}' AND status IN ('REQUESTED','RECORDING')"))
            .await;
        assert_eq!(active, 0, "restriction wins");
    }
}

#[tokio::test]
async fn h_c08_profile_update_vs_reconnect() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "a").await;
    let reconnect = app.join(cid, a.auth(), json!({ "previousSessionId": a.session_id }));
    let (r, p) = tokio::join!(
        reconnect,
        set_profile(
            &app,
            &owner,
            sid,
            json!({ "access": { "guestSessionTtlSecs": 1800 } })
        )
    );
    assert_eq!(r.0, StatusCode::OK, "{r:?}");
    assert_eq!(p.0, StatusCode::OK, "{p:?}");
    assert_eq!(
        p.1["profilePolicyVersion"], 2,
        "exactly one version step from the default 1"
    );
    assert_eq!(r.1["participant"]["role"], "VIEWER");
    let active = app
        .scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{}' AND state IN ('PENDING','CONNECTED')", a.participant_id))
        .await;
    assert_eq!(active, 1);
}

// ===========================================================================
// Failure H-F1 … H-F8
// ===========================================================================

#[tokio::test]
async fn h_f01_registration_storage_failure() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, _) = live(&app, &owner, webinar()).await;
    common::rtc::exec(
        &app,
        "ALTER TABLE webinar_registrations ADD CONSTRAINT ck_test_outage CHECK (false) NOT VALID",
    )
    .await;
    let (s, v) = register(&app, sid, json!({ "displayName": "Eve" })).await;
    assert!(s.is_server_error(), "{s} {v}");
    assert!(v["admissionToken"].is_null());
    // Atomic: no orphan admission token.
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_invitations WHERE meeting_space_id='{sid}'"
        ))
        .await,
        0
    );
    common::rtc::exec(
        &app,
        "ALTER TABLE webinar_registrations DROP CONSTRAINT ck_test_outage",
    )
    .await;
    let (s, _) = register(&app, sid, json!({ "displayName": "Eve" })).await;
    assert_eq!(s, StatusCode::CREATED);
}

#[tokio::test]
async fn h_f02_token_validation_database_failure() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = live(&app, &owner, webinar()).await;
    let (_, v) = register(&app, sid, json!({ "displayName": "Eve" })).await;
    let token = v["admissionToken"].as_str().unwrap().to_string();
    common::rtc::exec(
        &app,
        "ALTER TABLE meeting_invitations RENAME TO meeting_invitations_offline",
    )
    .await;
    let (s, j) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "Eve", "invitationToken": token }),
        )
        .await;
    common::rtc::exec(
        &app,
        "ALTER TABLE meeting_invitations_offline RENAME TO meeting_invitations",
    )
    .await;
    assert_ne!(s, StatusCode::OK, "{j}");
    assert!(j["rtc"].is_null(), "fail closed: no credential");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND display_name='Eve'")).await, 0);
}

#[tokio::test]
async fn h_f03_promotion_commits_but_rtc_sync_fails() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "a").await;
    lk(&app).faults.fail_update.store(true, Ordering::SeqCst);
    let (s, v) = change_role(&app, cid, &owner, a.participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(role_of(&app, a.participant_id).await, "PRESENTER");
    assert!(
        publish(&app, cid, &a, proto::TrackSource::Camera).is_err(),
        "SFU not yet updated"
    );
    lk(&app).faults.fail_update.store(false, Ordering::SeqCst);
    common::artifacts::expire_backoff(&app, cid).await;
    common::artifacts::dispatch(&app, cid).await;
    assert!(
        publish(&app, cid, &a, proto::TrackSource::Camera).is_ok(),
        "retry converges"
    );
}

#[tokio::test]
async fn h_f04_backstage_provider_routing_fails() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let p = attendee(&app, cid, "p").await;
    change_role(&app, cid, &owner, p.participant_id, "PRESENTER").await;
    lk(&app).faults.fail_create.store(true, Ordering::SeqCst);
    let (s, v) = backstage(&app, cid, p.auth(), true).await;
    assert!(s.is_server_error(), "{s} {v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE id='{}' AND backstage_at IS NULL",
            p.participant_id
        ))
        .await,
        1,
        "context unchanged"
    );
    assert!(
        publish(&app, cid, &p, proto::TrackSource::Camera).is_ok(),
        "still on the main stage"
    );
    lk(&app).faults.fail_create.store(false, Ordering::SeqCst);
    let (s, _) = backstage(&app, cid, p.auth(), true).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn h_f05_healthcare_adapter_unavailable_fails_closed() {
    let Some(app) = TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        appointments: Some(Arc::new(HttpAppointmentAuthorizationAdapter::new(
            "http://127.0.0.1:1/authorize".into(),
            None,
            Duration::from_millis(500),
        ))),
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid) = live(&app, &clinician, healthcare()).await;
    let patient = app.user("patient").await;
    let (s, v) = clinical_join(&app, cid, &patient).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn h_f06_healthcare_adapter_timeout_fails_closed() {
    async fn slow() -> Json<Value> {
        tokio::time::sleep(Duration::from_secs(3)).await;
        Json(json!({ "authorized": true }))
    }
    let router = Router::new().route("/authorize", post(slow));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let Some(app) = TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        appointments: Some(Arc::new(HttpAppointmentAuthorizationAdapter::new(
            format!("http://{addr}/authorize"),
            None,
            Duration::from_millis(300),
        ))),
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid) = live(&app, &clinician, healthcare()).await;
    let patient = app.user("patient").await;
    let started = std::time::Instant::now();
    let (s, v) = clinical_join(&app, cid, &patient).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "bounded by the adapter timeout"
    );
}

#[tokio::test]
async fn h_f07_external_intelligence_unavailable_under_allowed_policy() {
    let app = app!();
    let (owner, _, cid, _) = common::artifacts::meeting_with(
        &app,
        json!({ "title": "x", "accessPolicy": "PUBLIC", "transcriptionPolicy": "HOST_OPTIONAL" }),
        0,
    )
    .await;
    let (_, transcript) =
        common::artifacts::transcript_ready(&app, cid, &owner, &[("hi", None)]).await;
    app.intelligence.fail.store(true, Ordering::SeqCst);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/intelligence/requests"),
            Auth::Bearer(&owner.token),
            json!({ "sourceArtifactId": transcript, "kind": "SUMMARY" }),
        )
        .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(code(&v), "INTELLIGENCE_PROVIDER_UNAVAILABLE");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}' AND artifact_type='MEETING_SUMMARY'")).await, 0);
    assert_eq!(common::rtc::conference_status(&app, cid).await, "LIVE");
}

#[tokio::test]
async fn h_f08_profile_misconfiguration_is_restrictive() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = live(
        &app,
        &owner,
        json!({ "title": "x", "accessPolicy": "PUBLIC" }),
    )
    .await;
    common::rtc::exec(&app, &format!("UPDATE meeting_spaces SET profile_policy = '{{\"recording\": {{\"allowed\": \"yes please\"}}, \"bogus\": 1}}'::jsonb WHERE id='{sid}'")).await;
    let (_, p) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/profile-policy"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(p["effective"]["misconfigured"], true, "{p}");
    let (_, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(code(&v), "RECORDING_NOT_ALLOWED", "{v}");
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "walk-in" }))
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "restrictive fallback denies public entry: {v}"
    );
}

// ===========================================================================
// End-to-end H-E2E-01 … H-E2E-18
// ===========================================================================

#[tokio::test]
async fn h_e2e_01_webinar_attendee_defaults_to_viewer() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "a").await;
    assert_eq!(a.body["participant"]["role"], "VIEWER");
    let u = app.user("registered").await;
    let j = join_user(&app, cid, &u).await;
    assert_eq!(j.body["participant"]["role"], "VIEWER");
    let (_, p) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/profile-policy"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(p["effective"]["profile"], "WEBINAR");
    assert_eq!(p["effective"]["policy"]["stage"]["backstageEnabled"], true);
}

#[tokio::test]
async fn h_e2e_02_modified_attendee_client_cannot_publish() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "a").await;
    let claims = lk(&app).participant(&room(cid), &a.identity).unwrap();
    assert!(
        !claims.permission.can_publish,
        "credential carries no publish grant"
    );
    assert!(claims.permission.can_subscribe);
    for src in [
        proto::TrackSource::Microphone,
        proto::TrackSource::Camera,
        proto::TrackSource::ScreenShare,
    ] {
        assert!(publish(&app, cid, &a, src).is_err());
    }
}

#[tokio::test]
async fn h_e2e_03_attendee_engagement() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "a").await;
    let base = format!("/api/v2/conferences/{cid}");
    let (s, v) = app
        .post(
            format!("{base}/questions"),
            a.auth(),
            json!({ "body": "When is the Q&A?" }),
        )
        .await;
    assert!(s.is_success(), "Q&A: {v}");
    let (s, v) = app
        .post(
            format!("{base}/reactions"),
            a.auth(),
            json!({ "reaction": "thumbs_up" }),
        )
        .await;
    assert!(s.is_success(), "reaction: {v}");
    let (s, v) = app
        .post(format!("{base}/hand-raise"), a.auth(), json!({}))
        .await;
    assert!(s.is_success(), "hand raise: {v}");
    let pid =
        common::collab::open_poll(&app, cid, &owner, common::collab::simple_poll("AFTER_VOTE"))
            .await;
    let (_, poll) = app
        .get(format!("{base}/polls/{pid}"), Auth::Bearer(&owner.token))
        .await;
    let opt = poll["questions"][0]["options"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, v) = common::collab::vote(&app, cid, pid, a.auth(), &opt).await;
    assert!(s.is_success(), "poll: {v}");
}

#[tokio::test]
async fn h_e2e_04_05_06_promote_publish_demote() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let a = attendee(&app, cid, "a").await;
    // 04: host promotes.
    let (s, v) = change_role(&app, cid, &owner, a.participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(common::artifacts::events(&app, cid, "participant.promoted").await >= 1);
    // 05: RTC grants update; the presenter publishes.
    assert!(publish(&app, cid, &a, proto::TrackSource::Camera).is_ok());
    assert!(publish(&app, cid, &a, proto::TrackSource::Microphone).is_ok());
    // 06: demotion removes publish rights (and live tracks).
    let (s, v) = change_role(&app, cid, &owner, a.participant_id, "VIEWER").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(publish(&app, cid, &a, proto::TrackSource::Camera).is_err());
    assert!(common::artifacts::events(&app, cid, "participant.demoted").await >= 1);
    assert!(
        common::artifacts::audits(&app, cid, "PARTICIPANT_DEMOTE").await
            + common::artifacts::audits(&app, cid, "PARTICIPANT_ROLE_CHANGE").await
            >= 1
    );
}

#[tokio::test]
async fn h_e2e_07_registration_collects_only_approved_fields() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, _) = live(&app, &owner, webinar()).await;
    let (s, v) = set_profile(&app, &owner, sid, json!({ "registration": {
        "email": "REQUIRED",
        "customFields": [{ "key": "company_size", "label": "Company size", "required": true, "maxLength": 20 }]
    } })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = register(
        &app,
        sid,
        json!({ "displayName": "Eve", "customFields": { "company_size": "10" } }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "email required");
    let (s, _) = register(&app, sid, json!({ "displayName": "Eve", "email": "eve@example.com", "customFields": { "company_size": "10", "ssn": "123" } })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unapproved field");
    let (s, _) = register(
        &app,
        sid,
        json!({ "displayName": "Eve", "email": "eve@example.com", "phone": "555" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown top-level field");
    let (s, v) = register(&app, sid, json!({ "displayName": "Eve", "email": "Eve@Example.com", "customFields": { "company_size": "10" } })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let rid = uuid(&v["registrationId"]);
    let (_, r) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/registrations/{rid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(r["email"], "eve@example.com");
    assert_eq!(r["customFields"], json!({ "company_size": "10" }));
    assert!(r["organization"].is_null());
}

#[tokio::test]
async fn h_e2e_08_09_registration_token_through_normal_admission() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = live(&app, &owner, webinar()).await;
    let (_, v) = register(
        &app,
        sid,
        json!({ "displayName": "Eve", "conferenceId": cid }),
    )
    .await;
    let token = v["admissionToken"].as_str().unwrap().to_string();
    assert!(token.starts_with("reg_"));
    // 08: scoped, hash-only, VIEWER-role invitation.
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_invitations WHERE meeting_space_id='{sid}' AND conference_id='{cid}' AND requested_role='VIEWER' AND invitee_email IS NULL")).await, 1);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_invitations WHERE token_hash = '{token}'"
        ))
        .await,
        0
    );
    // 09: normal admission (participant + session + credential).
    let (s, j) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "Eve", "invitationToken": token }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["participant"]["identityType"], "INVITED_EXTERNAL");
    assert_eq!(j["participant"]["role"], "VIEWER");
    assert!(j["participantSession"]["id"].is_string());
}

#[tokio::test]
async fn h_e2e_10_11_12_backstage() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = live(&app, &owner, webinar()).await;
    let panelist = attendee(&app, cid, "panelist").await;
    let viewer = attendee(&app, cid, "viewer").await;
    change_role(&app, cid, &owner, panelist.participant_id, "PRESENTER").await;
    // 10: panelist enters; isolated provider room.
    let (s, v) = backstage(&app, cid, panelist.auth(), true).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mediaContext"]["kind"], "BACKSTAGE", "{v}");
    let bs = backend::services::specialized::backstage_room_name(cid);
    assert!(lk(&app).has_room(&bs));
    let (_, r) = refresh(&app, cid, &panelist, panelist.auth()).await;
    lk(&app)
        .connect(r["rtc"]["credential"].as_str().unwrap())
        .unwrap();
    assert!(lk(&app).identities(&bs).contains(&panelist.identity));
    assert!(
        !lk(&app).identities(&room(cid)).contains(&panelist.identity),
        "single media context"
    );
    // 11: attendee cannot.
    let (s, _) = backstage(&app, cid, viewer.auth(), true).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(!lk(&app).identities(&bs).contains(&viewer.identity));
    // 12: back to the main stage.
    let (s, v) = backstage(&app, cid, panelist.auth(), false).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mediaContext"]["kind"], "MAIN");
    assert!(!lk(&app).identities(&bs).contains(&panelist.identity));
    let (_, r) = refresh(&app, cid, &panelist, panelist.auth()).await;
    lk(&app)
        .connect(r["rtc"]["credential"].as_str().unwrap())
        .unwrap();
    assert!(publish(&app, cid, &panelist, proto::TrackSource::Camera).is_ok());
}

#[tokio::test]
async fn h_e2e_13_town_hall_stage_limit() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = live(&app, &owner, town_hall()).await;
    let (s, v) = set_profile(
        &app,
        &owner,
        sid,
        json!({ "stage": { "maxStageParticipants": 2, "maxPresenters": 1 } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let mut viewers = vec![];
    for i in 0..4 {
        viewers.push(attendee(&app, cid, &format!("v{i}")).await);
    }
    assert!(viewers
        .iter()
        .all(|v| v.body["participant"]["role"] == "VIEWER"));
    let (s, _) = change_role(&app, cid, &owner, viewers[0].participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = change_role(&app, cid, &owner, viewers[1].participant_id, "PRESENTER").await;
    assert_eq!(code(&v), "STAGE_CAPACITY_REACHED", "presenter limit: {v}");
    let (s, _) = change_role(&app, cid, &owner, viewers[1].participant_id, "PARTICIPANT").await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = change_role(&app, cid, &owner, viewers[2].participant_id, "PARTICIPANT").await;
    assert_eq!(code(&v), "STAGE_CAPACITY_REACHED", "stage limit: {v}");
    for v in &viewers[2..] {
        assert!(
            publish(&app, cid, v, proto::TrackSource::Camera).is_err(),
            "audience stays subscribe-only"
        );
    }
}

#[tokio::test]
async fn h_e2e_14_healthcare_rejects_uncontrolled_public_admission() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid) = live(&app, &clinician, healthcare()).await;
    let (s, _) = app
        .join(cid, Auth::None, json!({ "displayName": "walk-in" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let stranger = app.user("stranger").await;
    let (s, _) = clinical_join(&app, cid, &stranger).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "no appointment → denied");
}

#[tokio::test]
async fn h_e2e_15_healthcare_waiting_room_default() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical.clone()).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (sid, cid) = live(&app, &clinician, healthcare()).await;
    let (_, p) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/profile-policy"),
            Auth::Bearer(&clinician.token),
        )
        .await;
    assert_eq!(
        p["effective"]["policy"]["access"]["waitingRoomForced"],
        true
    );
    let patient = app.user("patient").await;
    clinical.grant(patient.id, "PATIENT", 30);
    let (_, v) = clinical_join(&app, cid, &patient).await;
    assert_eq!(v["participant"]["admissionState"], "WAITING");
    // The clinician (host) is not held in the waiting room.
    let j = join_user(&app, cid, &clinician).await;
    assert_eq!(j.body["participant"]["admissionState"], "ADMITTED");
}

#[tokio::test]
async fn h_e2e_16_external_clinical_role_mapped() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical.clone()).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid) = live(&app, &clinician, healthcare()).await;
    for (who, ext, expected) in [
        ("pat", "PATIENT", "PARTICIPANT"),
        ("nurse", "NURSE", "PRESENTER"),
        ("spec", "SPECIALIST", "CO_HOST"),
        ("rep", "HOSPITAL_REPRESENTATIVE", "VIEWER"),
    ] {
        let u = app.user(who).await;
        clinical.grant(u.id, ext, 30);
        let v = clinical_admitted(&app, cid, &clinician, &u).await;
        assert_eq!(v["participant"]["role"], expected, "{ext}: {v}");
        let pid = uuid(&v["participant"]["id"]);
        assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{pid}' AND external_role='{ext}'")).await, 1);
    }
    assert!(
        common::artifacts::audits(&app, cid, "EXTERNAL_AUTHORIZATION").await >= 4,
        "enhanced audit"
    );
}

#[tokio::test]
async fn h_e2e_17_healthcare_processing_restrictions() {
    let clinical = Arc::new(Clinical::default());
    let Some(app) = healthcare_app(clinical.clone()).await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (sid, cid) = live(&app, &clinician, healthcare()).await;
    let (_, p) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/profile-policy"),
            Auth::Bearer(&clinician.token),
        )
        .await;
    let pol = &p["effective"]["policy"];
    assert_eq!(pol["recording"]["allowed"], false);
    assert_eq!(pol["recording"]["consentMode"], "EXPLICIT_CONSENT_REQUIRED");
    assert_eq!(pol["transcription"]["allowed"], false);
    assert_eq!(pol["intelligence"]["location"], "DISABLED");
    assert_eq!(pol["exports"]["chatExportAllowed"], false);
    let (_, v) = start_recording(&app, cid, Auth::Bearer(&clinician.token)).await;
    assert_eq!(code(&v), "RECORDING_NOT_ALLOWED");
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/artifacts/reports"),
            Auth::Bearer(&clinician.token),
            json!({ "artifactType": "CHAT_EXPORT" }),
        )
        .await;
    assert_eq!(code(&v), "PROFILE_POLICY_DENIED");
    // A space override cannot lift the HEALTHCARE ceiling to external processing.
    set_profile(&app, &clinician, sid, json!({ "intelligence": { "location": "EXTERNAL_ALLOWED" }, "transcription": { "location": "EXTERNAL_ALLOWED" } })).await;
    let (_, p) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/profile-policy"),
            Auth::Bearer(&clinician.token),
        )
        .await;
    assert_ne!(
        p["effective"]["policy"]["intelligence"]["location"], "EXTERNAL_ALLOWED",
        "{p}"
    );
    assert_ne!(
        p["effective"]["policy"]["transcription"]["location"], "EXTERNAL_ALLOWED",
        "{p}"
    );
    let clamped = p["effective"]["clampedFields"].to_string();
    assert!(
        clamped.contains("intelligence.location") && clamped.contains("transcription.location"),
        "{p}"
    );
    let _ = consent;
}

#[tokio::test]
async fn h_e2e_18_no_clinical_payload_persisted() {
    let url = clinical_payload_authority().await;
    let Some(app) = TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        appointments: Some(Arc::new(HttpAppointmentAuthorizationAdapter::new(
            url,
            None,
            Duration::from_secs(2),
        ))),
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let clinician = app.user("clinician").await;
    let (_, cid) = live(&app, &clinician, healthcare()).await;
    let patient = app.user("patient").await;
    let v = clinical_admitted(&app, cid, &clinician, &patient).await;
    let j: Joined = Joined {
        participant_id: uuid(&v["participant"]["id"]),
        session_id: uuid(&v["participantSession"]["id"]),
        identity: format!("ps_{}", uuid(&v["participantSession"]["id"]).simple()),
        credential: v["rtc"]["credential"].as_str().unwrap().to_string(),
        participant_token: None,
        body: v.clone(),
    };
    connect(&app, cid, &j).await;
    // Join response, participant views and provider metadata carry no clinical data.
    assert!(!v.to_string().contains("SECRET"), "{v}");
    let (_, list) = app
        .get(
            format!("/api/v2/conferences/{cid}/participants"),
            Auth::Bearer(&clinician.token),
        )
        .await;
    assert!(!list.to_string().contains("SECRET"));
    no_clinical_payload(&app).await;
}
