//! Multi-device policy (account-wide linked devices) against a real PostgreSQL and Redis,
//! through the real HTTP router, with real Ed25519 device keys.
//!
//! Covers the Account-authority rows of the backend acceptance matrix: web-only accounts,
//! four web browser profiles (one logical device per profile), the first mobile, mobile
//! replacement (device approval and email step-up), web kept signed in, concurrent mobile
//! replacements (exactly one primary), replayed proofs and old credentials refused, forged
//! and cross-account links refused, device revocation, removing the primary mobile,
//! sign-out everywhere, and replacing a mobile during a live meeting (only its media end).
mod common;

use axum::http::StatusCode;
use base64::Engine as _;
use common::*;
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const PASSWORD: &str = "Str0ng!Passw0rd";

macro_rules! app_with_redis {
    () => {{
        if !require_test_redis() {
            return;
        }
        match TestApp::new().await {
            Some(app) => app,
            None => return,
        }
    }};
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// One client installation: its key never leaves this struct.
struct Device {
    key: SigningKey,
    installation: String,
    class: &'static str,
    platform: &'static str,
    id: Option<Uuid>,
    access: String,
    refresh: String,
    session: Uuid,
}

impl Device {
    fn new(class: &'static str, platform: &'static str) -> Self {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        Self {
            key: SigningKey::from_bytes(&seed),
            installation: format!("inst-{}", Uuid::new_v4().simple()),
            class,
            platform,
            id: None,
            access: String::new(),
            refresh: String::new(),
            session: Uuid::nil(),
        }
    }
    fn web() -> Self {
        Self::new("WEB", "WEB")
    }
    fn mobile(platform: &'static str) -> Self {
        Self::new("MOBILE", platform)
    }
    fn public_key(&self) -> Vec<u8> {
        self.key.verifying_key().to_bytes().to_vec()
    }
    fn thumbprint(&self) -> String {
        backend::services::account_devices::thumbprint(&self.public_key())
    }
    fn proof(
        &self,
        purpose: backend::services::account_devices::ProofPurpose,
        context: &str,
    ) -> Value {
        let mut nonce = [0u8; 18];
        rand::thread_rng().fill_bytes(&mut nonce);
        let nonce = b64(&nonce);
        let issued_at = chrono::Utc::now().timestamp();
        let statement = backend::services::account_devices::proof_statement(
            purpose,
            &self.thumbprint(),
            context,
            issued_at,
            &nonce,
        );
        let sig = self.key.sign(statement.as_bytes());
        json!({ "issuedAt": issued_at, "nonce": nonce, "signature": b64(&sig.to_bytes()) })
    }
}

use backend::services::account_devices::ProofPurpose as P;

async fn password_user(app: &TestApp, email: &str) -> Uuid {
    let id = Uuid::new_v4();
    let hash = backend::services::auth::hash_password(PASSWORD).unwrap();
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO users (id, username, email, password_hash, auth_type, role, status, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'local', 'user', 'active'::user_status, now(), now())",
            [
                id.into(),
                format!("u_{}", &id.simple().to_string()[..10]).into(),
                email.into(),
                hash.into(),
            ],
        ))
        .await
        .unwrap();
    id
}

fn email() -> String {
    format!(
        "dev-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..10]
    )
}

/// Sign in on `d` (password) and declare it. Returns the declare response.
async fn sign_in(app: &TestApp, email: &str, d: &mut Device) -> (StatusCode, Value) {
    let (s, v) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": email, "password": PASSWORD }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    d.access = v["access_token"].as_str().unwrap().into();
    d.refresh = v["refresh_token"].as_str().unwrap().into();
    let claims = app.state.jwt_service.verify_token(&d.access).unwrap();
    d.session = claims.session_id().unwrap();
    let body = json!({
        "installationId": d.installation,
        "deviceClass": d.class,
        "platform": d.platform,
        "label": format!("{} test", d.platform),
        "publicKey": b64(&d.public_key()),
        "proof": d.proof(P::Declare, &d.session.to_string()),
    });
    let (s, v) = app
        .post("/api/v2/me/devices/current", Auth::Bearer(&d.access), body)
        .await;
    if s == StatusCode::OK {
        d.id = Some(uuid(&v["device"]["deviceId"]));
    }
    (s, v)
}

