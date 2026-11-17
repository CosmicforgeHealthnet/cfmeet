//! Gap audit A7/A8: one-time codes cannot be guessed past their budget, and the
//! verify-email / reset-password answers never reveal whether an address has an account.
mod common;

use axum::http::StatusCode;
use backend::services::otp::OtpType;
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

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

fn email() -> String {
    format!(
        "gapacc-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..10]
    )
}

/// A8: after the fifth wrong guess the code is burned; even the right code then fails.
#[tokio::test]
async fn gap_a_code_is_burned_after_its_guess_budget() {
    let app = app_with_redis!();
    let e = email();
    let otp = app.state.otp_service.clone();
    otp.store_code(&e, "123456", OtpType::PasswordReset)
        .await
        .unwrap();
    for _ in 0..5 {
        assert!(!otp
            .verify_code(&e, "000000", OtpType::PasswordReset)
            .await
            .unwrap());
    }
    assert!(!otp.has_code(&e, OtpType::PasswordReset).await.unwrap());
    assert!(otp
        .verify_code(&e, "123456", OtpType::PasswordReset)
        .await
        .is_err());
}

/// A7: an unknown address and an existing one get the same answers (400 then 429).
#[tokio::test]
async fn gap_reset_and_verify_answer_alike_for_unknown_and_existing_addresses() {
    let app = app_with_redis!();
    let known = email();
    let id = Uuid::new_v4();
    let hash = backend::services::auth::hash_password("Str0ng!Passw0rd").unwrap();
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO users (id, username, email, password_hash, auth_type, role, status, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'local', 'user', 'active'::user_status, now(), now())",
            [
                id.into(),
                format!("u_{}", &id.simple().to_string()[..10]).into(),
                known.clone().into(),
                hash.into(),
            ],
        ))
        .await
        .unwrap();
    let unknown = email();
    for path in ["/api/v1/auth/reset-password", "/api/v1/auth/verify-email"] {
        let mut answers = Vec::new();
        for address in [&known, &unknown] {
            let mut seen = Vec::new();
            for _ in 0..6 {
                let (s, v) = app
                    .post(
                        path,
                        Auth::None,
                        json!({ "email": address, "token": "000000", "new_password": "An0ther!Passw0rd", "confirm_password": "An0ther!Passw0rd" }),
                    )
                    .await;
                seen.push((s, v["error"]["message"].clone()));
            }
            answers.push(seen);
        }
        assert_eq!(answers[0], answers[1], "{path}");
        assert_eq!(answers[0][0].0, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(answers[0][5].0, StatusCode::TOO_MANY_REQUESTS, "{path}");
    }
}

/// A9: a signed-in session cannot guess the current password without bound.
#[tokio::test]
async fn gap_current_password_guesses_are_limited() {
    let app = app_with_redis!();
    let e = email();
    let id = Uuid::new_v4();
    let hash = backend::services::auth::hash_password("Str0ng!Passw0rd").unwrap();
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO users (id, username, email, password_hash, auth_type, role, status, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'local', 'user', 'active'::user_status, now(), now())",
            [
                id.into(),
                format!("u_{}", &id.simple().to_string()[..10]).into(),
                e.clone().into(),
                hash.into(),
            ],
        ))
        .await
        .unwrap();
    let (s, v) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": e, "password": "Str0ng!Passw0rd" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let token = v["access_token"].as_str().unwrap().to_string();
    let body = json!({
        "current_password": "Wr0ng!Passw0rd",
        "new_password": "N3w!Passw0rd-long",
        "confirm_password": "N3w!Passw0rd-long",
    });
    for _ in 0..10 {
        let (s, _) = app
            .post(
                "/api/v1/auth/change-password",
                Auth::Bearer(&token),
                body.clone(),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }
    let (s, _) = app
        .post("/api/v1/auth/change-password", Auth::Bearer(&token), body)
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}
