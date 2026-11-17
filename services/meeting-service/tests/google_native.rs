//! Gap audit A4: native Google sign-in verifies the ID token's signature against the
//! (here: local) Google keys and every claim, then signs in like the web flow. Its own
//! binary: it points GOOGLE_JWKS_URL at a local key server.
mod common;

use axum::{http::StatusCode, routing::get, Json, Router};
use common::*;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;

/// A throwaway RSA key generated at test time with the `openssl` CLI (never committed):
/// (private key PEM, base64url modulus).
fn test_key() -> (String, String) {
    use base64::Engine as _;
    let dir = std::env::temp_dir().join(format!("gnative-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let pem_path = dir.join("key.pem");
    let ok = std::process::Command::new("openssl")
        .args(["genrsa", "-traditional", "-out"])
        .arg(&pem_path)
        .arg("2048")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        || std::process::Command::new("openssl")
            .args(["genrsa", "-out"])
            .arg(&pem_path)
            .arg("2048")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    assert!(ok, "openssl is needed to generate the test key");
    let modulus = std::process::Command::new("openssl")
        .args(["rsa", "-noout", "-modulus", "-in"])
        .arg(&pem_path)
        .output()
        .unwrap();
    let hex_n = String::from_utf8(modulus.stdout).unwrap();
    let hex_n = hex_n.trim().split('=').nth(1).unwrap().to_string();
    let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hex::decode(hex_n).unwrap());
    let pem = std::fs::read_to_string(&pem_path).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    (pem, n)
}

async fn key_server(n: &str) -> String {
    let jwks = json!({ "keys": [{ "kty": "RSA", "alg": "RS256", "use": "sig", "kid": "test-kid", "n": n, "e": "AQAB" }] });
    let app = Router::new().route(
        "/certs",
        get(move || {
            let j = jwks.clone();
            async move { Json(j) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/certs")
}

fn token(pem: &str, claims: serde_json::Value, kid: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.into());
    encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn native_google_id_tokens_are_verified_and_sign_in() {
    if !require_test_redis() {
        return;
    }
    let (pem, n) = test_key();
    std::env::set_var("GOOGLE_JWKS_URL", key_server(&n).await);
    std::env::set_var(
        "GOOGLE_NATIVE_CLIENT_IDS",
        "android-client.apps.googleusercontent.com",
    );
    let Some(app) = TestApp::new().await else {
        return;
    };
    let web = app.state.google_client_id.clone();
    let now = chrono::Utc::now().timestamp();
    let email = format!(
        "native-{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let claims = |aud: &str, azp: &str| {
        json!({
            "iss": "https://accounts.google.com", "aud": aud, "azp": azp,
            "sub": format!("g-{email}"), "email": email, "email_verified": true,
            "name": "Native User", "iat": now, "exp": now + 600,
        })
    };
    let path = "/api/v1/auth/oauth/google/native";
    // An Android token: audience = the web client, azp = the Android client.
    let (s, v) = app
        .post(path, Auth::None, json!({ "idToken": token(&pem, claims(&web, "android-client.apps.googleusercontent.com"), "test-kid") }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["access_token"].is_string(), "{v}");
    // A token for another app, an unknown key or a forged signature is refused.
    let (s, _) = app
        .post(
            path,
            Auth::None,
            json!({ "idToken": token(&pem, claims("someone-else", "someone-else"), "test-kid") }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = app
        .post(
            path,
            Auth::None,
            json!({ "idToken": token(&pem, claims(&web, &web), "unknown-kid") }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let good = token(&pem, claims(&web, &web), "test-kid");
    let mut parts: Vec<&str> = good.split('.').collect();
    let tampered = format!("{}x", parts[1]);
    parts[1] = &tampered;
    let (s, _) = app
        .post(path, Auth::None, json!({ "idToken": parts.join(".") }))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // An expired token is refused.
    let mut expired = claims(&web, &web);
    expired["exp"] = json!(now - 3600);
    let (s, _) = app
        .post(
            path,
            Auth::None,
            json!({ "idToken": token(&pem, expired, "test-kid") }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