async fn policy(app: &TestApp, d: &Device) -> Value {
    let (s, v) = app
        .get("/api/v2/me/device-policy", Auth::Bearer(&d.access))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

async fn me(app: &TestApp, d: &Device) -> StatusCode {
    app.get("/api/v1/users/me", Auth::Bearer(&d.access)).await.0
}

async fn request_link(app: &TestApp, d: &Device) -> Value {
    let (s, v) = app
        .post(
            "/api/v2/me/device-links",
            Auth::Bearer(&d.access),
            json!({ "proof": d.proof(P::LinkRequest, &d.id.unwrap().to_string()) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v
}

async fn approve(
    app: &TestApp,
    approver: &Device,
    link: &Value,
    new: &Device,
    code: &str,
) -> (StatusCode, Value) {
    let link_id = link["linkId"].as_str().unwrap();
    app.post(
        &format!("/api/v2/me/device-links/{link_id}/approve"),
        Auth::Bearer(&approver.access),
        json!({
            "comparisonCode": code,
            "proof": approver.proof(P::LinkApprove, &format!("{link_id}|{}", new.thumbprint())),
        }),
    )
    .await
}

async fn complete(
    app: &TestApp,
    d: &Device,
    link: &Value,
    email_code: Option<&str>,
) -> (StatusCode, Value) {
    let link_id = link["linkId"].as_str().unwrap();
    let mut body = json!({ "proof": d.proof(P::LinkComplete, link_id) });
    if let Some(c) = email_code {
        body["emailCode"] = json!(c);
    }
    app.post(
        &format!("/api/v2/me/device-links/{link_id}/complete"),
        Auth::Bearer(&d.access),
        body,
    )
    .await
}

/// Link `new` with an approval from `trusted` (the happy path).
async fn link_by_approval(app: &TestApp, trusted: &Device, new: &Device) -> Value {
    let link = request_link(app, new).await;
    let code = link["comparisonCode"].as_str().unwrap().to_string();
    let (s, v) = approve(app, trusted, &link, new, &code).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = complete(app, new, &link, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

async fn emailed_code(app: &TestApp, link_id: &str) -> String {
    let row = app
        .db
        .conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT text_body FROM email_deliveries WHERE category = 'SECURITY_DEVICE_STEP_UP' \
             AND idempotency_key LIKE $1 ORDER BY created_at DESC LIMIT 1",
            [format!("device-step-up:{link_id}:%").into()],
        ))
        .await
        .unwrap()
        .expect("step-up email queued");
    let text: String = row.try_get("", "text_body").unwrap();
    let i = text.find("Your code is ").expect("code in body") + "Your code is ".len();
    text[i..i + 6].to_string()
}

#[tokio::test]
async fn web_only_account_bootstraps_a_trusted_browser_and_stays_fully_usable() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut web = Device::web();
    let (s, v) = sign_in(&app, &e, &mut web).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["device"]["trust"], "TRUSTED");
    assert_eq!(v["device"]["trustedVia"], "BOOTSTRAP");
    assert_eq!(v["device"]["primary"], false);
    let p = policy(&app, &web).await;
    assert_eq!(p["accountState"], "WEB_ONLY");
    assert_eq!(p["maxWebDevices"], 4);
    assert_eq!(p["maxPrimaryMobiles"], 1);
    assert_eq!(p["currentDeviceStatus"], "TRUSTED");
    // No phone, SMS or QR needed; the account works (v1 and v2, meetings included).
    assert_eq!(me(&app, &web).await, StatusCode::OK);
    let owner = backend_user(&app, &e, &web.access).await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "web only", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let (s, v) = app.join(cid, Auth::Bearer(&web.access), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // The session list shows the device binding.
    let (_, sessions) = app
        .get("/api/v2/me/sessions", Auth::Bearer(&web.access))
        .await;
    assert_eq!(sessions["sessions"][0]["deviceId"], json!(web.id.unwrap()));
}

/// The common `User` wrapper for a password user's session token.
async fn backend_user(_app: &TestApp, email: &str, token: &str) -> User {
    User {
        id: Uuid::nil(),
        token: token.to_string(),
        email: email.to_string(),
    }
}

#[tokio::test]
async fn four_browser_profiles_one_device_each_and_the_fifth_is_refused() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut first = Device::web();
    assert_eq!(sign_in(&app, &e, &mut first).await.0, StatusCode::OK);
    let mut others = vec![];
    for _ in 0..3 {
        let mut d = Device::web();
        let (s, v) = sign_in(&app, &e, &mut d).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(
            v["device"]["trust"], "UNTRUSTED",
            "password alone never trusts"
        );
        assert_eq!(
            me(&app, &d).await,
            StatusCode::OK,
            "untrusted web still works"
        );
        others.push(d);
    }
    assert_eq!(policy(&app, &first).await["activeWebDevices"], 4);
    let mut fifth = Device::web();
    let (s, v) = sign_in(&app, &e, &mut fifth).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(code(&v), "WEB_DEVICE_LIMIT_REACHED");

    // The same browser profile signing in again (another tab, or after sign-out) is the
    // same logical device: no extra slot, the previous sign-in is superseded.
    let old_access = first.access.clone();
    let before = first.id;
    let (s, v) = sign_in(&app, &e, &mut first).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(first.id, before);
    assert_eq!(v["endedSessions"], 1);
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&old_access))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(policy(&app, &first).await["activeWebDevices"], 4);

    // Revoking a web device from a trusted device frees a slot; the others keep working.
    let victim = others.pop().unwrap();
    let target = victim.id.unwrap();
    let (s, v) = app
        .post(
            &format!("/api/v2/me/devices/{target}/revoke"),
            Auth::Bearer(&first.access),
            json!({ "proof": first.proof(P::Revoke, &target.to_string()) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["device"]["state"], "REVOKED");
    assert_eq!(me(&app, &victim).await, StatusCode::UNAUTHORIZED);
    for d in &others {
        assert_eq!(me(&app, d).await, StatusCode::OK);
    }
    assert_eq!(me(&app, &first).await, StatusCode::OK);
    let (s, v) = sign_in(&app, &e, &mut fifth).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // An untrusted device cannot revoke another device.
    let target = first.id.unwrap();
    let (s, v) = app
        .post(
            &format!("/api/v2/me/devices/{target}/revoke"),
            Auth::Bearer(&fifth.access),
            json!({ "proof": fifth.proof(P::Revoke, &target.to_string()) }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "DEVICE_NOT_TRUSTED");
}

#[tokio::test]
async fn first_mobile_becomes_primary_and_a_new_mobile_replaces_it_without_touching_web() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut web_a = Device::web();
    sign_in(&app, &e, &mut web_a).await;
    let mut web_b = Device::web();
    sign_in(&app, &e, &mut web_b).await;
    link_by_approval(&app, &web_a, &web_b).await;
    assert_eq!(policy(&app, &web_b).await["currentDeviceStatus"], "TRUSTED");

    // First mobile: pending until approved; its sign-in has no authority meanwhile.
    let mut mobile_a = Device::mobile("ANDROID");
    let (s, v) = sign_in(&app, &e, &mut mobile_a).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["device"]["state"], "REPLACEMENT_PENDING");
    let (s, v) = app
        .get("/api/v1/users/me", Auth::Bearer(&mobile_a.access))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "DEVICE_REPLACEMENT_PENDING");
    assert_eq!(
        app.get("/api/v2/me/sessions", Auth::Bearer(&mobile_a.access))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        policy(&app, &web_a).await["accountState"],
        "DEVICE_REPLACEMENT_PENDING"
    );

    // A wrong comparison code is refused; the listing never shows the code.
    let link = request_link(&app, &mobile_a).await;
    let (_, pending) = app
        .get("/api/v2/me/device-links", Auth::Bearer(&web_a.access))
        .await;
    assert_eq!(pending["links"].as_array().unwrap().len(), 1);
    assert!(pending["links"][0].get("comparisonCode").is_none());
    let (s, _) = approve(&app, &web_a, &link, &mobile_a, "000000x").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Completing before approval is refused.
    let (s, v) = complete(&app, &mobile_a, &link, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let code_a = link["comparisonCode"].as_str().unwrap().to_string();
    assert_eq!(
        approve(&app, &web_a, &link, &mobile_a, &code_a).await.0,
        StatusCode::OK
    );
    let (s, v) = complete(&app, &mobile_a, &link, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["device"]["primary"], true);
    assert_eq!(v["replacedDeviceId"], Value::Null);
    // N0-10 (S-9): the owner's notice was committed with the link, under a deterministic key.
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM email_deliveries WHERE category = 'NOTIFICATION' AND idempotency_key = 'device-linked:{}'",
            link["linkId"].as_str().unwrap()
        ))
        .await,
        1
    );
    assert_eq!(me(&app, &mobile_a).await, StatusCode::OK);
    assert_eq!(policy(&app, &web_a).await["accountState"], "MOBILE_PRIMARY");

    // Mobile B, nobody at hand to approve: email step-up (account mailbox), never SMS.
    let mut mobile_b = Device::mobile("IOS");
    let (s, v) = sign_in(&app, &e, &mut mobile_b).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["device"]["state"], "REPLACEMENT_PENDING");
    assert_eq!(
        me(&app, &mobile_a).await,
        StatusCode::OK,
        "A stays primary while B is pending"
    );
    let link = request_link(&app, &mobile_b).await;
    let link_id = link["linkId"].as_str().unwrap().to_string();
    let (s, v) = app
        .post(
            &format!("/api/v2/me/device-links/{link_id}/email-code"),
            Auth::Bearer(&mobile_b.access),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let emailed = emailed_code(&app, &link_id).await;
    let (s, _) = complete(&app, &mobile_b, &link, Some("999999")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let old_refresh_a = mobile_a.refresh.clone();
    let (s, v) = complete(&app, &mobile_b, &link, Some(&emailed)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["device"]["primary"], true);
    assert_eq!(v["device"]["trustedVia"], "EMAIL_STEP_UP");
    assert_eq!(v["replacedDeviceId"], json!(mobile_a.id.unwrap()));
    assert_eq!(v["endedSessions"], 1);

    // Mobile A is refused everywhere, including its refresh token; web stays connected.
    assert_eq!(me(&app, &mobile_a).await, StatusCode::UNAUTHORIZED);
    let proof = mobile_a.proof(P::Refresh, &mobile_a.session.to_string());
    let (s, _) = app
        .post(
            "/api/v1/auth/refresh",
            Auth::None,
            json!({ "refresh_token": old_refresh_a, "deviceProof": proof }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&app, &web_a).await, StatusCode::OK);
    assert_eq!(me(&app, &web_b).await, StatusCode::OK);
    assert_eq!(me(&app, &mobile_b).await, StatusCode::OK);
    let (_, list) = app
        .get(
            "/api/v2/me/devices?includeRevoked=true",
            Auth::Bearer(&web_a.access),
        )
        .await;
    let a = list["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["deviceId"] == json!(mobile_a.id.unwrap()))
        .unwrap()
        .clone();
    assert_eq!(a["state"], "REVOKED");
    assert_eq!(a["revokeReason"], "DEVICE_REPLACED");
    assert_eq!(a["replacedByDeviceId"], json!(mobile_b.id.unwrap()));
    // Audit and notice: the surviving devices can read the replacement.
    let (_, ev) = app
        .get("/api/v2/me/security-events", Auth::Bearer(&web_b.access))
        .await;
    assert!(ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["eventType"] == "PRIMARY_MOBILE_REPLACED"));
    assert!(
        app.scalar("SELECT count(*) FROM email_deliveries WHERE subject LIKE 'A new phone%'")
            .await
            >= 1
    );
    // The Account authority still holds exactly one primary mobile.
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM account_devices WHERE device_class='MOBILE' AND state='ACTIVE'"
        )
        .await,
        1
    );

    // The same installed app signing in again rotates, it is not a new phone.
    let id_b = mobile_b.id;
    let (s, v) = sign_in(&app, &e, &mut mobile_b).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(mobile_b.id, id_b);
    assert_eq!(v["device"]["primary"], true);
    assert_eq!(me(&app, &web_a).await, StatusCode::OK);

    // Removing the primary mobile leaves a working web-only account.
    let target = mobile_b.id.unwrap();
    let (s, v) = app
        .post(
            &format!("/api/v2/me/devices/{target}/revoke"),
            Auth::Bearer(&web_a.access),
            json!({ "proof": web_a.proof(P::Revoke, &target.to_string()) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(me(&app, &mobile_b).await, StatusCode::UNAUTHORIZED);
    assert_eq!(policy(&app, &web_a).await["accountState"], "WEB_ONLY");
    assert_eq!(me(&app, &web_b).await, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_mobile_replacements_leave_exactly_one_primary() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut web = Device::web();
    sign_in(&app, &e, &mut web).await;
    let mut phones = vec![];
    for p in ["ANDROID", "IOS", "ANDROID"] {
        let mut m = Device::mobile(p);
        sign_in(&app, &e, &mut m).await;
        let link = request_link(&app, &m).await;
        let c = link["comparisonCode"].as_str().unwrap().to_string();
        assert_eq!(approve(&app, &web, &link, &m, &c).await.0, StatusCode::OK);
        phones.push((m, link));
    }
    let results = futures_util::future::join_all(
        phones.iter().map(|(m, link)| complete(&app, m, link, None)),
    )
    .await;
    for (s, v) in &results {
        assert_eq!(*s, StatusCode::OK, "{v}");
    }
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM account_devices WHERE device_class='MOBILE' AND state='ACTIVE'"
        )
        .await,
        1
    );
    let mut ok = 0;
    for (m, _) in &phones {
        if me(&app, m).await == StatusCode::OK {
            ok += 1;
        }
    }
    assert_eq!(ok, 1, "exactly one phone keeps authority");
    assert_eq!(me(&app, &web).await, StatusCode::OK);
}

#[tokio::test]
async fn proofs_are_single_use_and_bound_refresh_needs_the_device_key() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut web = Device::web();
    sign_in(&app, &e, &mut web).await;
    // Refresh of a device-bound sign-in: refused without a proof, refused with a proof of
    // another key, accepted with the device's proof, and the proof cannot be replayed.
    let (s, v) = app
        .post(
            "/api/v1/auth/refresh",
            Auth::None,
            json!({ "refresh_token": web.refresh }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let thief = Device::web();
    let (s, _) = app
        .post(
            "/api/v1/auth/refresh",
            Auth::None,
            json!({ "refresh_token": web.refresh, "deviceProof": thief.proof(P::Refresh, &web.session.to_string()) }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let proof = web.proof(P::Refresh, &web.session.to_string());
    let (s, v) = app
        .post(
            "/api/v1/auth/refresh",
            Auth::None,
            json!({ "refresh_token": web.refresh, "deviceProof": proof }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let next = v["refresh_token"].as_str().unwrap().to_string();
    let (s, _) = app
        .post(
            "/api/v1/auth/refresh",
            Auth::None,
            json!({ "refresh_token": next, "deviceProof": proof }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "replayed proof refused");
    // A proof for one purpose is useless for another, and a stale one is refused.
    let mut other = Device::web();
    let (s, _) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": e, "password": PASSWORD }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": e, "password": PASSWORD }),
        )
        .await;
    other.access = v["access_token"].as_str().unwrap().into();
    other.session = app
        .state
        .jwt_service
        .verify_token(&other.access)
        .unwrap()
        .session_id()
        .unwrap();
    let wrong_purpose = other.proof(P::Refresh, &other.session.to_string());
    let body = |proof: Value| {
        json!({
            "installationId": other.installation, "deviceClass": "WEB", "platform": "WEB",
            "publicKey": b64(&other.public_key()), "proof": proof,
        })
    };
    let (s, v) = app
        .post(
            "/api/v2/me/devices/current",
            Auth::Bearer(&other.access),
            body(wrong_purpose),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "DEVICE_PROOF_INVALID")
    );
    let mut stale = other.proof(P::Declare, &other.session.to_string());
    stale["issuedAt"] = json!(chrono::Utc::now().timestamp() - 3600);
    let (s, _) = app
        .post(
            "/api/v2/me/devices/current",
            Auth::Bearer(&other.access),
            body(stale),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // A key already bound to a live device cannot be bound again (forged identity).
    let stolen = json!({
        "installationId": other.installation, "deviceClass": "WEB", "platform": "WEB",
        "publicKey": b64(&web.public_key()), "proof": web.proof(P::Declare, &other.session.to_string()),
    });
    let (s, v) = app
        .post(
            "/api/v2/me/devices/current",
            Auth::Bearer(&other.access),
            stolen,
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "DEVICE_KEY_IN_USE"));
}

#[tokio::test]
async fn forged_and_cross_account_links_are_refused() {
    let app = app_with_redis!();
    let (e1, e2) = (email(), email());
    password_user(&app, &e1).await;
    password_user(&app, &e2).await;
    let mut alice = Device::web();
    sign_in(&app, &e1, &mut alice).await;
    let mut bob = Device::web();
    sign_in(&app, &e2, &mut bob).await;
    let mut attacker = Device::web(); // knows Alice's password
    sign_in(&app, &e1, &mut attacker).await;
    let link = request_link(&app, &attacker).await;
    let c = link["comparisonCode"].as_str().unwrap().to_string();

    // Bob's trusted device cannot approve Alice's link (non-enumerating 410).
    let (s, v) = approve(&app, &bob, &link, &attacker, &c).await;
    assert_eq!((s, code(&v)), (StatusCode::GONE, "DEVICE_LINK_INVALID"));
    // An untrusted device cannot approve, even with the code (a "stolen screenshot").
    let mut second_attacker = Device::web();
    sign_in(&app, &e1, &mut second_attacker).await;
    let (s, v) = approve(&app, &second_attacker, &link, &attacker, &c).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "DEVICE_NOT_TRUSTED"));
    // An approval signed by the wrong key is refused.
    let link_id = link["linkId"].as_str().unwrap();
    let (s, _) = app
        .post(
            &format!("/api/v2/me/device-links/{link_id}/approve"),
            Auth::Bearer(&alice.access),
            json!({ "comparisonCode": c, "proof": attacker.proof(P::LinkApprove, &format!("{link_id}|{}", attacker.thumbprint())) }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Another device of the same account cannot complete the link (bound to the requester).
    let (s, _) = complete(&app, &second_attacker, &link, None).await;
    assert_eq!(s, StatusCode::GONE);
    // Alice rejects; it can no longer be approved or completed.
    let (s, _) = app
        .post(
            &format!("/api/v2/me/device-links/{link_id}/reject"),
            Auth::Bearer(&alice.access),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        approve(&app, &alice, &link, &attacker, &c).await.0,
        StatusCode::GONE
    );
    assert_eq!(
        complete(&app, &attacker, &link, None).await.0,
        StatusCode::GONE
    );
    assert_eq!(
        policy(&app, &attacker).await["currentDeviceStatus"],
        "UNTRUSTED"
    );
    // Audit records the refusals.
    assert!(
        app.scalar(
            "SELECT count(*) FROM account_security_events WHERE event_type='DEVICE_LINK_REJECTED'"
        )
        .await
            >= 1
    );
}

#[tokio::test]
async fn sign_out_everywhere_revokes_every_device() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut web = Device::web();
    sign_in(&app, &e, &mut web).await;
    let mut phone = Device::mobile("ANDROID");
    sign_in(&app, &e, &mut phone).await;
    link_by_approval(&app, &web, &phone).await;
    let (s, _) = app
        .post(
            "/api/v2/me/sessions/revoke-all",
            Auth::Bearer(&web.access),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me(&app, &web).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&app, &phone).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.scalar("SELECT count(*) FROM account_devices WHERE state <> 'REVOKED'")
            .await,
        0
    );
    // Re-linking afterwards is account recovery: the account had trusted devices, so a new
    // browser is untrusted until the email step-up (no silent trust restore).
    let mut fresh = Device::web();
    let (s, v) = sign_in(&app, &e, &mut fresh).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["device"]["trust"], "UNTRUSTED");
    let p = policy(&app, &fresh).await;
    assert_eq!(p["linkingMethods"], json!(["EMAIL_STEP_UP"]));
}

