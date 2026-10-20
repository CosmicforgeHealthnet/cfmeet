//! Part V H — Meeting's Unified Messaging endpoints with a scripted peer (runs in every
//! CI suite; the real-process journey is `unified_messaging_e2e.rs`).
//!
//! Proves the Meeting-side rules that do not depend on the peers' behaviour: who may use
//! the identity bridge, what is signed into each call (acting principal, kind), that no
//! service credential is ever returned, fail-closed login verification, outage mapping,
//! and the platform-admin migration gates.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use backend::{
    config::FeatureFlags,
    services::unified_messaging::{Acting, GatewayError, MessagingGateway, Peer, PrincipalKind},
};
use common::{collab, Auth, RedisMode, RtcMode, TestApp, TestOptions};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Clone)]
struct Call {
    peer: Peer,
    acting: Option<Acting>,
    method: Method,
    path: String,
    body: Option<Value>,
}

/// A scripted Platform Access + Messaging (no network).
#[derive(Default)]
struct Scripted {
    env: Uuid,
    down: Mutex<bool>,
    calls: Mutex<Vec<Call>>,
    principals: Mutex<std::collections::HashMap<String, Uuid>>,
}

impl Scripted {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            env: Uuid::new_v4(),
            ..Default::default()
        })
    }
    fn set_down(&self, down: bool) {
        *self.down.lock().unwrap() = down;
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl MessagingGateway for Scripted {
    fn environment_id(&self) -> Uuid {
        self.env
    }

    async fn call_raw(
        &self,
        peer: Peer,
        acting: Option<Acting>,
        method: Method,
        path: &str,
        _query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value), GatewayError> {
        if *self.down.lock().unwrap() {
            return Err(GatewayError::Unavailable("connect"));
        }
        self.calls.lock().unwrap().push(Call {
            peer,
            acting,
            method: method.clone(),
            path: path.to_string(),
            body: body.cloned(),
        });
        Ok(match (peer, path) {
            (Peer::PlatformAccess, "/internal/v1/principals/ensure") => {
                let reference = body.unwrap()["externalReference"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let mut p = self.principals.lock().unwrap();
                let created = !p.contains_key(&reference);
                let id = *p.entry(reference).or_insert_with(Uuid::new_v4);
                (
                    200,
                    json!({ "principal": { "id": id, "status": "ACTIVE" }, "created": created }),
                )
            }
            (Peer::Messaging, p) if p.starts_with("/internal/v1/delegated/v1/messaging/") => {
                // Echo what a device-direct Messaging answer looks like (no secrets here).
                (
                    201,
                    json!({ "relayed": p, "deviceId": body.and_then(|b| b.get("deviceId").cloned()) }),
                )
            }
            (Peer::Messaging, "/internal/v1/meeting-conversations") => (
                201,
                json!({ "created": true, "conversation": { "conversationId": Uuid::new_v4() } }),
            ),
            (Peer::Messaging, p) if p.ends_with("/messages") => (200, json!({ "messages": [] })),
            _ => (200, json!({ "applied": true })),
        })
    }
}

async fn app(gateway: Option<Arc<Scripted>>, redis: RedisMode) -> Option<TestApp> {
    TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: FeatureFlags {
            rtc_provider_control_v2: true,
            conference_realtime_enabled: true,
            collaboration_v2_enabled: true,
            ..FeatureFlags::all_v2_enabled()
        },
        redis,
        unified_messaging: gateway.map(|g| g as Arc<dyn MessagingGateway>),
        ..TestOptions::default()
    })
    .await
}

const BRIDGE: &str = "/api/v2/messaging";

fn bridge_calls(device: Uuid) -> Vec<(String, Value, StatusCode)> {
    vec![
        (
            format!("{BRIDGE}/devices/registration-challenges"),
            json!({}),
            StatusCode::CREATED,
        ),
        (
            format!("{BRIDGE}/devices"),
            json!({ "challengeId": Uuid::new_v4(), "keyPackage": "AA" }),
            StatusCode::CREATED,
        ),
        (
            format!("{BRIDGE}/devices/{device}/key-packages"),
            json!({ "keyPackages": [] }),
            StatusCode::CREATED,
        ),
        (
            format!("{BRIDGE}/devices/{device}/revoke"),
            json!({}),
            StatusCode::CREATED,
        ),
        (
            format!("{BRIDGE}/client-tokens"),
            json!({ "deviceId": device }),
            StatusCode::CREATED,
        ),
        (
            format!("{BRIDGE}/client-sessions/revoke-all"),
            json!({}),
            StatusCode::CREATED,
        ),
    ]
}

