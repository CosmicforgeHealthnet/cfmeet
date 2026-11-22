mod common;
use async_trait::async_trait;
use backend::{
    error::ApiResult,
    services::account_phone::{self as phone, VerificationProvider},
};
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

struct Provider;

#[tokio::test]
async fn phone_http_binding_signin_and_removal_require_real_session_proofs() {
    let Some(mut app) = TestApp::new().await else {
        return;
    };
    app.state.phone_verification = Some(std::sync::Arc::new(Provider));
    app.router = backend::routes::create_routes(app.state.clone())
        .route_layer(axum::middleware::from_fn(common::route_coverage::record));
    let mut user = app.user("phone-http-owner").await;
    app.trust_device(&mut user).await;
    let step = app
        .step_up_token(&user.token, user.id, "MANAGE_PHONE")
        .await;
    let (status, challenge) = app
        .post(
            "/api/v2/me/phone/verification",
            Auth::Bearer(&user.token),
            serde_json::json!({"phoneNumber":"+393331234568","stepUpToken":step}),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{challenge}");
    let confirm = serde_json::json!({"challengeId":challenge["challengeId"],"code":"123456"});
    let (status, rejected) = app
        .post(
            "/api/v2/me/phone/confirm",
            Auth::Bearer(&user.token),
            serde_json::json!({"challengeId":challenge["challengeId"],"code":"000000"}),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{rejected}");
    let (status, verified) = app
        .post(
            "/api/v2/me/phone/confirm",
            Auth::Bearer(&user.token),
            confirm.clone(),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{verified}");
    assert_eq!(verified["status"], "VERIFIED");
    assert_eq!(verified["discoverable"], false);
    assert_eq!(
        app.post(
            "/api/v2/me/phone/confirm",
            Auth::Bearer(&user.token),
            confirm
        )
        .await
        .0,
        axum::http::StatusCode::BAD_REQUEST
    );
    let (status, visible) = app
        .patch(
            "/api/v2/me/phone",
            Auth::Bearer(&user.token),
            serde_json::json!({"discoverable":true}),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{visible}");
    assert_eq!(visible["discoverable"], true);
    let (status, current) = app.get("/api/v2/me/phone", Auth::Bearer(&user.token)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(current["phoneNumber"], "+393331234568");
    let (status, signin) = app
        .post(
            "/api/v1/auth/phone/request",
            Auth::None,
            serde_json::json!({"phoneNumber":"+393331234568"}),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{signin}");
    let proof = serde_json::json!({"challengeId":signin["challengeId"],"code":"123456"});
    let (status, session) = app
        .post("/api/v1/auth/phone/verify", Auth::None, proof.clone())
        .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{session}");
    let signed_in = session["access_token"]
        .as_str()
        .expect("phone sign-in token");
    assert_eq!(
        app.post("/api/v1/auth/phone/verify", Auth::None, proof)
            .await
            .0,
        axum::http::StatusCode::BAD_REQUEST
    );
    let step = app
        .step_up_token(&user.token, user.id, "MANAGE_PHONE")
        .await;
    let (status, result) = app
        .call(
            axum::http::Method::DELETE,
            "/api/v2/me/phone",
            Auth::Bearer(&user.token),
            Some(serde_json::json!({"stepUpToken":step})),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{result}");
    assert!(phone::find(&app.db.conn, "+393331234568", false)
        .await
        .unwrap()
        .is_none());
    for token in [&user.token[..], signed_in] {
        assert_eq!(
            app.get("/api/v2/me/phone", Auth::Bearer(token)).await.0,
            axum::http::StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn phone_routes_require_an_account_and_stay_disabled_without_provider_credentials() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let user = app.user("phone-routes").await;
    assert_eq!(
        app.get("/api/v2/me/phone", Auth::None).await.0,
        axum::http::StatusCode::UNAUTHORIZED
    );
    let (status, body) = app.get("/api/v2/me/phone", Auth::Bearer(&user.token)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["status"], "PROVIDER_BLOCKED");
    for (path, body) in [
        (
            "/api/v2/me/phone/verification",
            serde_json::json!({"phoneNumber":"+393331234567","stepUpToken":"unused"}),
        ),
        (
            "/api/v2/me/phone/confirm",
            serde_json::json!({"challengeId":Uuid::new_v4(),"code":"123456"}),
        ),
        (
            "/api/v1/auth/phone/request",
            serde_json::json!({"phoneNumber":"+393331234567"}),
        ),
        (
            "/api/v1/auth/phone/verify",
            serde_json::json!({"challengeId":Uuid::new_v4(),"code":"123456"}),
        ),
    ] {
        let (status, _) = app.post(path, Auth::Bearer(&user.token), body).await;
        assert!(
            status.is_client_error() || status.is_server_error(),
            "{path}: {status}"
        );
    }
    let (status, _) = app
        .patch(
            "/api/v2/me/phone",
            Auth::Bearer(&user.token),
            serde_json::json!({"discoverable":true}),
        )
        .await;
    assert!(status.is_client_error() || status.is_server_error());
    let (status, _) = app
        .call(
            axum::http::Method::DELETE,
            "/api/v2/me/phone",
            Auth::Bearer(&user.token),
            Some(serde_json::json!({"stepUpToken":"invalid"})),
        )
        .await;
    assert!(status.is_client_error());
}
#[async_trait]
impl VerificationProvider for Provider {
    async fn start(&self, _: &str) -> ApiResult<String> {
        Ok(format!("VE{}", "a".repeat(32)))
    }
    async fn approved(&self, _: &str, _: &str, code: &str) -> ApiResult<bool> {
        Ok(code == "123456")
    }
}

#[tokio::test]
async fn phone_binding_requires_the_original_session_and_a_single_provider_approval() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let user = app.user("phone-owner").await;
    let other = app.user("phone-other").await;
    let session = Uuid::new_v4();
    let challenge = phone::start(
        &app.db.conn,
        &Provider,
        user.id,
        Some(session),
        "+393331234567",
    )
    .await
    .unwrap();
    assert!(phone::verify(
        &app.db.conn,
        &Provider,
        challenge,
        Some((other.id, session)),
        "123456"
    )
    .await
    .is_err());
    assert!(phone::verify(
        &app.db.conn,
        &Provider,
        challenge,
        Some((user.id, Uuid::new_v4())),
        "123456"
    )
    .await
    .is_err());
    assert!(
        phone::verify(&app.db.conn, &Provider, challenge, None, "123456")
            .await
            .is_err()
    );
    assert_eq!(
        phone::verify(
            &app.db.conn,
            &Provider,
            challenge,
            Some((user.id, session)),
            "123456"
        )
        .await
        .unwrap(),
        user.id
    );
    assert!(phone::verify(
        &app.db.conn,
        &Provider,
        challenge,
        Some((user.id, session)),
        "123456"
    )
    .await
    .is_err());
    assert_eq!(
        phone::find(&app.db.conn, "+393331234567", false)
            .await
            .unwrap(),
        Some(user.id)
    );
    assert_eq!(
        phone::find(&app.db.conn, "+393331234567", true)
            .await
            .unwrap(),
        None
    );
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_phone_bindings SET discoverable=true WHERE user_id=$1",
            vec![user.id.into()],
        ))
        .await
        .unwrap();
    assert_eq!(
        phone::find(&app.db.conn, "+393331234567", true)
            .await
            .unwrap(),
        Some(user.id)
    );
}

#[tokio::test]
async fn wrong_phone_codes_lock_after_five_attempts_and_a_removed_number_cannot_sign_in() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let user = app.user("phone-attempts").await;
    let session = Uuid::new_v4();
    let first = phone::start(
        &app.db.conn,
        &Provider,
        user.id,
        Some(session),
        "+393331234567",
    )
    .await
    .unwrap();
    for _ in 0..5 {
        assert!(phone::verify(
            &app.db.conn,
            &Provider,
            first,
            Some((user.id, session)),
            "000000"
        )
        .await
        .is_err());
    }
    assert!(phone::verify(
        &app.db.conn,
        &Provider,
        first,
        Some((user.id, session)),
        "123456"
    )
    .await
    .is_err());
    let linked = phone::start(
        &app.db.conn,
        &Provider,
        user.id,
        Some(session),
        "+393331234567",
    )
    .await
    .unwrap();
    phone::verify(
        &app.db.conn,
        &Provider,
        linked,
        Some((user.id, session)),
        "123456",
    )
    .await
    .unwrap();
    let signin = phone::start(&app.db.conn, &Provider, user.id, None, "+393331234567")
        .await
        .unwrap();
    phone::remove(&app.db.conn, user.id).await.unwrap();
    assert!(phone::find(&app.db.conn, "+393331234567", false)
        .await
        .unwrap()
        .is_none());
    assert!(
        phone::verify(&app.db.conn, &Provider, signin, None, "123456")
            .await
            .is_err()
    );
    assert!(phone::start(
        &app.db.conn,
        &Provider,
        user.id,
        Some(session),
        "+393331234567"
    )
    .await
    .is_err());
}

#[tokio::test]
async fn removing_phone_retires_unconsumed_binding_proofs_and_credentials() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let user = app.user("phone-remove-pending").await;
    let session = Uuid::new_v4();
    let pending = phone::start(
        &app.db.conn,
        &Provider,
        user.id,
        Some(session),
        "+393331234569",
    )
    .await
    .unwrap();
    phone::remove(&app.db.conn, user.id).await.unwrap();
    assert!(phone::verify(
        &app.db.conn,
        &Provider,
        pending,
        Some((user.id, session)),
        "123456"
    )
    .await
    .is_err());
    let (status, _) = app.get("/api/v2/me/phone", Auth::Bearer(&user.token)).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
}