#[tokio::test]
async fn unclassified_sign_ins_keep_working_and_never_count() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    // Existing clients that never declare a device are untouched by the policy.
    let mut legacy = vec![];
    for _ in 0..6 {
        let (s, v) = app
            .post(
                "/api/v1/auth/login",
                Auth::None,
                json!({ "email": e, "password": PASSWORD }),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        legacy.push(v["access_token"].as_str().unwrap().to_string());
    }
    for t in &legacy {
        assert_eq!(
            app.get("/api/v1/users/me", Auth::Bearer(t)).await.0,
            StatusCode::OK
        );
    }
    let mut web = Device::web();
    assert_eq!(sign_in(&app, &e, &mut web).await.0, StatusCode::OK);
    assert_eq!(policy(&app, &web).await["activeWebDevices"], 1);
    // An unclassified sign-in older than the re-auth window cannot declare a device.
    // (Its tokens carry the sign-in's auth_time, which refresh never advances.)
    let (fresh_access, _, old_session) = legacy_sign_in(&app, &e).await;
    let old_access = aged_access_token(&app, &fresh_access, 3600).await;
    let late = Device::web();
    let (s, v) = app
        .post(
            "/api/v2/me/devices/current",
            Auth::Bearer(&old_access),
            json!({
                "installationId": late.installation,
                "deviceClass": late.class,
                "platform": late.platform,
                "label": "late",
                "publicKey": b64(&late.public_key()),
                "proof": late.proof(P::Declare, &old_session.to_string()),
            }),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(code(&v), "DEVICE_DECLARATION_REFUSED", "{v}");
    // Compatibility mode (no enforcement date): the old sign-in still works.
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&old_access))
            .await
            .0,
        StatusCode::OK
    );
}

