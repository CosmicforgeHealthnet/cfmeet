//! HEALTHCARE_APPOINTMENT through the AppointmentAuthorizationAdapter contract (item 4)
//! and token-revocation fail-closed behaviour for strict security profiles (item 6).

mod common;

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use backend::services::appointment_authorization::HttpAppointmentAuthorizationAdapter;
use chrono::Utc;
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Authority {
    patient: Arc<Mutex<Option<Uuid>>>,
    revoked: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<Value>>>,
}

/// Mock HealthNet appointment authority implementing the documented contract.
async fn authorize(
    State(a): State<Authority>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, String) {
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer svc-token") {
        return (StatusCode::UNAUTHORIZED, "{}".into());
    }
    a.seen.lock().unwrap().push(body.clone());
    let patient = a.patient.lock().unwrap().map(|p| p.to_string());
    let is_patient = body["actor"]["userId"].as_str().map(str::to_string) == patient;
    let now = Utc::now();
    let ok = |extra: Value| {
        let mut v = json!({ "authorized": true, "validUntil": now + chrono::Duration::hours(1) });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        (StatusCode::OK, v.to_string())
    };
    match body["externalContext"].as_str() {
        _ if !is_patient || a.revoked.load(Ordering::SeqCst) => {
            (StatusCode::OK, json!({ "authorized": false }).to_string())
        }
        Some("appt-ok") => ok(json!({})),
        Some("appt-host") => ok(json!({ "role": "OWNER" })),
        Some("appt-expired") => (
            StatusCode::OK,
            json!({ "authorized": true, "validUntil": now - chrono::Duration::minutes(1) })
                .to_string(),
        ),
        Some("appt-future") => ok(json!({ "validFrom": now + chrono::Duration::hours(2) })),
        Some("appt-error") => (StatusCode::INTERNAL_SERVER_ERROR, "boom".into()),
        Some("appt-garbage") => (StatusCode::OK, "not json".into()),
        Some("appt-slow") => {
            tokio::time::sleep(Duration::from_secs(2)).await;
            ok(json!({}))
        }
        _ => (StatusCode::OK, json!({ "authorized": false }).to_string()),
    }
}

