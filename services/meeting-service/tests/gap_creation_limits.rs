//! Gap audit M8: per-account creation budgets (own binary: it sets a process-wide limit).

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

#[tokio::test]
async fn an_account_cannot_create_meetings_beyond_its_hourly_budget() {
    if !require_test_redis() {
        return;
    }
    std::env::set_var("MEETING_SPACE_CREATES_PER_HOUR", "2");
    std::env::set_var("MEETING_INSTANT_CREATES_PER_HOUR", "1");
    let Some(app) = TestApp::new().await else {
        return;
    };
    let user = app.user("budget").await;
    for i in 0..2 {
        let (s, v) = app
            .post(
                "/api/v2/meeting-spaces",
                Auth::Bearer(&user.token),
                json!({ "title": format!("Space {i}") }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&user.token),
            json!({ "title": "One too many" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED"),
        "{v}"
    );
    let (s, _) = app
        .post(
            "/api/v2/instant-meetings",
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, v) = app
        .post(
            "/api/v2/instant-meetings",
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED"),
        "{v}"
    );
    // Another account has its own budget.
    let other = app.user("budget-other").await;
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&other.token),
            json!({ "title": "Mine" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}