/// Move a sign-in's authentication back by `secs` (the database clock decides windows).
async fn backdate_sign_in(app: &TestApp, session: Uuid, secs: i64) {
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_sessions SET authenticated_at = authenticated_at - make_interval(secs => $2) \
             WHERE id = $1",
            [session.into(), (secs as f64).into()],
        ))
        .await
        .unwrap();
}

/// The sign-in behind `access`, aged by `secs`: its database authentication time and a
/// token carrying that auth_time (what an hour-old sign-in holds after refreshes).
async fn aged_access_token(app: &TestApp, access: &str, secs: i64) -> String {
    let claims = app.state.jwt_service.verify_token(access).unwrap();
    let session = claims.session_id().unwrap();
    backdate_sign_in(app, session, secs).await;
    let user: Uuid = claims.sub.parse().unwrap();
    let auth_time = claims.auth_time.unwrap() - secs;
    app.state
        .jwt_service
        .session_tokens(user, &claims.role, auth_time, session, Uuid::new_v4())
        .unwrap()
        .0
}

/// A password sign-in that never declares a device (legacy client). Returns (access,
/// refresh, session id).
async fn legacy_sign_in(app: &TestApp, email: &str) -> (String, String, Uuid) {
    let (s, v) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": email, "password": PASSWORD }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let access = v["access_token"].as_str().unwrap().to_string();
    let refresh = v["refresh_token"].as_str().unwrap().to_string();
    let session = app
        .state
        .jwt_service
        .verify_token(&access)
        .unwrap()
        .session_id()
        .unwrap();
    (access, refresh, session)
}

