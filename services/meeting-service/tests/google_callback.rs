//! Gap audit A14: the Google browser sign-in succeeds end to end against a stand-in for
//! Google's token endpoint (`GOOGLE_TOKEN_URL`): start, callback, one-time exchange code,
//! a new verified account, the audit trail, and a replayed callback refused. Kept in its
//! own test binary because it sets process environment.
mod common;

use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{header, HeaderMap, Method, Request, StatusCode},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use common::*;
use serde_json::json;
use tower::ServiceExt;

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

async fn get(app: &TestApp, uri: &str, cookie: Option<&str>) -> (StatusCode, HeaderMap) {
    let mut req = Request::builder().method(Method::GET).uri(uri);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let resp = app
        .router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    (resp.status(), resp.headers().clone())
}

fn param(url: &str, name: &str) -> Option<String> {
    url.split(['?', '&'])
        .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
        .map(|v| urlencoding::decode(v).unwrap().into_owned())
}

#[tokio::test]
async fn google_browser_sign_in_succeeds_once() {
    // Stand-in for Google's token endpoint: answers with an ID token for the nonce the
    // sign-in started with.
    let nonce: Arc<Mutex<String>> = Arc::default();
    let seen = nonce.clone();
    let google = Router::new().route(
        "/token",
        post(move || {
            let nonce = seen.lock().unwrap().clone();
            async move {
                let now = chrono::Utc::now().timestamp();
                let claims = json!({
                    "iss": "https://accounts.google.com",
                    "aud": "",
                    "sub": "google-subject-a14",
                    "email": "new.googler@example.com",
                    "email_verified": true,
                    "nonce": nonce,
                    "iat": now,
                    "exp": now + 600,
                });
                let token = format!(
                    "{}.{}.sig",
                    URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
                    URL_SAFE_NO_PAD.encode(claims.to_string())
                );
                Json(json!({ "id_token": token, "access_token": "unused" }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, google).await.unwrap() });
    std::env::set_var("GOOGLE_TOKEN_URL", format!("http://{addr}/token"));

    let app = app_with_redis!();
    let (s, h) = get(&app, "/api/v1/auth/oauth/google", None).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    let location = h["location"].to_str().unwrap().to_string();
    let state = param(&location, "state").unwrap();
    *nonce.lock().unwrap() = param(&location, "nonce").unwrap();
    let cookie = h["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let callback = format!("/api/v1/auth/oauth/google/callback?code=c&state={state}");
    let (s, h) = get(&app, &callback, Some(&cookie)).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    let target = h["location"].to_str().unwrap().to_string();
    assert!(
        target.starts_with("http://localhost:3000/google/callback?"),
        "{target}"
    );
    assert!(
        !target.contains("access_token"),
        "tokens never travel in the URL"
    );
    let code = param(&target, "exchange_code").expect("exchange code");

    let (s, v) = app
        .post(
            "/api/v1/auth/oauth/google/exchange",
            Auth::None,
            json!({ "exchange_code": code }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let access = v["access_token"].as_str().unwrap().to_string();
    let (s, me) = app.get("/api/v1/users/me", Auth::Bearer(&access)).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    assert_eq!(me["email"], "new.googler@example.com");
    // The exchange code is single use.
    let (s, _) = app
        .post(
            "/api/v1/auth/oauth/google/exchange",
            Auth::None,
            json!({ "exchange_code": code }),
        )
        .await;
    assert!(s.is_client_error());
    // A new account, verified by Google, bound to the Google subject, with an audit row.
    let id = me["id"].as_str().unwrap();
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM users WHERE id = '{id}' AND email_verified_at IS NOT NULL"
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_state WHERE user_id = '{id}' \
              AND google_subject = 'google-subject-a14'"
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' \
              AND event_type = 'LOGIN' AND detail->>'method' = 'GOOGLE'"
        ))
        .await,
        1
    );
    // Replaying the callback is refused: the state was single use.
    let (s, h) = get(&app, &callback, Some(&cookie)).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert!(h["location"]
        .to_str()
        .unwrap()
        .ends_with("error=oauth_invalid_state"));
}
