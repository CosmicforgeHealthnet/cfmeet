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
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM account_phone_bindings WHERE user_id=$1",
            vec![user.id.into()],
        ))
        .await
        .unwrap();
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