/// Section 5 migration: a legacy (unclassified) sign-in, e.g. on the old phone, cannot
/// outlive a primary-mobile replacement or a device revocation. Classified web devices
/// stay connected; the ended sign-ins are audited with the replacement.
#[tokio::test]
async fn replacement_and_revocation_end_unclassified_sign_ins_but_not_web_devices() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut web = Device::web();
    sign_in(&app, &e, &mut web).await;
    let mut web2 = Device::web();
    sign_in(&app, &e, &mut web2).await;
    link_by_approval(&app, &web, &web2).await;
    let mut phone_a = Device::mobile("ANDROID");
    sign_in(&app, &e, &mut phone_a).await;
    link_by_approval(&app, &web, &phone_a).await;
    // The old phone also holds a legacy sign-in (an app version without the device flow).
    let (legacy_access, legacy_refresh, legacy_session) = legacy_sign_in(&app, &e).await;
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&legacy_access))
            .await
            .0,
        StatusCode::OK
    );

    // A new phone replaces phone A.
    let mut phone_b = Device::mobile("IOS");
    let (s, v) = sign_in(&app, &e, &mut phone_b).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["device"]["state"], "REPLACEMENT_PENDING");
    let done = link_by_approval(&app, &web, &phone_b).await;
    assert!(done["replacedDeviceId"].is_string(), "{done}");

    assert_eq!(me(&app, &phone_a).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&legacy_access))
            .await
            .0,
        StatusCode::UNAUTHORIZED,
        "the legacy sign-in ends with the replacement"
    );
    let (s, _) = app
        .post(
            "/api/v1/auth/refresh",
            Auth::None,
            json!({ "refresh_token": legacy_refresh }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "and cannot be refreshed");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_sessions WHERE id = '{legacy_session}' \
             AND revoke_reason = 'DEVICE_REPLACED'"
        ))
        .await,
        1
    );
    // Both web devices and the new primary keep working.
    assert_eq!(me(&app, &web).await, StatusCode::OK);
    assert_eq!(me(&app, &web2).await, StatusCode::OK);
    assert_eq!(me(&app, &phone_b).await, StatusCode::OK);
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM account_devices WHERE device_class = 'MOBILE' AND state = 'ACTIVE'"
        )
        .await,
        1
    );

    // Revoking a web device ends the legacy sign-ins too, never the other devices.
    let (legacy2, _, _) = legacy_sign_in(&app, &e).await;
    let web2_id = web2.id.unwrap();
    let (s, v) = app
        .post(
            &format!("/api/v2/me/devices/{web2_id}/revoke"),
            Auth::Bearer(&web.access),
            json!({ "proof": web.proof(P::Revoke, &web2_id.to_string()) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(me(&app, &web2).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&legacy2)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(me(&app, &web).await, StatusCode::OK);
    assert_eq!(me(&app, &phone_b).await, StatusCode::OK);
    // Signing in again works (Meetings and Calling stay independently available).
    let (fresh, _, _) = legacy_sign_in(&app, &e).await;
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&fresh)).await.0,
        StatusCode::OK
    );
}

