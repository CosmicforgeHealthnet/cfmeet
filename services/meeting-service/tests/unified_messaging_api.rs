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
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
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

    fn scope_ids(&self) -> Option<(Uuid, Uuid, Uuid)> {
        Some((Uuid::nil(), Uuid::max(), self.env))
    }

    async fn call_raw(
        &self,
        peer: Peer,
        acting: Option<Acting>,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value), GatewayError> {
        self.call_raw_with_headers(peer, acting, method, path, query, &[], body)
            .await
    }

    async fn call_raw_with_headers(
        &self,
        peer: Peer,
        acting: Option<Acting>,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value), GatewayError> {
        if *self.down.lock().unwrap() {
            return Err(GatewayError::Unavailable("connect"));
        }
        let own = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        self.calls.lock().unwrap().push(Call {
            peer,
            acting,
            method: method.clone(),
            path: path.to_string(),
            query: own(query),
            headers: own(headers),
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
            (Peer::Messaging, p) if p.starts_with("/internal/v1/delegated/v1/conversations") => (
                if method == Method::POST { 201 } else { 200 },
                json!({ "relayed": p }),
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

/// The CF Meet client's Messaging API: only allowlisted end-user operations reach
/// Messaging, always acting for the signed-in account's own principal; the client cannot
/// choose another principal, administration routes or extra query parameters.
#[tokio::test]
async fn the_client_relay_forwards_only_allowlisted_operations_for_the_caller() {
    let gw = Scripted::new();
    let Some(app) = app(Some(gw.clone()), RedisMode::FromEnv).await else {
        return;
    };
    let user = app.user("relay-user").await;
    let auth = || Auth::Bearer(&user.token);
    let conv = Uuid::new_v4();
    let relay = |p: &str| format!("{BRIDGE}/relay/v1/{p}");

    // An allowlisted read with its query, and a send.
    let (s, v) = app
        .get(
            relay(&format!(
                "conversations/{conv}/messages?afterSequence=7&limit=50"
            )),
            auth(),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .post(
            &relay(&format!("conversations/{conv}/messages")),
            auth(),
            // A body that names another principal changes nothing: identity is signed.
            json!({ "clientMessageId": Uuid::new_v4(), "text": "hi", "actingPrincipalId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // Conversation creation forwards the client's Idempotency-Key (and nothing else).
    let (s, v) = app
        .call_with_headers(
            Method::POST,
            &relay("conversations"),
            &[
                ("authorization", &format!("Bearer {}", user.token)),
                ("idempotency-key", "web-create-1"),
                ("cf-acting-principal", &Uuid::new_v4().to_string()),
            ],
            json!({ "contextClass": "DIRECT", "counterpartPrincipalId": Uuid::new_v4() })
                .to_string(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let principal: Uuid = *gw.principals.lock().unwrap().values().next().unwrap();
    let relayed: Vec<Call> = gw
        .calls()
        .into_iter()
        .filter(|c| c.peer == Peer::Messaging)
        .collect();
    assert_eq!(relayed.len(), 3, "{relayed:?}");
    for c in &relayed {
        assert_eq!(
            c.acting,
            Some(Acting {
                principal_id: principal,
                kind: PrincipalKind::NativeCfUser
            })
        );
        assert!(
            c.path.starts_with("/internal/v1/delegated/v1/"),
            "{}",
            c.path
        );
    }
    assert_eq!(
        relayed[0].query,
        vec![
            ("afterSequence".to_string(), "7".to_string()),
            ("limit".to_string(), "50".to_string())
        ]
    );
    assert!(relayed[0].body.is_none(), "GET carries no body");
    assert!(relayed[1].headers.is_empty());
    assert_eq!(
        relayed[2].headers,
        vec![("idempotency-key".to_string(), "web-create-1".to_string())],
        "only the Idempotency-Key is forwarded"
    );

    // Refused before any Messaging call: administration, unknown shapes, extra query keys,
    // non-JSON bodies, anonymous callers.
    let before = gw.calls().len();
    for (method, path) in [
        (Method::PUT, relay("messaging/policy")),
        (Method::POST, relay("messaging/legal-holds")),
        (Method::POST, relay("messaging/exports")),
        (Method::GET, relay("messaging/audit")),
        (Method::POST, relay(&format!("conversations/{conv}/bans"))),
        (
            Method::GET,
            relay(&format!("conversations/{conv}/attachments/{conv}/content")),
        ),
        (Method::GET, relay("conversations/not-a-uuid")),
        (
            Method::GET,
            format!("{BRIDGE}/relay/internal/v1/meeting-conversations"),
        ),
    ] {
        let (s, v) = app
            .call(method.clone(), &path, auth(), Some(json!({})))
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {v}");
    }
    let (s, v) = app
        .get(
            relay(&format!(
                "conversations/{conv}/messages?principalId={}",
                Uuid::new_v4()
            )),
            auth(),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, _) = app
        .post_raw(
            &relay(&format!("conversations/{conv}/messages")),
            auth(),
            "not json",
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = app.get(relay("conversations"), Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(gw.calls().len(), before, "nothing refused reached a peer");

    // The caller's identity for client-side encryption (identifiers only).
    let (s, v) = app.get(format!("{BRIDGE}/me"), auth()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["principalId"], json!(principal));
    assert_eq!(v["environmentId"], json!(gw.env));
    assert!(v.get("tenantId").is_some() && v.get("applicationId").is_some());
}

/// The directory is bounded by the caller's verified organizations: colleagues are found
/// and resolved to principals; anyone else is neither listed, resolvable nor named.
#[tokio::test]
async fn the_directory_lists_and_resolves_only_organization_colleagues() {
    use sea_orm::ConnectionTrait;
    let gw = Scripted::new();
    let Some(app) = app(Some(gw.clone()), RedisMode::FromEnv).await else {
        return;
    };
    let alice = app.user("dir-alice").await;
    let bob = app.user("dir-bob").await;
    let mallory = app.user("dir-mallory").await;
    let org = Uuid::new_v4();
    let other_org = Uuid::new_v4();
    let sql = |q: String| {
        let conn = app.db.conn.clone();
        async move { conn.execute_unprepared(&q).await.unwrap() }
    };
    for (id, slug) in [(org, "dir-org"), (other_org, "dir-other")] {
        sql(format!(
            "INSERT INTO organizations (id, name, slug, status, created_at, updated_at, profile_policy) \
             VALUES ('{id}', '{slug}', '{slug}-{}', 'ACTIVE', now(), now(), '{{}}')",
            &Uuid::new_v4().simple().to_string()[..6]
        ))
        .await;
    }
    for (o, u) in [(org, alice.id), (org, bob.id), (other_org, mallory.id)] {
        sql(format!(
            "INSERT INTO organization_memberships (id, organization_id, user_id, role, status, created_at, updated_at) \
             VALUES ('{}', '{o}', '{u}', 'MEMBER', 'ACTIVE', now(), now())",
            Uuid::new_v4()
        ))
        .await;
    }
    let auth = Auth::Bearer(&alice.token);
    let (s, v) = app
        .get(format!("{BRIDGE}/directory"), Auth::Bearer(&alice.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let people = v["people"].as_array().unwrap();
    assert_eq!(people.len(), 1, "{v}");
    assert_eq!(people[0]["userId"], json!(bob.id));
    let (s, v) = app
        .post(
            &format!("{BRIDGE}/directory/resolve"),
            auth,
            json!({ "userIds": [bob.id] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let bob_principal = v["people"][0]["principalId"].clone();
    assert!(bob_principal.is_string());
    let (s, v) = app
        .post(
            &format!("{BRIDGE}/directory/resolve"),
            Auth::Bearer(&alice.token),
            json!({ "userIds": [mallory.id] }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    // Mallory gets a principal of her own; Alice cannot learn her name.
    let (_, m) = app
        .get(format!("{BRIDGE}/me"), Auth::Bearer(&mallory.token))
        .await;
    let (s, v) = app
        .post(
            &format!("{BRIDGE}/principals/names"),
            Auth::Bearer(&alice.token),
            json!({ "principalIds": [bob_principal, m["principalId"]] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let named = v["principals"].as_array().unwrap();
    assert_eq!(named.len(), 1, "{v}");
    assert_eq!(named[0]["principalId"], bob_principal);
    // Anonymous callers get nothing from any of the client endpoints.
    for (method, path, body) in [
        (Method::GET, format!("{BRIDGE}/me"), None),
        (Method::GET, format!("{BRIDGE}/directory"), None),
        (
            Method::POST,
            format!("{BRIDGE}/directory/resolve"),
            Some(json!({ "userIds": [bob.id] })),
        ),
        (
            Method::POST,
            format!("{BRIDGE}/principals/names"),
            Some(json!({ "principalIds": [bob_principal] })),
        ),
    ] {
        let (s, v) = app.call(method, &path, Auth::None, body).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{path}: {v}");
    }
}