async fn start_authority() -> (String, Authority) {
    let authority = Authority::default();
    let app = Router::new()
        .route("/authorize", post(authorize))
        .with_state(authority.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/authorize"), authority)
}

async fn app_with_authority(token: &str) -> Option<(TestApp, Authority)> {
    let (url, authority) = start_authority().await;
    let adapter = HttpAppointmentAuthorizationAdapter::new(
        url,
        Some(token.into()),
        Duration::from_millis(500),
    );
    let app = TestApp::with(TestOptions {
        appointments: Some(Arc::new(adapter)),
        ..TestOptions::default()
    })
    .await?;
    Some((app, authority))
}

/// STANDARD security profile isolates adapter behaviour from the revocation policy.
async fn appointment_conference(app: &TestApp, clinician: &User) -> Uuid {
    app.live_conference(clinician, json!({ "title": "consult", "accessPolicy": "HEALTHCARE_APPOINTMENT", "accessConfig": { "appointmentRef": "APPT-42" } }))
        .await
        .1
}

#[tokio::test]
async fn appt01_active_appointment_is_verified_by_the_authority() {
    let Some((app, authority)) = app_with_authority("svc-token").await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let patient = app.user("patient").await;
    let stranger = app.user("stranger").await;
    *authority.patient.lock().unwrap() = Some(patient.id);
    let cid = appointment_conference(&app, &clinician).await;

    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&patient.token),
            json!({ "externalContext": "appt-ok" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "PARTICIPANT");
    // The authority received server-resolved identity and the space's appointment ref.
    let seen = authority.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen["actor"]["userId"], json!(patient.id));
    assert_eq!(seen["appointmentRef"], "APPT-42");
    assert!(seen["meetingSpaceId"].is_string());

    // The opaque context alone is never proof: a different account with it is denied.
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&stranger.token),
            json!({ "externalContext": "appt-ok" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED")
    );
    for ctx in [
        "appt-expired",
        "appt-future",
        "appt-error",
        "appt-garbage",
        "appt-slow",
        "unknown",
    ] {
        let (s, v) = app
            .join(
                cid,
                Auth::Bearer(&patient.token),
                json!({ "externalContext": ctx }),
            )
            .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED"),
            "{ctx} must fail closed"
        );
    }
    let (s, _) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "anon", "externalContext": "appt-ok" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Reconnect re-verifies: once the authority withdraws authorization, access stops.
    authority.revoked.store(true, Ordering::SeqCst);
    let (s, _) = app
        .join(
            cid,
            Auth::Bearer(&patient.token),
            json!({ "externalContext": "appt-ok" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn appt02_authority_role_is_capped_and_service_token_enforced() {
    let Some((app, authority)) = app_with_authority("svc-token").await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let patient = app.user("patient").await;
    *authority.patient.lock().unwrap() = Some(patient.id);
    let cid = appointment_conference(&app, &clinician).await;
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&patient.token),
            json!({ "externalContext": "appt-host" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["participant"]["role"], "HOST",
        "authority can never mint OWNER"
    );

    // Misconfigured credentials toward the authority → denied (fail closed).
    let Some((bad_app, bad_authority)) = app_with_authority("wrong-token").await else {
        return;
    };
    let clinician = bad_app.user("clinician").await;
    let patient = bad_app.user("patient").await;
    *bad_authority.patient.lock().unwrap() = Some(patient.id);
    let cid = appointment_conference(&bad_app, &clinician).await;
    let (s, _) = bad_app
        .join(
            cid,
            Auth::Bearer(&patient.token),
            json!({ "externalContext": "appt-ok" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(bad_authority.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn appt03_unconfigured_authority_denies_everyone() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let clinician = app.user("clinician").await;
    let patient = app.user("patient").await;
    let cid = appointment_conference(&app, &clinician).await;
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&patient.token),
            json!({ "externalContext": "appt-ok" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED")
    );
    // The space owner (explicit grant) is unaffected.
    assert_eq!(
        app.join(cid, Auth::Bearer(&clinician.token), json!({}))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn rev01_strict_profiles_fail_closed_when_revocation_unverifiable() {
    let Some(app) = TestApp::with(TestOptions {
        redis: RedisMode::Unreachable,
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let owner = app.user("owner").await;
    let user = app.user("user").await;
    // STANDARD profile keeps working without Redis (Redis is not meeting authority).
    let (sid, cid) = app
        .live_conference(&owner, json!({ "title": "s", "accessPolicy": "PUBLIC" }))
        .await;
    assert_eq!(
        app.join(cid, Auth::Bearer(&user.token), json!({})).await.0,
        StatusCode::OK
    );
    // Switch the space to HIGH_SECURITY: authenticated identities now need a verified revocation check.
    let (s, v) = app
        .patch(
            &format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
            json!({ "version": 1, "securityProfile": "HIGH_SECURITY" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVOCATION_CHECK_UNAVAILABLE"
        )
    );
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/lock"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVOCATION_CHECK_UNAVAILABLE"
        ),
        "management fails closed too"
    );
    let (s, view) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(view["view"], "SUMMARY", "reads degrade to the summary view");
    // Guests hold no revocable token and are evaluated by the access policy as usual.
    assert_eq!(
        app.join(cid, Auth::None, json!({ "displayName": "g" }))
            .await
            .0,
        StatusCode::OK
    );
    // Per-space configuration opts other profiles in.
    let (sid2, _) = app
        .live_conference(&owner, json!({ "title": "t", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, _) = app
        .patch(
            &format!("/api/v2/meeting-spaces/{sid2}"),
            Auth::Bearer(&owner.token),
            json!({ "version": 1, "accessConfig": { "requireVerifiedRevocation": true } }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid2}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVOCATION_CHECK_UNAVAILABLE"
        )
    );
}

#[tokio::test]
async fn rev02_revoked_tokens_are_rejected_and_strict_spaces_work_with_redis() {
    if !require_test_redis() {
        return;
    }
    let Some(app) = TestApp::new().await else {
        return;
    };
    let owner = app.user("owner").await;
    let user = app.user("user").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "h", "accessPolicy": "AUTHENTICATED", "securityProfile": "HIGH_SECURITY" }))
        .await;
    assert_eq!(
        app.join(cid, Auth::Bearer(&user.token), json!({})).await.0,
        StatusCode::OK
    );
    app.state
        .token_blacklist
        .blacklist_token(&user.token, 900)
        .await
        .unwrap();
    let (s, v) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED"),
        "logged-out token"
    );
}