/// Section 5 migration: with `ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM` reached, a sign-in
/// that did not declare its device within the declaration window is refused (requests and
/// refresh) with 403 DEVICE_DECLARATION_REQUIRED; a fresh sign-in can still declare, and
/// declared web and mobile devices are unaffected. Before the date nothing changes.
#[tokio::test]
async fn declaration_enforcement_ends_legacy_sign_ins_after_the_owner_date() {
    let base = app_with_redis!();
    let future = base.with_auth_env(&[(
        "ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM",
        "2999-01-01T00:00:00Z",
    )]);
    let e = email();
    password_user(&future, &e).await;
    let (fresh, _, _) = legacy_sign_in(&future, &e).await;
    let old_access = aged_access_token(&future, &fresh, 3600).await;
    assert_eq!(
        future
            .get("/api/v1/users/me", Auth::Bearer(&old_access))
            .await
            .0,
        StatusCode::OK,
        "a future enforcement date changes nothing yet"
    );

    let app = future.with_auth_env(&[(
        "ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM",
        "2020-01-01T00:00:00Z",
    )]);
    let mut web = Device::web();
    assert_eq!(sign_in(&app, &e, &mut web).await.0, StatusCode::OK);
    let mut phone = Device::mobile("ANDROID");
    sign_in(&app, &e, &mut phone).await;
    link_by_approval(&app, &web, &phone).await;

    // The legacy sign-in past its window: refused, also on refresh.
    let (s, v) = app.get("/api/v1/users/me", Auth::Bearer(&old_access)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "DEVICE_DECLARATION_REQUIRED", "{v}");
    let (acc2, refresh2, s2) = legacy_sign_in(&app, &e).await;
    // Within the window an undeclared sign-in still works (it can declare now) ...
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&acc2)).await.0,
        StatusCode::OK
    );
    // ... but once the window passes without a declaration it has no authority.
    let acc2 = aged_access_token(&app, &acc2, 3600).await;
    let _ = s2;
    let (s, v) = app.get("/api/v1/users/me", Auth::Bearer(&acc2)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let (s, v) = app
        .post(
            "/api/v1/auth/refresh",
            Auth::None,
            json!({ "refresh_token": refresh2 }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "DEVICE_DECLARATION_REQUIRED", "{v}");
    // The device endpoints still answer it, so the client can explain what to do.
    let (s, v) = app
        .get("/api/v2/me/device-policy", Auth::Bearer(&acc2))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["currentDeviceStatus"], "UNCLASSIFIED", "{v}");
    // Declared devices are unaffected, even when their sign-in is old.
    web.access = aged_access_token(&app, &web.access, 3600).await;
    assert_eq!(me(&app, &web).await, StatusCode::OK);
    assert_eq!(me(&app, &phone).await, StatusCode::OK);
    // A fresh sign-in that declares right away keeps working.
    let mut web2 = Device::web();
    assert_eq!(sign_in(&app, &e, &mut web2).await.0, StatusCode::OK);
    web2.access = aged_access_token(&app, &web2.access, 3600).await;
    assert_eq!(me(&app, &web2).await, StatusCode::OK);
}

