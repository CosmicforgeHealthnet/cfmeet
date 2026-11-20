//! ADR 0062 / build contract §9 at the gateway, against a SYNTHETIC route table (the
//! generated manifest carries no `USER_TOKEN` route yet) and a scripted Platform Access:
//!
//! * `USER_TOKEN`: a valid `Bearer cfut_…` is forwarded with `Authorization` intact (no
//!   gateway service token); a bad signature, an expired token, an unknown `kid` or a
//!   malformed token is `401` BEFORE forwarding; no usable keys is `503` (fail closed);
//!   rotated keys are picked up;
//! * partner-website CORS: preflight only for registered origins of ACTIVE environments
//!   on routes accepting a device mode (never PLATFORM_CREDENTIAL-only routes); actual
//!   responses carry `Access-Control-Allow-Origin` only for the token's environment;
//! * the static origin list and flags-off behaviour are unchanged;
//! * WebSocket upgrades forward `Origin` (flag on only).

mod common;

use std::{sync::Arc, time::Duration};

use api_gateway::{
    origins::OriginIndex,
    routing::{AuthMode, Protocol, RouteDefinition},
    user_tokens::{unix_now, UserTokenError, UserTokenVerifier},
};
use axum::http::{Method, StatusCode};
use base64::Engine;
use cf_service_auth::services;
use common::*;
use serde_json::json;
use uuid::Uuid;

const KID_A: &str = "00000000000000aa";
const KID_B: &str = "00000000000000bb";
const ORIGIN_A: &str = "https://shop-a.example";
const ORIGIN_B: &str = "https://shop-b.example";
const CRED: &str = "cfmk_sbx_aaaaaaaaaaaaaaaaaaaaaaaa_bbbb";

/// The test table plus the device-exchange shapes of contract §4.
fn routes() -> Vec<RouteDefinition> {
    use AuthMode::*;
    let mut r = test_routes();
    r.push(route(
        "messaging.userSessions",
        "messaging",
        Method::POST,
        "/v1/test/user-sessions",
        services::MESSAGING,
        &[UserToken],
        None,
    ));
    r.push(route(
        "meetings.join",
        "meetings",
        Method::POST,
        "/api/v2/test-conferences/{conference_id}/join",
        services::MEETING_SERVICE,
        &[MeetingSession, ParticipantToken, UserToken],
        None,
    ));
    r
}

struct Partner {
    signer: UserTokenSigner,
    keys: Arc<ScriptedKeys>,
    feed: Arc<ScriptedFeed>,
    env_a: Uuid,
    env_b: Uuid,
}

impl Partner {
    fn new() -> Self {
        let signer = UserTokenSigner::new(KID_A);
        let keys = ScriptedKeys::with(vec![signer.published("ACTIVE")]);
        let feed = Arc::new(ScriptedFeed::default());
        let (env_a, env_b) = (Uuid::new_v4(), Uuid::new_v4());
        feed.push(env_a, &[ORIGIN_A, "http://localhost:3000"], "ACTIVE");
        feed.push(env_b, &[ORIGIN_B], "ACTIVE");
        feed.push(Uuid::new_v4(), &["https://suspended.example"], "SUSPENDED");
        Self {
            signer,
            keys,
            feed,
            env_a,
            env_b,
        }
    }

    async fn gateway(&self, user_tokens: bool, origins: bool, cors: Vec<String>) -> Gateway {
        let verifier = user_tokens.then(|| Arc::new(UserTokenVerifier::new(self.keys.clone())));
        let index = if origins {
            let i = Arc::new(OriginIndex::new(self.feed.clone()));
            i.sync().await.unwrap();
            Some(i)
        } else {
            None
        };
        Gateway::with(Options {
            routes: Some(routes()),
            cors,
            user_tokens: verifier,
            origins: index,
            ..Options::default()
        })
        .await
    }

    fn token(&self, env: Uuid) -> String {
        self.signer.token(env, unix_now(), 3600)
    }
}