#[tokio::test]
async fn the_identity_bridge_acts_only_for_verified_cf_meet_accounts() {
    let gw = Scripted::new();
    let Some(app) = app(Some(gw.clone()), RedisMode::FromEnv).await else {
        return;
    };
    let user = app.user("bridge-user").await;
    let device = Uuid::new_v4();
    for (path, body, expected) in bridge_calls(device) {
        let (s, v) = app.post(&path, Auth::Bearer(&user.token), body).await;
        assert_eq!(s, expected, "{path}: {v}");
        // Never a service credential or signing material in what the client receives.
        let text = v.to_string().to_lowercase();
        assert!(
            !text.contains("x-cf-service") && !text.contains("private"),
            "{text}"
        );
    }
    let calls = gw.calls();
    // One idempotent principal ensure with an opaque reference (no email / number).
    let ensures: Vec<_> = calls
        .iter()
        .filter(|c| c.peer == Peer::PlatformAccess)
        .collect();
    assert_eq!(
        ensures.len(),
        1,
        "the account → principal mapping is cached"
    );
    assert_eq!(
        ensures[0].body.as_ref().unwrap(),
        &json!({ "kind": "NATIVE_CF_USER", "externalReference": format!("cfmeet-user:{}", user.id) })
    );
    let principal: Uuid = *gw.principals.lock().unwrap().values().next().unwrap();
    // Every relayed call acts for THAT principal, signed (never a request field).
    let relayed: Vec<_> = calls.iter().filter(|c| c.peer == Peer::Messaging).collect();
    assert_eq!(relayed.len(), 6);
    for c in &relayed {
        assert_eq!(c.method, Method::POST);
        assert_eq!(
            c.acting,
            Some(Acting {
                principal_id: principal,
                kind: PrincipalKind::NativeCfUser
            }),
            "{}",
            c.path
        );
        assert!(
            c.path.starts_with("/internal/v1/delegated/v1/messaging/"),
            "{}",
            c.path
        );
    }
    assert!(relayed
        .iter()
        .any(|c| c.path.ends_with("/revoke") && c.body.is_none()));
    // A guest (no account) and an anonymous caller are refused before any peer call.
    let (_, cid, guests) = collab::meeting(&app, collab::public(), 1).await;
    let _ = cid;
    let before = gw.calls().len();
    let (s, v) = app
        .post(
            &format!("{BRIDGE}/client-tokens"),
            guests[0].auth(),
            json!({ "deviceId": device }),
        )
        .await;
    assert!(
        s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN,
        "{s} {v}"
    );
    let (s, v) = app
        .post(
            &format!("{BRIDGE}/client-tokens"),
            Auth::None,
            json!({ "deviceId": device }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(
        gw.calls().len(),
        before,
        "no peer call for unauthenticated callers"
    );
    // Messaging unreachable: a typed, retryable chat error.
    gw.set_down(true);
    let (s, v) = app
        .post(
            &format!("{BRIDGE}/client-tokens"),
            Auth::Bearer(&user.token),
            json!({ "deviceId": device }),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("CHAT_UNAVAILABLE")),
        "{v}"
    );
    gw.set_down(false);
    // A body that is not JSON is rejected with the standard envelope, never relayed.
    let (s, _) = app
        .post_raw(
            &format!("{BRIDGE}/client-tokens"),
            Auth::Bearer(&user.token),
            "not json",
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_bridge_fails_closed_when_the_login_cannot_be_verified() {
    let gw = Scripted::new();
    // The revocation store is unreachable: a login cannot be proven un-revoked.
    let Some(app) = app(Some(gw.clone()), RedisMode::Unreachable).await else {
        return;
    };
    let user = app.user("unverifiable").await;
    let (s, v) = app
        .post(
            &format!("{BRIDGE}/client-tokens"),
            Auth::Bearer(&user.token),
            json!({ "deviceId": Uuid::new_v4() }),
        )
        .await;
    assert!(
        s == StatusCode::SERVICE_UNAVAILABLE || s == StatusCode::UNAUTHORIZED,
        "{s} {v}"
    );
    assert!(
        gw.calls().is_empty(),
        "nothing is asked of Messaging for an unverified login"
    );
}

#[tokio::test]
async fn messaging_endpoints_answer_feature_disabled_without_the_integration() {
    let Some(app) = app(None, RedisMode::FromEnv).await else {
        return;
    };
    let user = app.user("plain").await;
    let ops = app.admin("ops-plain").await;
    for (path, body, _) in bridge_calls(Uuid::new_v4()) {
        let (s, v) = app.post(&path, Auth::Bearer(&user.token), body).await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (StatusCode::NOT_FOUND, Some("FEATURE_DISABLED")),
            "{path}: {v}"
        );
    }
    let (s, _) = app
        .get("/api/v2/admin/chat-migration", Auth::Bearer(&ops.token))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Chat keeps working on the meeting store exactly as before.
    let (_, cid, guests) = collab::meeting(&app, collab::public(), 1).await;
    let (s, v) = collab::send(
        &app,
        cid,
        guests[0].auth(),
        json!({ "body": "legacy only" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

#[tokio::test]
async fn chat_migration_controls_are_platform_admin_only_and_gated() {
    let gw = Scripted::new();
    let Some(app) = app(Some(gw.clone()), RedisMode::FromEnv).await else {
        return;
    };
    let ops = app.admin("ops").await;
    let user = app.user("not-ops").await;
    let status = "/api/v2/admin/chat-migration";
    let (s, v) = app.get(status, Auth::Bearer(&ops.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["state"].as_str(), v["version"].as_i64()),
        (Some("LEGACY_PRIMARY"), Some(1))
    );
    // Every chat scope migrates under the same state machine; none stays legacy-only.
    assert_eq!(v["scope"]["legacy"], json!([]));
    assert_eq!(
        v["scope"]["migrated"],
        json!([
            "MAIN_ROOM_CHAT",
            "ANNOUNCEMENTS",
            "BREAKOUT_CHAT",
            "PRIVATE_CHAT"
        ])
    );
    let (s, _) = app.get(status, Auth::Bearer(&user.token)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let t = "/api/v2/admin/chat-migration/transitions";
    let (s, _) = app
        .post(
            t,
            Auth::Bearer(&user.token),
            json!({ "targetState": "SHADOW_VALIDATION", "expectedVersion": 1, "reason": "try" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // LEGACY_PRIMARY cannot jump to MESSAGING_PRIMARY; reasons are required.
    let (s, v) = app
        .post(t, Auth::Bearer(&ops.token), json!({ "targetState": "MESSAGING_PRIMARY", "expectedVersion": 1, "reason": "skip", "approvedBy": "b" }))
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("CHAT_MIGRATION_REFUSED")),
        "{v}"
    );
    let (s, _) = app
        .post(
            t,
            Auth::Bearer(&ops.token),
            json!({ "targetState": "SHADOW_VALIDATION", "expectedVersion": 1, "reason": "" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = app
        .post(t, Auth::Bearer(&ops.token), json!({ "targetState": "SHADOW_VALIDATION", "expectedVersion": 1, "reason": "start shadow validation" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "SHADOW_VALIDATION");
    // Cutover without a parity run (and without an approver) is refused.
    let (s, v) = app
        .post(t, Auth::Bearer(&ops.token), json!({ "targetState": "MESSAGING_PRIMARY", "expectedVersion": 2, "reason": "cutover", "approvedBy": "second" }))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    // Parity runs are admin-only; with nothing to compare they pass.
    let p = "/api/v2/admin/chat-migration/parity-runs";
    let (s, _) = app.post(p, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app.post(p, Auth::Bearer(&ops.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "PASSED", "{v}");
    // Legacy main-room chat of a conference that has no conversation yet fails parity.
    let (_, cid, guests) = collab::meeting(&app, collab::public(), 1).await;
    let (s, _) = collab::send(&app, cid, guests[0].auth(), json!({ "body": "unmirrored" })).await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, v) = app.post(p, Auth::Bearer(&ops.token), json!({})).await;
    assert_eq!(
        (v["status"].as_str(), v["mismatches"].as_i64()),
        (Some("FAILED"), Some(1)),
        "{v}"
    );
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM conference_chat_mismatches WHERE kind = 'CONVERSATION_MISSING'"
        )
        .await,
        1
    );
}