#[tokio::test]
async fn replacing_a_mobile_during_a_meeting_ends_only_its_media() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut web = Device::web();
    sign_in(&app, &e, &mut web).await;
    let mut phone_a = Device::mobile("ANDROID");
    sign_in(&app, &e, &mut phone_a).await;
    link_by_approval(&app, &web, &phone_a).await;
    let host = app.user("host").await;
    let (_, cid) = app
        .live_conference(
            &host,
            json!({ "title": "devices", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let (s, w) = app.join(cid, Auth::Bearer(&web.access), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{w}");
    let (s, m) = app
        .join(cid, Auth::Bearer(&phone_a.access), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{m}");
    let live = format!(
        "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state IN ('PENDING','CONNECTED')"
    );
    assert_eq!(
        app.scalar(&live).await,
        2,
        "one participant, two device sessions"
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(DISTINCT conference_participant_id) FROM participant_sessions WHERE conference_id='{cid}'")).await,
        1,
        "the same account is one logical participant"
    );

    let mut phone_b = Device::mobile("IOS");
    sign_in(&app, &e, &mut phone_b).await;
    let v = link_by_approval(&app, &web, &phone_b).await;
    assert_eq!(v["replacedDeviceId"], json!(phone_a.id.unwrap()));
    assert_eq!(
        app.scalar(&live).await,
        1,
        "only the old phone's media ended"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND end_reason='ACCOUNT_SESSION_ENDED'"
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='rtc.participant_remove_requested' AND payload->>'reason'='ACCOUNT_SESSION_ENDED'"
        ))
        .await,
        1,
        "provider removal queued for the old phone only"
    );
    // The old phone cannot rejoin; the web device's session continues.
    let (s, _) = app
        .join(cid, Auth::Bearer(&phone_a.access), json!({}))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&app, &web).await, StatusCode::OK);
}