async fn post(g: &Gateway, path: &str, headers: &[(&str, &str)]) -> Resp {
    g.call(
        Method::POST,
        path,
        headers,
        Some(r#"{"installationId":"inst-12345678"}"#),
    )
    .await
}

fn acao(r: &Resp) -> Option<&str> {
    r.headers
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
}

async fn preflight(g: &Gateway, path: &str, origin: &str, method: &str) -> Resp {
    g.call(
        Method::OPTIONS,
        path,
        &[
            ("origin", origin),
            ("access-control-request-method", method),
            (
                "access-control-request-headers",
                "authorization, content-type",
            ),
        ],
        None,
    )
    .await
}

/// A compact JWS with `claims` as payload (the signature is never checked here).
fn unsigned_jws(claims: serde_json::Value) -> String {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "{}.{}.c2ln",
        b64.encode(
            json!({"alg": "EdDSA", "typ": "cfmeet-mcat+jwt", "kid": "0123456789abcdef"})
                .to_string()
        ),
        b64.encode(claims.to_string())
    )
}

#[tokio::test]
async fn a_valid_user_token_is_forwarded_with_authorization_intact() {
    let p = Partner::new();
    let g = p.gateway(true, false, vec![]).await;
    let token = p.token(p.env_a);
    let auth = format!("Bearer {token}");
    let r = post(&g, "/v1/test/user-sessions", &[("authorization", &auth)]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let got = g.messaging().last();
    assert_eq!(got.path, "/v1/test/user-sessions");
    assert_eq!(got.headers.get("authorization").unwrap(), auth.as_str());
    assert!(
        got.issuer.is_none() && !got.headers.contains_key("x-cf-service-authorization"),
        "a user token is transported, never wrapped in a gateway service token"
    );
    assert_eq!(got.body["installationId"], "inst-12345678");
    // A route accepting several modes: the user token wins for `cfut_`, others unchanged.
    let cid = Uuid::new_v4();
    let path = format!("/api/v2/test-conferences/{cid}/join");
    let r = post(&g, &path, &[("authorization", &auth)]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        g.meeting().last().headers.get("authorization").unwrap(),
        auth.as_str()
    );
    let r = post(&g, &path, &[("authorization", "Bearer meeting-session-1")]).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "meeting sessions still transported"
    );
    assert_eq!(
        g.meeting().last().headers.get("authorization").unwrap(),
        "Bearer meeting-session-1"
    );
    // A platform credential is never accepted on a user-token route.
    let r = post(
        &g,
        "/v1/test/user-sessions",
        &[("authorization", &format!("Bearer {CRED}"))],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // A user token is never accepted on a route without the mode.
    let r = g.get("/v1/test/items/abc", Some(&token)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(*g.authority.calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn forged_expired_and_unknown_user_tokens_never_reach_the_service() {
    let p = Partner::new();
    let g = p.gateway(true, false, vec![]).await;
    let now = unix_now();
    let forger = UserTokenSigner::new(KID_A);
    let other_kid = UserTokenSigner::new(KID_B);
    let good = p.token(p.env_a);
    let (head, sig) = good.rsplit_once('.').unwrap();
    let mut flipped = sig.as_bytes().to_vec();
    flipped[3] = if flipped[3] == b'A' { b'B' } else { b'A' };
    let header = json!({"alg": "EdDSA", "typ": "cfmeet-user+jwt", "kid": KID_A});
    let mut wrong_aud = user_token_claims(p.env_a, now, now + 600);
    wrong_aud["aud"] = json!("cfmeet.messaging.realtime");
    let cases = [
        (
            "bad signature (other key, same kid)",
            forger.token(p.env_a, now, 3600),
        ),
        (
            "bad signature (altered)",
            format!("{head}.{}", String::from_utf8(flipped).unwrap()),
        ),
        ("expired", p.signer.token(p.env_a, now - 7200, 3600)),
        ("not yet valid", p.signer.token(p.env_a, now + 900, 3600)),
        ("unknown kid", other_kid.token(p.env_a, now, 3600)),
        (
            "wrong audience",
            p.signer.sign_raw("sbx", &header, &wrong_aud),
        ),
        (
            "tag / kind mismatch",
            p.signer
                .sign_raw("live", &header, &user_token_claims(p.env_a, now, now + 600)),
        ),
        ("malformed", "cfut_sbx_not-a-jws".to_string()),
    ];
    for (what, token) in cases {
        let r = post(
            &g,
            "/v1/test/user-sessions",
            &[("authorization", &format!("Bearer {token}"))],
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{what}");
        assert_eq!(r.code(), "AUTHENTICATION_FAILED", "{what}");
        assert!(
            !r.bytes.windows(5).any(|w| w == b"cfut_"),
            "{what}: no echo"
        );
    }
    assert_eq!(g.messaging().count(), 0, "nothing was forwarded");
}

#[tokio::test]
async fn without_usable_keys_user_tokens_fail_closed() {
    let p = Partner::new();
    *p.keys.down.lock().unwrap() = true;
    let g = p.gateway(true, false, vec![]).await;
    let auth = format!("Bearer {}", p.token(p.env_a));
    let r = post(&g, "/v1/test/user-sessions", &[("authorization", &auth)]).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(g.messaging().count(), 0);
    // An empty key set: no key verifies.
    *p.keys.down.lock().unwrap() = false;
    p.keys.keys.lock().unwrap().clear();
    let g = p.gateway(true, false, vec![]).await;
    let r = post(&g, "/v1/test/user-sessions", &[("authorization", &auth)]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(g.messaging().count(), 0);
    // /ready reports the key state while the flag is on, without failing readiness.
    let ready = g.get("/ready", None).await;
    assert_eq!(ready.status, StatusCode::OK);
    assert_eq!(ready.json["userTokens"]["usableKeys"], 0);
}

#[tokio::test]
async fn signing_keys_refresh_and_rotate() {
    let p = Partner::new();
    let b = UserTokenSigner::new(KID_B);
    let now = unix_now();
    // Unknown kids refetch at most once per minimum interval (default 30 s).
    let v = UserTokenVerifier::new(p.keys.clone());
    assert!(v.verify(&p.token(p.env_a), now).await.is_ok());
    let fetches = *p.keys.fetches.lock().unwrap();
    for _ in 0..10 {
        assert_eq!(
            v.verify(&b.token(p.env_a, now, 600), now).await,
            Err(UserTokenError::UnknownKey)
        );
    }
    assert!(*p.keys.fetches.lock().unwrap() <= fetches + 1);
    // Rotation: B published, A retired; a refreshing verifier follows the key set.
    let v = Arc::new(
        UserTokenVerifier::new(p.keys.clone()).with_timings(Duration::ZERO, Duration::ZERO),
    );
    assert!(v.verify(&p.token(p.env_a), now).await.is_ok());
    *p.keys.keys.lock().unwrap() = vec![p.signer.published("RETIRED"), b.published("ACTIVE")];
    assert!(v.verify(&b.token(p.env_a, now, 600), now).await.is_ok());
    assert_eq!(
        v.verify(&p.token(p.env_a), now).await,
        Err(UserTokenError::UnknownKey),
        "a retired key no longer verifies"
    );
    assert_eq!(v.usable_keys(), 1);
    // The background refresher keeps fetching.
    let before = *p.keys.fetches.lock().unwrap();
    let task = tokio::spawn(v.clone().run_refresher());
    tokio::time::sleep(Duration::from_millis(350)).await;
    task.abort();
    assert!(*p.keys.fetches.lock().unwrap() >= before + 2);
}

#[tokio::test]
async fn with_the_flag_off_user_tokens_are_handled_as_before() {
    let p = Partner::new();
    let g = p.gateway(false, false, vec![]).await;
    let auth = format!("Bearer {}", p.token(p.env_a));
    // A USER_TOKEN-only route: no mode applies -> 401, nothing forwarded.
    let r = post(&g, "/v1/test/user-sessions", &[("authorization", &auth)]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(g.messaging().count(), 0);
    // A mixed route: the bearer is transported as a meeting session, as today.
    let path = format!("/api/v2/test-conferences/{}/join", Uuid::new_v4());
    let r = post(&g, &path, &[("authorization", &auth)]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(*p.keys.fetches.lock().unwrap(), 0, "no key fetch while off");
    let ready = g.get("/ready", None).await;
    assert!(ready.json.get("userTokens").is_none());
    assert!(ready.json.get("environmentOrigins").is_none());
}

#[tokio::test]
async fn partner_preflight_only_for_registered_origins_on_device_routes() {
    let p = Partner::new();
    let g = p.gateway(true, true, vec![]).await;
    // Device routes (user token; client token; signaling credential; participant token).
    for (path, method) in [
        ("/v1/test/user-sessions", "POST"),
        ("/v1/test/mixed", "GET"),
        ("/v1/test/devices", "GET"),
        ("/api/v2/test-spaces/abc", "GET"),
    ] {
        let r = preflight(&g, path, ORIGIN_A, method).await;
        assert_eq!(r.status, StatusCode::NO_CONTENT, "{path}");
        assert_eq!(acao(&r), Some(ORIGIN_A), "{path}");
        let methods = r.headers["access-control-allow-methods"].to_str().unwrap();
        assert!(methods.contains(method), "{path}: {methods}");
        assert!(r.headers["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains("authorization"));
    }
    // Loopback origins registered by a SANDBOX environment.
    let r = preflight(
        &g,
        "/v1/test/user-sessions",
        "http://localhost:3000",
        "POST",
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    // PLATFORM_CREDENTIAL-only routes: never for a partner origin.
    for (path, method) in [
        ("/v1/test/items", "POST"),
        ("/v1/test/items/abc", "GET"),
        ("/v1/developer-platform/context", "GET"),
        ("/api/v2/test-instant-meetings", "POST"),
        ("/openapi.json", "GET"),
    ] {
        let r = preflight(&g, path, ORIGIN_A, method).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(r.code(), "ORIGIN_NOT_ALLOWED", "{path}");
        assert!(acao(&r).is_none(), "{path}");
    }
    // Unknown origins and origins of a non-ACTIVE environment.
    for origin in [
        "https://evil.example",
        "https://suspended.example",
        "https://shop-a.example.evil.example",
        "http://shop-a.example",
    ] {
        let r = preflight(&g, "/v1/test/user-sessions", origin, "POST").await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{origin}");
    }
    // The feed is followed: B suspended, A gains an origin.
    p.feed.push(p.env_b, &[ORIGIN_B], "SUSPENDED");
    p.feed
        .push(p.env_a, &[ORIGIN_A, "https://new-a.example"], "ACTIVE");
    g.state.origins.as_ref().unwrap().sync().await.unwrap();
    let r = preflight(&g, "/v1/test/user-sessions", ORIGIN_B, "POST").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = preflight(
        &g,
        "/v1/test/user-sessions",
        "https://new-a.example",
        "POST",
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        g.get("/ready", None).await.json["environmentOrigins"],
        "synced"
    );
}

#[tokio::test]
async fn partner_responses_are_readable_only_by_the_token_environment_origin() {
    let p = Partner::new();
    let g = p.gateway(true, true, vec![]).await;
    let token_a = format!("Bearer {}", p.token(p.env_a));
    let token_b = format!("Bearer {}", p.token(p.env_b));
    let r = post(
        &g,
        "/v1/test/user-sessions",
        &[("authorization", &token_a), ("origin", ORIGIN_A)],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(acao(&r), Some(ORIGIN_A));
    assert!(r.headers["access-control-expose-headers"]
        .to_str()
        .unwrap()
        .contains("x-request-id"));
    assert!(r
        .headers
        .get_all("vary")
        .iter()
        .any(|v| v.to_str().unwrap().contains("origin")));
    // A's origin with B's token: forwarded (the service decides), but not readable.
    let r = post(
        &g,
        "/v1/test/user-sessions",
        &[("authorization", &token_b), ("origin", ORIGIN_A)],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(acao(&r).is_none());
    // An invalid user token: 401 without ACAO (no verified environment).
    let forged = UserTokenSigner::new(KID_A).token(p.env_a, unix_now(), 600);
    let r = post(
        &g,
        "/v1/test/user-sessions",
        &[
            ("authorization", &format!("Bearer {forged}")),
            ("origin", ORIGIN_A),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(acao(&r).is_none());
    // Client token (`eid`) and signaling credential (`env`) claims.
    let client = format!("CF-Client-Token {}", unsigned_jws(json!({"eid": p.env_a})));
    let r = g
        .call(
            Method::GET,
            "/v1/test/mixed",
            &[("authorization", &client), ("origin", ORIGIN_A)],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(acao(&r), Some(ORIGIN_A));
    let r = g
        .call(
            Method::GET,
            "/v1/test/mixed",
            &[("authorization", &client), ("origin", ORIGIN_B)],
            None,
        )
        .await;
    assert!(acao(&r).is_none(), "client token of A, origin of B");
    let signaling = format!(
        "Bearer cfsig_{}",
        unsigned_jws(json!({"env": p.env_b.to_string()}))
    );
    let r = g
        .call(
            Method::GET,
            "/v1/test/devices",
            &[("authorization", &signaling), ("origin", ORIGIN_B)],
            None,
        )
        .await;
    assert_eq!(acao(&r), Some(ORIGIN_B));
    // A platform credential on a mixed route: never a partner ACAO.
    g.authority.add(CRED, customer(&[]));
    let r = g
        .call(
            Method::GET,
            "/v1/test/mixed",
            &[
                ("authorization", &format!("Bearer {CRED}")),
                ("origin", ORIGIN_A),
            ],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(acao(&r).is_none());
    // PLATFORM_CREDENTIAL-only route.
    let r = g
        .call(
            Method::GET,
            "/v1/test/items/history",
            &[
                ("authorization", &format!("Bearer {CRED}")),
                ("origin", ORIGIN_A),
            ],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(acao(&r).is_none());
}

#[tokio::test]
async fn the_static_origin_list_is_unchanged() {
    let p = Partner::new();
    let console = "https://console.cfmeet.com";
    for (user_tokens, origins) in [(false, false), (true, true)] {
        let g = p.gateway(user_tokens, origins, vec![console.into()]).await;
        // Static origins: every routable path, preflight and actual, as before.
        let r = preflight(&g, "/v1/test/items", console, "POST").await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
        assert_eq!(acao(&r), Some(console));
        let r = g
            .call(Method::GET, "/ready", &[("origin", console)], None)
            .await;
        assert_eq!(acao(&r), Some(console));
        let r = preflight(&g, "/v1/test/nothing-here", console, "GET").await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        // A partner origin: refused while the flag is off, device routes only when on.
        let r = preflight(&g, "/v1/test/user-sessions", ORIGIN_A, "POST").await;
        let want = if origins {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::FORBIDDEN
        };
        assert_eq!(r.status, want, "origins flag {origins}");
        let r = preflight(&g, "/v1/test/items", ORIGIN_A, "POST").await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
        let r = g
            .call(Method::GET, "/ready", &[("origin", ORIGIN_A)], None)
            .await;
        assert!(acao(&r).is_none());
    }
}

/// Realtime routes: the browser's `Origin` reaches the owning service (flag on only).
#[tokio::test]
async fn websocket_upgrades_forward_the_origin() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let p = Partner::new();
    for origins in [true, false] {
        let g = p.gateway(false, origins, vec![]).await;
        let addr = g.serve().await;
        let mut req = format!("ws://{addr}/v1/test/realtime")
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(
            "sec-websocket-protocol",
            "cfmeet.messaging.v1, cfmeet.ticket.good".parse().unwrap(),
        );
        req.headers_mut()
            .insert("origin", ORIGIN_A.parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let _ = ws.close(None).await;
        let got = g.messaging().last();
        assert_eq!(got.path, "/v1/test/realtime");
        if origins {
            assert_eq!(got.headers.get("origin").unwrap(), ORIGIN_A);
        } else {
            assert!(got.headers.get("origin").is_none());
        }
    }
}

#[test]
fn device_modes_and_the_manifest_name() {
    assert_eq!(AuthMode::parse("USER_TOKEN"), Some(AuthMode::UserToken));
    assert_eq!(AuthMode::UserToken.as_str(), "USER_TOKEN");
    assert!(AuthMode::UserToken.is_passthrough());
    for m in [
        AuthMode::UserToken,
        AuthMode::ClientToken,
        AuthMode::RealtimeTicket,
        AuthMode::SignalingCredential,
        AuthMode::ParticipantToken,
    ] {
        assert!(m.is_device(), "{m:?}");
    }
    for m in [
        AuthMode::PlatformCredential,
        AuthMode::DeveloperSession,
        AuthMode::MeetingSession,
        AuthMode::Anonymous,
    ] {
        assert!(!m.is_device(), "{m:?}");
    }
    let mut ws = route(
        "messaging.realtime2",
        "messaging",
        Method::GET,
        "/v1/test/realtime2",
        services::MESSAGING,
        &[AuthMode::UserToken, AuthMode::RealtimeTicket],
        None,
    );
    ws.protocol = Protocol::WebSocket;
    assert!(ws.accepts_device());
}