#[tokio::test]
async fn a_new_device_reads_and_cancels_its_own_link_request() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut trusted = Device::web();
    sign_in(&app, &e, &mut trusted).await;
    let mut new = Device::web();
    sign_in(&app, &e, &mut new).await;
    let link = request_link(&app, &new).await;
    let link_id = link["linkId"].as_str().unwrap().to_string();
    let (s, v) = app
        .get(
            &format!("/api/v2/me/device-links/{link_id}"),
            Auth::Bearer(&new.access),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "PENDING");
    assert_eq!(v["comparisonCode"], link["comparisonCode"]);
    // Another device of the account cannot read it (non-enumerating).
    let (s, _) = app
        .get(
            &format!("/api/v2/me/device-links/{link_id}"),
            Auth::Bearer(&trusted.access),
        )
        .await;
    assert_eq!(s, StatusCode::GONE);
    let (s, v) = app
        .post(
            &format!("/api/v2/me/device-links/{link_id}/cancel"),
            Auth::Bearer(&new.access),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "CANCELLED");
    let c = link["comparisonCode"].as_str().unwrap().to_string();
    assert_eq!(
        approve(&app, &trusted, &link, &new, &c).await.0,
        StatusCode::GONE
    );
}

/// Gap audit A5: a password-only (undeclared) sign-in cannot end a trusted device's
/// sign-in nor sign out everywhere; it can still end itself, and the trusted device can
/// end it.
#[tokio::test]
async fn gap_a_password_only_sign_in_cannot_end_trusted_devices() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let mut web = Device::web();
    let (s, v) = sign_in(&app, &e, &mut web).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["device"]["trust"], "TRUSTED");
    let (legacy, _, legacy_session) = legacy_sign_in(&app, &e).await;

    let (s, v) = app
        .post(
            &format!("/api/v2/me/sessions/{}/revoke", web.session),
            Auth::Bearer(&legacy),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "DEVICE_NOT_TRUSTED");
    let (s, v) = app
        .post(
            "/api/v2/me/sessions/revoke-all",
            Auth::Bearer(&legacy),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "DEVICE_NOT_TRUSTED");
    assert_eq!(me(&app, &web).await, StatusCode::OK);

    // The trusted device ends the password-only sign-in.
    let (s, v) = app
        .post(
            &format!("/api/v2/me/sessions/{legacy_session}/revoke"),
            Auth::Bearer(&web.access),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&legacy)).await.0,
        StatusCode::UNAUTHORIZED
    );

    // A password-only sign-in may always end itself.
    let (legacy2, _, legacy2_session) = legacy_sign_in(&app, &e).await;
    let (s, v) = app
        .post(
            &format!("/api/v2/me/sessions/{legacy2_session}/revoke"),
            Auth::Bearer(&legacy2),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// Gap audit A10: a sign-in past its absolute lifetime is recorded EXPIRED by the sweep.
#[tokio::test]
async fn gap_expired_sign_ins_are_swept() {
    let app = app_with_redis!();
    let e = email();
    password_user(&app, &e).await;
    let (_access, _, session) = legacy_sign_in(&app, &e).await;
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_sessions SET authenticated_at = now() - interval '31 days', \
             absolute_expires_at = now() - interval '1 minute' WHERE id = $1",
            [session.into()],
        ))
        .await
        .unwrap();
    let ended = backend::services::account_security::sweep_expired_sessions(&app.state)
        .await
        .unwrap();
    assert!(ended >= 1);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_sessions WHERE id = '{session}' AND revoke_reason = 'EXPIRED' AND revoked_at IS NOT NULL"
        ))
        .await,
        1
    );
}
