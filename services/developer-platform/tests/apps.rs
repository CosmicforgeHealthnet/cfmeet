//! ADR 0064 / 0065 — CF Meet Apps, the Developer Console part: app registration and
//! immutable versions, the client secret shown once, the operator review (named reviewers,
//! no self-review, Platform Access sync on every transition), the anonymous OAuth
//! endpoints (client authentication, RFC errors, userinfo), the app directory, the
//! APP_GRANT projection and the install routing pass, and every flag off. REAL router and
//! service tokens; Platform Access's OAuth backend and the Meeting Service's userinfo are
//! in-memory fakes that follow the integration contract.

mod common;

use std::sync::{Arc, Mutex as StdMutex};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use cf_service_auth::CorrelationId;
use common::*;
use developer_platform::{
    apps::{
        installations,
        port::{AccountProfiles, OAuthReply, PlatformOAuthPort},
        AppSettings,
    },
    config::BuildFeatures,
    platform_access::AuthorityUnavailable,
    webhooks::{DeliveryMode, WebhookSettings},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";
const WH: &str = "platform-developer/webhooks/v1.schema.json#/$defs/";
const SECRET: &str =
    "cfcs_sbx_0123456789abcdef01234567_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

// ---------------------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------------------

/// Platform Access's OAuth backend (contract §2), scripted.
#[derive(Default)]
struct FakeOAuth {
    /// Every client profile PUT: (application id, body).
    clients: StdMutex<Vec<(Uuid, Value)>>,
    /// Every token / revoke request body.
    tokens: StdMutex<Vec<Value>>,
    revokes: StdMutex<Vec<Value>>,
    /// The next token answer (None: a valid exchange).
    token_reply: StdMutex<Option<OAuthReply>>,
    /// access token -> introspection.
    introspection: StdMutex<std::collections::HashMap<String, Value>>,
    /// The grants of every app.
    grants: StdMutex<Vec<Value>>,
    /// The change feed (sequence order).
    feed: StdMutex<Vec<Value>>,
    /// Refuse client changes with this (status, code).
    refuse_clients: StdMutex<Option<(u16, String)>>,
    down: std::sync::atomic::AtomicBool,
}

impl FakeOAuth {
    fn check(&self) -> Result<(), AuthorityUnavailable> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            Err(AuthorityUnavailable("down".into()))
        } else {
            Ok(())
        }
    }

    fn last_client(&self, app: Uuid) -> Value {
        self.clients
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(a, _)| *a == app)
            .map(|(_, b)| b.clone())
            .expect("a client profile was synced")
    }
}

#[async_trait::async_trait]
impl PlatformOAuthPort for FakeOAuth {
    async fn put_client(
        &self,
        app_id: Uuid,
        body: &Value,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.check()?;
        if let Some((status, code)) = self.refuse_clients.lock().unwrap().clone() {
            return Ok(OAuthReply::Refused { status, code });
        }
        self.clients.lock().unwrap().push((app_id, body.clone()));
        let mut v = body.clone();
        v["clientId"] = json!(app_id);
        Ok(OAuthReply::Ok {
            status: 200,
            body: v,
        })
    }

    async fn issue_secret(
        &self,
        _: Uuid,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.check()?;
        Ok(OAuthReply::Ok {
            status: 201,
            body: json!({ "clientSecret": SECRET, "secretId": format!("sec_{}", Uuid::new_v4().simple()),
                "issuedAt": chrono::Utc::now().to_rfc3339() }),
        })
    }

    async fn client_grants(
        &self,
        app_id: Uuid,
        _: Option<&str>,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.check()?;
        let items: Vec<Value> = self
            .grants
            .lock()
            .unwrap()
            .iter()
            .filter(|g| g["appId"] == json!(app_id))
            .cloned()
            .collect();
        Ok(OAuthReply::Ok {
            status: 200,
            body: json!({ "items": items, "nextCursor": null }),
        })
    }

    async fn token(
        &self,
        body: &Value,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.check()?;
        self.tokens.lock().unwrap().push(body.clone());
        if let Some(r) = self.token_reply.lock().unwrap().clone() {
            return Ok(r);
        }
        if body["clientSecret"] != SECRET {
            return Ok(OAuthReply::Refused {
                status: 401,
                code: "invalid_client".into(),
            });
        }
        Ok(OAuthReply::Ok {
            status: 200,
            body: json!({ "accessToken": "cfoa_sbx_access", "tokenType": "Bearer", "expiresIn": 3600,
                "refreshToken": "cfor_sbx_refresh", "scope": "profile:read meetings:write",
                "grant": { "id": Uuid::new_v4(), "target": "ORGANIZATION", "organizationId": Uuid::new_v4() },
                "botToken": "cfob_sbx_bot" }),
        })
    }

    async fn revoke(
        &self,
        body: &Value,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.check()?;
        self.revokes.lock().unwrap().push(body.clone());
        Ok(OAuthReply::Ok {
            status: 200,
            body: json!({}),
        })
    }

    async fn introspect(
        &self,
        access_token: &str,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.check()?;
        Ok(OAuthReply::Ok {
            status: 200,
            body: self
                .introspection
                .lock()
                .unwrap()
                .get(access_token)
                .cloned()
                .unwrap_or(json!({ "active": false })),
        })
    }

    async fn identity_changes(
        &self,
        after: i64,
        limit: u32,
        _: &CorrelationId,
    ) -> Result<OAuthReply, AuthorityUnavailable> {
        self.check()?;
        let feed = self.feed.lock().unwrap();
        let page: Vec<Value> = feed
            .iter()
            .filter(|c| c["sequence"].as_i64().unwrap() > after)
            .take(limit as usize)
            .cloned()
            .collect();
        let next = page
            .last()
            .and_then(|c| c["sequence"].as_i64())
            .unwrap_or(after);
        let more = feed.iter().any(|c| c["sequence"].as_i64().unwrap() > next);
        Ok(OAuthReply::Ok {
            status: 200,
            body: json!({ "changes": page, "nextAfter": next, "hasMore": more }),
        })
    }
}

/// The Meeting Service's userinfo (contract §3).
#[derive(Default)]
struct FakeProfiles {
    calls: StdMutex<Vec<(Uuid, Vec<String>)>>,
}

#[async_trait::async_trait]
impl AccountProfiles for FakeProfiles {
    async fn userinfo(
        &self,
        account_id: Uuid,
        scopes: &[String],
        _: &CorrelationId,
    ) -> Result<Option<Value>, AuthorityUnavailable> {
        self.calls
            .lock()
            .unwrap()
            .push((account_id, scopes.to_vec()));
        Ok(Some(json!({ "displayName": "Ada Lovelace",
            "avatarUrl": "https://cdn.cfmeet.com/a.png", "email": "ada@example.com" })))
    }
}

fn all_on() -> BuildFeatures {
    BuildFeatures {
        apps: true,
        oauth: true,
        app_directory: true,
        install_events: true,
        ..Default::default()
    }
}

struct World {
    c: Control,
    oauth: Arc<FakeOAuth>,
    profiles: Arc<FakeProfiles>,
}

async fn world(build: BuildFeatures) -> Option<World> {
    let oauth = Arc::new(FakeOAuth::default());
    let profiles = Arc::new(FakeProfiles::default());
    let (o, p) = (oauth.clone(), profiles.clone());
    let c = Control::with_state(move |s| {
        s.with_build(build)
            .with_oauth(o)
            .with_account_profiles(p)
            .with_app_settings(AppSettings {
                reviewers: vec!["ops.tester".into()],
                ..Default::default()
            })
    })
    .await?;
    Some(World { c, oauth, profiles })
}

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

/// Organization -> project (and a SANDBOX environment).
async fn workspace(c: &Control, owner: &str) -> (Uuid, Uuid, Uuid) {
    let o = c
        .req(
            As::User(owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Calendar Co" })),
        )
        .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let o = id(&o.json);
    let p = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "Calendar sync" })),
        )
        .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.json);
    let p = id(&p.json);
    let e = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/projects/{p}/environments"),
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    (o, p, id(&e.json))
}

async fn join(c: &Control, owner: &str, org: Uuid, email: &str, role: &str) -> (String, Uuid) {
    let (token, user) = c.sessions.sign_in(email);
    let inv = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/organizations/{org}/invitations"),
            Some(json!({ "email": email, "role": role })),
        )
        .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.json);
    let acc = c
        .req(
            As::User(&token),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": inv.json["token"] })),
        )
        .await;
    assert_eq!(acc.status, StatusCode::OK, "{}", acc.json);
    (token, user)
}

fn manifest() -> Value {
    json!({
        "name": "Calendar Sync",
        "description": "Creates CF Meet meetings from your calendar.",
        "logoUrl": "https://calendar.example.com/logo.png",
        "privacyUrl": "https://calendar.example.com/privacy",
        "termsUrl": "https://calendar.example.com/terms",
        "redirectUris": ["https://calendar.example.com/oauth/callback"],
        "userScopes": ["profile:read", "meetings:write", "events:meetings"],
        "botScopes": ["chat:write:bot", "chat:read:bot"],
        "eventTypes": ["meeting.started", "participant.joined", "bot.mentioned", "bot.message_received", "app.installed"],
        "meetingSurface": { "url": "https://panel.calendar.example.com/meet", "origins": [] },
        "distribution": "PUBLIC",
    })
}

/// A raw anonymous request (no gateway service token, no session): `(status, headers, json)`.
async fn anon(
    c: &Control,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Response {
    let mut r = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    let response = c
        .router
        .clone()
        .oneshot(r.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Response {
        status,
        headers,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

const FORM: (&str, &str) = ("content-type", "application/x-www-form-urlencoded");

fn rfc(r: &Response) -> &str {
    r.json["error"].as_str().unwrap_or_default()
}

/// The operator review route of `app`.
fn decide(app: Uuid, command: &str) -> String {
    format!("/internal/v1/operator/apps/{app}/{command}")
}

/// Registers, submits, reviews, approves and publishes the project's app.
async fn publish_app(w: &World, owner: &str, project: Uuid, body: Value) {
    let c = &w.c;
    let r = c
        .req(
            As::User(owner),
            Method::PUT,
            &format!("/v1/projects/{project}/app"),
            Some(body),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.json);
    let r = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/projects/{project}/app/submit"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let reviewer = Some(Uuid::new_v4());
    for command in ["review", "approve", "publish"] {
        let r = c
            .operator_as(Method::POST, &decide(project, command), None, reviewer)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{command}: {}", r.json);
    }
}

// ---------------------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn registration_keeps_immutable_versions_syncs_platform_access_and_shows_the_secret_once() {
    let Some(w) = world(all_on()).await else {
        return;
    };
    let c = &w.c;
    let (owner, owner_id) = c.sessions.sign_in("owner@calendar.example");
    let (org, project, _) = workspace(c, &owner).await;
    let (developer, _) = join(c, &owner, org, "dev@calendar.example", "DEVELOPER").await;
    let (admin, admin_id) = join(c, &owner, org, "admin@calendar.example", "ADMIN").await;
    let base = format!("/v1/projects/{project}/app");
    // Nothing registered yet.
    let r = c.req(As::User(&owner), Method::GET, &base, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // DEVELOPER may not register; an invalid manifest is refused.
    let r = c
        .req(As::User(&developer), Method::PUT, &base, Some(manifest()))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    let mut bad = manifest();
    bad["redirectUris"] = json!(["https://*.example.com/cb"]);
    let r = c.req(As::User(&owner), Method::PUT, &base, Some(bad)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Register (201): version 1, DRAFT, synced to Platform Access.
    let mut draft = manifest();
    draft["distribution"] = json!("PRIVATE");
    draft["redirectUris"] = json!(["http://localhost:8080/cb"]);
    let r = c
        .req(As::User(&owner), Method::PUT, &base, Some(draft.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    conforms(&format!("{CTRL}App"), &r.json);
    assert_eq!(r.json["status"], "DRAFT");
    assert_eq!(r.json["currentVersion"], 1);
    assert_eq!(r.json["clientId"], json!(project));
    let synced = w.oauth.last_client(project);
    assert_eq!(synced["status"], "DRAFT");
    assert_eq!(synced["distribution"], "PRIVATE");
    assert_eq!(synced["redirectUris"], json!(["http://localhost:8080/cb"]));
    // PRIVATE: the workspace members' CF Meet accounts are the allow list.
    let allowed = synced["allowedAccountIds"].as_array().unwrap();
    assert!(allowed.contains(&json!(owner_id)) && allowed.contains(&json!(admin_id)));
    // The consent-page and in-meeting fields come from the live manifest and workspace.
    assert_eq!(synced["developerName"], "Calendar Co");
    assert_eq!(synced["logoUrl"], "https://calendar.example.com/logo.png");
    assert_eq!(
        synced["meetingSurface"],
        json!({ "url": "https://panel.calendar.example.com/meet",
                "origins": ["https://panel.calendar.example.com"] })
    );
    assert!(synced.get("allowedOrganizationIds").is_none());
    // The same manifest again adds no version.
    let r = c
        .req(As::User(&admin), Method::PUT, &base, Some(draft.clone()))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["currentVersion"], 1);
    // A change is a new immutable version.
    let r = c
        .req(As::User(&admin), Method::PUT, &base, Some(manifest()))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["currentVersion"], 2);
    assert_eq!(r.json["manifest"]["distribution"], "PUBLIC");
    assert!(w
        .oauth
        .last_client(project)
        .get("allowedAccountIds")
        .is_none());
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_app_versions WHERE app_id = '{project}'"
        ))
        .await,
        2
    );
    assert!(c
        .db
        .conn
        .execute_unprepared("UPDATE devplat_app_versions SET created_by = 'x'")
        .await
        .is_err());
    // Platform Access refusing the profile changes nothing here.
    *w.oauth.refuse_clients.lock().unwrap() = Some((400, "INVALID_REDIRECT_URI".into()));
    let mut third = manifest();
    third["name"] = json!("Calendar Sync 3");
    let r = c
        .req(As::User(&owner), Method::PUT, &base, Some(third))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    *w.oauth.refuse_clients.lock().unwrap() = None;
    let r = c.req(As::User(&owner), Method::GET, &base, None).await;
    assert_eq!(r.json["currentVersion"], 2);
    // The secret is shown once; a replay never carries it.
    let secret_path = format!("/v1/projects/{project}/app/secret");
    let s = c
        .req_with(
            As::User(&owner),
            Method::POST,
            &secret_path,
            None,
            &[("idempotency-key", "rotate-1")],
        )
        .await;
    assert_eq!(s.status, StatusCode::CREATED, "{}", s.json);
    conforms(&format!("{CTRL}AppClientSecret"), &s.json);
    assert_eq!(s.json["clientSecret"], SECRET);
    assert_eq!(s.headers["cache-control"], "no-store");
    let replay = c
        .req_with(
            As::User(&owner),
            Method::POST,
            &secret_path,
            None,
            &[("idempotency-key", "rotate-1")],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert!(replay.json.get("clientSecret").is_none());
    assert_eq!(replay.json["clientSecretRedacted"], true);
    let stored = c
        .count("SELECT count(*) FROM devplat_idempotency_records WHERE response_body::text LIKE '%cfcs_%'")
        .await;
    assert_eq!(stored, 0, "the secret is never stored");
    let r = c.req(As::User(&owner), Method::GET, &base, None).await;
    assert!(r.json["secretIssuedAt"].is_string());
    assert!(!r.json.to_string().contains("cfcs_"));
    // Test accounts: format only, at most 25, synced.
    let tester = Uuid::new_v4();
    let t = c
        .req(
            As::User(&owner),
            Method::PUT,
            &format!("/v1/projects/{project}/app/test-accounts"),
            Some(json!({ "accountIds": [tester, tester] })),
        )
        .await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.json);
    assert_eq!(t.json["testAccountIds"], json!([tester]));
    assert_eq!(
        w.oauth.last_client(project)["testAccountIds"],
        json!([tester])
    );
    let too_many: Vec<Uuid> = (0..26).map(|_| Uuid::new_v4()).collect();
    let t = c
        .req(
            As::User(&owner),
            Method::PUT,
            &format!("/v1/projects/{project}/app/test-accounts"),
            Some(json!({ "accountIds": too_many })),
        )
        .await;
    assert_eq!(t.status, StatusCode::BAD_REQUEST);
    let t = c
        .req(
            As::User(&owner),
            Method::PUT,
            &format!("/v1/projects/{project}/app/test-accounts"),
            Some(json!({ "accountIds": ["not-a-uuid"] })),
        )
        .await;
    assert_eq!(t.status, StatusCode::BAD_REQUEST);
    // Grants come from Platform Access, without CF Meet account ids.
    w.oauth.grants.lock().unwrap().push(json!({
        "id": Uuid::new_v4(), "appId": project, "target": "USER", "accountId": Uuid::new_v4(),
        "scopes": ["profile:read"], "status": "ACTIVE", "createdAt": "2026-11-18T10:00:00Z" }));
    let g = c
        .req(
            As::User(&admin),
            Method::GET,
            &format!("/v1/projects/{project}/app/grants"),
            None,
        )
        .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.json);
    conforms(&format!("{CTRL}AppGrantList"), &g.json);
    assert_eq!(g.json["items"].as_array().unwrap().len(), 1);
    assert!(!g.json.to_string().contains("accountId"));
    // Machine credentials never manage apps; activity is recorded without secrets.
    let acts = c
        .count(&format!(
            "SELECT count(*) FROM devplat_activity_events WHERE target_id = '{project}' AND action LIKE 'app.%'"
        ))
        .await;
    assert!(acts >= 4, "{acts}");
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_activity_events WHERE summary::text LIKE '%cfcs_%'")
            .await,
        0
    );
    // Platform Access down: fail closed.
    w.oauth
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let r = c
        .req(As::User(&owner), Method::POST, &secret_path, None)
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

use sea_orm::ConnectionTrait;

// ---------------------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn review_needs_named_non_member_reviewers_and_syncs_every_transition() {
    let Some(w) = world(all_on()).await else {
        return;
    };
    let c = &w.c;
    let (owner, owner_id) = c.sessions.sign_in("owner@calendar.example");
    let (_, project, _) = workspace(c, &owner).await;
    let base = format!("/v1/projects/{project}/app");
    // A loopback redirect cannot go to review.
    let mut local = manifest();
    local["redirectUris"] = json!(["http://localhost:3000/cb"]);
    c.req(As::User(&owner), Method::PUT, &base, Some(local))
        .await;
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("{base}/submit"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let r = c
        .req(As::User(&owner), Method::PUT, &base, Some(manifest()))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("{base}/submit"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["status"], "SUBMITTED");
    assert_eq!(r.json["submittedVersion"], 2);
    // Locked during review.
    let r = c
        .req(As::User(&owner), Method::PUT, &base, Some(local_name("x")))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // The operator list and view.
    let l = c
        .operator(
            Method::GET,
            "/internal/v1/operator/apps?status=SUBMITTED",
            None,
        )
        .await;
    assert_eq!(l.status, StatusCode::OK, "{}", l.json);
    conforms(&format!("{CTRL}AppList"), &l.json);
    assert_eq!(l.json["items"].as_array().unwrap().len(), 1);
    let g = c
        .operator(
            Method::GET,
            &format!("/internal/v1/operator/apps/{project}"),
            None,
        )
        .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.json);
    conforms(&format!("{CTRL}OperatorApp"), &g.json);
    assert_eq!(g.json["submittedManifest"]["name"], "Calendar Sync");
    // No reviewer id: OPERATOR_REQUIRED; a member of the workspace: no self-review.
    let r = c
        .operator_as(Method::POST, &decide(project, "review"), None, None)
        .await;
    assert_eq!(r.json["error"]["code"], "OPERATOR_REQUIRED");
    let r = c
        .operator_as(
            Method::POST,
            &decide(project, "review"),
            None,
            Some(owner_id),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"]["code"], "SELF_APPROVAL_FORBIDDEN");
    let reviewer = Some(Uuid::new_v4());
    // Approving before review is an invalid transition.
    let r = c
        .operator_as(Method::POST, &decide(project, "approve"), None, reviewer)
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = c
        .operator_as(Method::POST, &decide(project, "review"), None, reviewer)
        .await;
    assert_eq!(r.json["status"], "IN_REVIEW", "{}", r.json);
    let r = c
        .operator_as(Method::POST, &decide(project, "approve"), None, reviewer)
        .await;
    assert_eq!(r.json["status"], "APPROVED", "{}", r.json);
    assert_eq!(w.oauth.last_client(project)["status"], "APPROVED");
    let r = c
        .operator_as(Method::POST, &decide(project, "publish"), None, reviewer)
        .await;
    assert_eq!(r.json["status"], "PUBLISHED", "{}", r.json);
    assert_eq!(r.json["listedInDirectory"], true);
    assert_eq!(w.oauth.last_client(project)["status"], "PUBLISHED");
    // Changes to a published app wait for a new review; Platform Access keeps the live one.
    let r = c
        .req(
            As::User(&owner),
            Method::PUT,
            &base,
            Some(local_name("Calendar Sync Pro")),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["pendingChanges"], true);
    assert_eq!(r.json["liveVersion"], 2);
    assert_eq!(w.oauth.last_client(project)["name"], "Calendar Sync");
    // A rejection states its reason; the published version stays live.
    c.req(
        As::User(&owner),
        Method::POST,
        &format!("{base}/submit"),
        None,
    )
    .await;
    let r = c
        .operator_as(
            Method::POST,
            &decide(project, "reject"),
            Some(json!({ "reasonCode": "MISLEADING_NAME" })),
            reviewer,
        )
        .await;
    assert_eq!(r.json["status"], "REJECTED", "{}", r.json);
    assert_eq!(r.json["review"]["reasonCode"], "MISLEADING_NAME");
    assert_eq!(w.oauth.last_client(project)["status"], "PUBLISHED");
    // Suspension: any administering operator; Platform Access suspends the client.
    let r = c
        .operator_as(Method::POST, &decide(project, "suspend"), None, None)
        .await;
    assert_eq!(r.json["status"], "SUSPENDED", "{}", r.json);
    assert_eq!(w.oauth.last_client(project)["status"], "SUSPENDED");
    assert_eq!(r.json["listedInDirectory"], false);
    // Unknown command; audited history.
    let r = c
        .operator_as(
            Method::POST,
            &decide(project, "auto-approve"),
            None,
            reviewer,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let transitions = c
        .count(&format!(
            "SELECT count(*) FROM devplat_app_transitions WHERE app_id = '{project}'"
        ))
        .await;
    assert!(transitions >= 8, "{transitions}");
    let operator_events = c
        .count(&format!(
            "SELECT count(*) FROM devplat_activity_events WHERE target_id = '{project}' AND actor_kind = 'OPERATOR'"
        ))
        .await;
    assert_eq!(operator_events, 5);
}

fn local_name(name: &str) -> Value {
    let mut m = manifest();
    m["name"] = json!(name);
    m
}

#[tokio::test]
async fn without_named_reviewers_no_app_decision_is_possible() {
    let oauth = Arc::new(FakeOAuth::default());
    let o = oauth.clone();
    let Some(c) = Control::with_state(move |s| s.with_build(all_on()).with_oauth(o)).await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@calendar.example");
    let (_, project, _) = workspace(&c, &owner).await;
    c.req(
        As::User(&owner),
        Method::PUT,
        &format!("/v1/projects/{project}/app"),
        Some(manifest()),
    )
    .await;
    c.req(
        As::User(&owner),
        Method::POST,
        &format!("/v1/projects/{project}/app/submit"),
        None,
    )
    .await;
    let r = c
        .operator_as(
            Method::POST,
            &decide(project, "review"),
            None,
            Some(Uuid::new_v4()),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "POLICY_VIOLATION", "{}", r.json);
    // Without a logo or a meeting surface, the optional profile fields are left out.
    let mut plain = manifest();
    plain["logoUrl"] = Value::Null;
    plain["meetingSurface"] = Value::Null;
    let (_, other, _) = workspace(&c, &owner).await;
    let r = c
        .req(
            As::User(&owner),
            Method::PUT,
            &format!("/v1/projects/{other}/app"),
            Some(plain),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let synced = oauth.last_client(other);
    assert!(synced.get("logoUrl").is_none() && synced.get("meetingSurface").is_none());
    assert_eq!(synced["developerName"], "Calendar Co");
}

// ---------------------------------------------------------------------------------------
// OAuth endpoints
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_token_endpoint_authenticates_clients_and_maps_platform_access_answers() {
    let Some(w) = world(all_on()).await else {
        return;
    };
    let c = &w.c;
    let client = Uuid::new_v4();
    let code = "authcode-123";
    let verifier = "v".repeat(43);
    let body = format!(
        "grant_type=authorization_code&code={code}&redirect_uri=https%3A%2F%2Fcalendar.example.com%2Fcb&code_verifier={verifier}"
    );
    // client_secret_basic, without any gateway caller.
    let basic = format!("Basic {}", STANDARD.encode(format!("{client}:{SECRET}")));
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[FORM, ("authorization", basic.as_str())],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}OAuthToken"), &r.json);
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(r.json["token_type"], "Bearer");
    assert_eq!(r.json["bot_token"], "cfob_sbx_bot");
    assert!(r.json["installation"]["organization_id"].is_string());
    let sent = w.oauth.tokens.lock().unwrap().last().cloned().unwrap();
    assert_eq!(sent["clientId"], json!(client));
    assert_eq!(sent["grantType"], "authorization_code");
    assert_eq!(sent["redirectUri"], "https://calendar.example.com/cb");
    assert_eq!(sent["codeVerifier"], json!(verifier));
    // client_secret_post with a refresh token.
    let post = format!("grant_type=refresh_token&refresh_token=cfor_sbx_r&client_id={client}&client_secret={SECRET}");
    let r = anon(c, Method::POST, "/v1/oauth/token", &[FORM], &post).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        w.oauth.tokens.lock().unwrap().last().unwrap()["refreshToken"],
        "cfor_sbx_r"
    );
    // RFC errors.
    let wrong = format!("Basic {}", STANDARD.encode(format!("{client}:cfcs_wrong")));
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[FORM, ("authorization", wrong.as_str())],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    conforms(&format!("{CTRL}OAuthError"), &r.json);
    assert_eq!(rfc(&r), "invalid_client");
    assert!(r.headers.contains_key("www-authenticate"));
    let both = format!("{body}&client_secret={SECRET}");
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[FORM, ("authorization", basic.as_str())],
        &both,
    )
    .await;
    assert_eq!(
        rfc(&r),
        "invalid_request",
        "one client authentication method"
    );
    let r = anon(c, Method::POST, "/v1/oauth/token", &[FORM], &body).await;
    assert_eq!(rfc(&r), "invalid_client", "no client authentication");
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[FORM, ("authorization", basic.as_str())],
        "grant_type=password&username=a&password=b",
    )
    .await;
    assert_eq!(rfc(&r), "unsupported_grant_type");
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[FORM, ("authorization", basic.as_str())],
        &format!("grant_type=authorization_code&code={code}&redirect_uri=https%3A%2F%2Fx"),
    )
    .await;
    assert_eq!(rfc(&r), "invalid_request", "PKCE is mandatory");
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[
            ("content-type", "application/json"),
            ("authorization", basic.as_str()),
        ],
        "{}",
    )
    .await;
    assert_eq!(rfc(&r), "invalid_request", "form encoding only");
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[FORM, ("authorization", basic.as_str())],
        &format!("{body}&code=again"),
    )
    .await;
    assert_eq!(rfc(&r), "invalid_request", "a parameter appears once");
    // Platform Access's RFC codes pass through; unavailable is temporarily_unavailable.
    *w.oauth.token_reply.lock().unwrap() = Some(OAuthReply::Refused {
        status: 400,
        code: "invalid_grant".into(),
    });
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[FORM, ("authorization", basic.as_str())],
        &body,
    )
    .await;
    assert_eq!(
        (r.status, rfc(&r)),
        (StatusCode::BAD_REQUEST, "invalid_grant")
    );
    assert!(!r.json.to_string().contains(code));
    *w.oauth.token_reply.lock().unwrap() = None;
    w.oauth
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[FORM, ("authorization", basic.as_str())],
        &body,
    )
    .await;
    assert_eq!(
        (r.status, rfc(&r)),
        (StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable")
    );
    w.oauth
        .down
        .store(false, std::sync::atomic::Ordering::SeqCst);
    // Revocation: 200 whatever the token, client authentication required.
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/revoke",
        &[FORM, ("authorization", basic.as_str())],
        "token=cfor_sbx_whatever&token_type_hint=refresh_token",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(
        w.oauth.revokes.lock().unwrap().last().unwrap()["token"],
        "cfor_sbx_whatever"
    );
    let r = anon(c, Method::POST, "/v1/oauth/revoke", &[FORM], "token=x").await;
    assert_eq!(rfc(&r), "invalid_client");
}

#[tokio::test]
async fn userinfo_introspects_and_reads_the_meeting_service_profile() {
    let Some(w) = world(all_on()).await else {
        return;
    };
    let c = &w.c;
    let account = Uuid::new_v4();
    w.oauth.introspection.lock().unwrap().insert(
        "cfoa_sbx_profile".into(),
        json!({ "active": true, "grantId": Uuid::new_v4(), "appId": Uuid::new_v4(),
            "accountId": account, "principalId": Uuid::new_v4(), "sub": "pairwise-sub-1",
            "scopes": ["profile:read"] }),
    );
    w.oauth.introspection.lock().unwrap().insert(
        "cfoa_sbx_email".into(),
        json!({ "active": true, "accountId": account, "sub": "pairwise-sub-1",
            "scopes": ["profile:read", "profile:email"] }),
    );
    w.oauth.introspection.lock().unwrap().insert(
        "cfoa_sbx_meetings".into(),
        json!({ "active": true, "accountId": account, "sub": "s", "scopes": ["meetings:write"] }),
    );
    let get = |t: &'static str| {
        let h = format!("Bearer {t}");
        async move {
            anon(
                c,
                Method::GET,
                "/v1/oauth/userinfo",
                &[("authorization", h.as_str())],
                "",
            )
            .await
        }
    };
    let r = get("cfoa_sbx_profile").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}OAuthUserinfo"), &r.json);
    assert_eq!(r.json["sub"], "pairwise-sub-1");
    assert_eq!(r.json["name"], "Ada Lovelace");
    assert!(
        r.json.get("email").is_none(),
        "email only with profile:email"
    );
    assert_eq!(
        w.profiles.calls.lock().unwrap().last().unwrap(),
        &(account, vec!["profile:read".to_string()])
    );
    let r = get("cfoa_sbx_email").await;
    assert_eq!(r.json["email"], "ada@example.com");
    let r = get("cfoa_sbx_meetings").await;
    assert_eq!(
        (r.status, rfc(&r)),
        (StatusCode::FORBIDDEN, "insufficient_scope")
    );
    let r = get("cfoa_sbx_unknown").await;
    assert_eq!(
        (r.status, rfc(&r)),
        (StatusCode::UNAUTHORIZED, "invalid_token")
    );
    let r = get("cfob_sbx_bot").await;
    assert_eq!(rfc(&r), "invalid_token", "bot tokens have no user profile");
    let r = anon(c, Method::GET, "/v1/oauth/userinfo", &[], "").await;
    assert_eq!(rfc(&r), "invalid_token");
    // Metadata.
    let m = anon(c, Method::GET, "/v1/oauth/metadata", &[], "").await;
    assert_eq!(m.status, StatusCode::OK, "{}", m.json);
    conforms(&format!("{CTRL}OAuthMetadata"), &m.json);
    assert_eq!(
        m.json["authorization_endpoint"],
        "https://cfmeet.com/oauth/authorize"
    );
    assert_eq!(
        m.json["token_endpoint"],
        "https://api.cfmeet.com/v1/oauth/token"
    );
    assert_eq!(m.json["code_challenge_methods_supported"], json!(["S256"]));
}

/// The anonymous routes work without a gateway caller; every other console route still
/// requires one.
#[tokio::test]
async fn anonymous_routes_need_no_gateway_caller_and_the_console_still_does() {
    let Some(w) = world(all_on()).await else {
        return;
    };
    let c = &w.c;
    for path in ["/v1/oauth/metadata", "/v1/app-directory"] {
        let r = anon(c, Method::GET, path, &[], "").await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.json);
    }
    let project = Uuid::new_v4();
    for path in [
        format!("/v1/projects/{project}/app"),
        "/v1/organizations".to_string(),
        "/v1/oauth".to_string(),
        "/v1/oauth/unknown".to_string(),
    ] {
        let r = anon(c, Method::GET, &path, &[], "").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(r.json["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
    }
}

// ---------------------------------------------------------------------------------------
// Directory
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_directory_lists_only_published_public_apps() {
    let Some(w) = world(all_on()).await else {
        return;
    };
    let c = &w.c;
    let (owner, _) = c.sessions.sign_in("owner@calendar.example");
    let (_, public, _) = workspace(c, &owner).await;
    publish_app(&w, &owner, public, manifest()).await;
    // A PRIVATE published app, and a PUBLIC one still in draft, are not listed.
    let (_, private, _) = workspace(c, &owner).await;
    let mut m = manifest();
    m["distribution"] = json!("PRIVATE");
    publish_app(&w, &owner, private, m).await;
    let (_, draft, _) = workspace(c, &owner).await;
    c.req(
        As::User(&owner),
        Method::PUT,
        &format!("/v1/projects/{draft}/app"),
        Some(manifest()),
    )
    .await;
    let l = anon(c, Method::GET, "/v1/app-directory", &[], "").await;
    assert_eq!(l.status, StatusCode::OK, "{}", l.json);
    conforms(&format!("{CTRL}DirectoryAppList"), &l.json);
    let items = l.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{}", l.json);
    assert_eq!(items[0]["appId"], json!(public));
    assert_eq!(items[0]["developerName"], "Calendar Co");
    assert_eq!(items[0]["meetingSurface"], true);
    let one = anon(
        c,
        Method::GET,
        &format!("/v1/app-directory/{public}"),
        &[],
        "",
    )
    .await;
    assert_eq!(one.status, StatusCode::OK);
    conforms(&format!("{CTRL}DirectoryApp"), &one.json);
    for hidden in [private, draft, Uuid::new_v4()] {
        let r = anon(
            c,
            Method::GET,
            &format!("/v1/app-directory/{hidden}"),
            &[],
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
    let r = anon(c, Method::GET, "/v1/app-directory?limit=0", &[], "").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Suspension removes it at once.
    c.operator_as(Method::POST, &decide(public, "suspend"), None, None)
        .await;
    let l = anon(c, Method::GET, "/v1/app-directory", &[], "").await;
    assert!(l.json["items"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------
// Install events
// ---------------------------------------------------------------------------------------

fn webhook_settings() -> WebhookSettings {
    WebhookSettings {
        enabled: true,
        key_ring: Some(Arc::new(
            cf_webhooks::secret_box::SecretBox::from_bytes(&std::array::from_fn::<u8, 32, _>(
                |i| 42u8.wrapping_add((i as u8).wrapping_mul(7)),
            ))
            .unwrap(),
        )),
        policy: cf_webhooks::endpoint::EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        },
        mode: DeliveryMode::Shadow,
        ..Default::default()
    }
}

fn grant_change(sequence: i64, snapshot: Value) -> Value {
    json!({ "sequence": sequence, "entityKind": "APP_GRANT", "operation": "UPSERT",
        "entityKey": snapshot["id"], "revision": sequence, "occurredAt": "2026-11-18T10:00:00Z",
        "snapshot": snapshot })
}

fn meeting_envelope(scope: (Uuid, Uuid, Uuid), event: &str, payload: Value) -> Value {
    json!({
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": event, "schema_version": 1, "occurred_at": "2026-11-18T10:05:00Z",
        "producer_service": "cfmeet.meeting-service", "runtime_environment": "test",
        "correlation_id": "corr_0a1b2c3d4e5f60718293a4b5c6d7e8f9",
        "ownership": { "tenant_id": scope.0, "application_id": scope.1,
            "application_environment_id": scope.2 },
        "payload": payload,
    })
}

#[tokio::test]
async fn install_events_reach_installed_apps_with_the_installation_id() {
    let oauth = Arc::new(FakeOAuth::default());
    let o = oauth.clone();
    let Some((c, wh)) = Control::with_webhooks_configured(webhook_settings(), move |mut s| {
        s.webhooks = s.webhooks.map(|w| w.with_install_events(true));
        s.with_build(all_on())
            .with_oauth(o)
            .with_app_settings(AppSettings {
                reviewers: vec!["ops.tester".into()],
                ..Default::default()
            })
    })
    .await
    else {
        return;
    };
    let w = World {
        c,
        oauth: oauth.clone(),
        profiles: Arc::new(FakeProfiles::default()),
    };
    let c = &w.c;
    let (owner, _) = c.sessions.sign_in("owner@calendar.example");
    let (_, project, env) = workspace(c, &owner).await;
    publish_app(&w, &owner, project, manifest()).await;
    // The app subscribes in its own environment.
    let hook = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{env}/webhooks"),
            Some(json!({ "url": "http://127.0.0.1:9/hook",
                "eventTypes": ["meeting.started", "participant.joined", "bot.mentioned", "bot.message_received", "app.installed", "app.uninstalled"] })),
        )
        .await;
    assert_eq!(hook.status, StatusCode::CREATED, "{}", hook.json);
    // Platform Access's feed: an organization install and a user install.
    let org_scope = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let user_scope = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (org_grant, user_grant) = (Uuid::new_v4(), Uuid::new_v4());
    let (bot, user_principal) = (Uuid::new_v4(), Uuid::new_v4());
    let snapshot = |grant: Uuid,
                    target: &str,
                    principal: Uuid,
                    scope: (Uuid, Uuid, Uuid),
                    scopes: Value,
                    status: &str| {
        json!({ "id": grant, "appId": project, "target": target, "organizationId": if target == "ORGANIZATION" { json!(Uuid::new_v4()) } else { Value::Null },
            "accountId": if target == "USER" { json!(Uuid::new_v4()) } else { Value::Null },
            "installedByAccountId": Uuid::new_v4(), "tenantId": scope.0, "applicationId": scope.1,
            "environmentId": scope.2, "principalId": principal, "serviceAccountId": Uuid::new_v4(),
            "scopes": scopes, "status": status, "revokedAt": null, "revokeReason": null, "revision": 1,
            "createdAt": "2026-11-18T10:00:00Z", "updatedAt": "2026-11-18T10:00:00Z" })
    };
    {
        let mut feed = oauth.feed.lock().unwrap();
        feed.push(
            json!({ "sequence": 1, "entityKind": "TENANT", "operation": "UPSERT", "entityKey": "t",
            "revision": 1, "occurredAt": "2026-11-18T09:00:00Z", "snapshot": {} }),
        );
        feed.push(grant_change(
            2,
            snapshot(
                org_grant,
                "ORGANIZATION",
                bot,
                org_scope,
                json!(["chat:read:bot", "chat:write:bot", "events:meetings"]),
                "ACTIVE",
            ),
        ));
        feed.push(grant_change(
            3,
            snapshot(
                user_grant,
                "USER",
                user_principal,
                user_scope,
                json!(["profile:read", "events:meetings"]),
                "ACTIVE",
            ),
        ));
    }
    let report = installations::sync_once(&c.db.conn, oauth.as_ref(), Some(&wh.service))
        .await
        .unwrap();
    assert_eq!(report.applied, 2, "{report:?}");
    assert_eq!(report.deliveries, 2, "app.installed for both installs");
    // Re-reading the feed changes nothing.
    let again = installations::sync_once(&c.db.conn, oauth.as_ref(), Some(&wh.service))
        .await
        .unwrap();
    assert_eq!(again.applied, 0);
    let installed = c
        .count("SELECT count(*) FROM devplat_webhook_events WHERE event_type = 'app.installed'")
        .await;
    assert_eq!(installed, 2);
    let count_routed = |grant: Uuid, event_type: &str| {
        let sql = format!(
            "SELECT count(*) FROM devplat_webhook_events e JOIN devplat_webhook_deliveries d \
               ON d.event_id = e.event_id WHERE e.environment_id = '{env}' AND e.event_type = '{event_type}' \
               AND e.payload -> 'data' ->> 'installationId' = '{grant}'"
        );
        async move { c.count(&sql).await }
    };
    // An organization meeting event reaches the org install.
    let r = c
        .internal(
            &wh.producers.meeting,
            Method::POST,
            "/internal/v1/platform/events",
            Some(meeting_envelope(
                org_scope,
                "meeting-service.meeting.started",
                json!({ "meeting_id": Uuid::new_v4() }),
            )),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.json);
    assert_eq!(count_routed(org_grant, "meeting.started").await, 1);
    let routed: Value = c
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT payload FROM devplat_webhook_events WHERE environment_id = '{env}' AND event_type = 'meeting.started'"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "payload")
        .unwrap();
    conforms(&format!("{WH}WebhookEvent"), &routed);
    // A user install only receives events naming the user's principal.
    for (principal, expected) in [(Uuid::new_v4(), 0), (user_principal, 1)] {
        c.internal(
            &wh.producers.meeting,
            Method::POST,
            "/internal/v1/platform/events",
            Some(meeting_envelope(user_scope, "meeting-service.participant.joined",
                json!({ "meeting_id": Uuid::new_v4(), "participant_id": Uuid::new_v4(), "principal_id": principal }))),
        )
        .await;
        assert_eq!(
            count_routed(user_grant, "participant.joined").await,
            expected
        );
    }
    // bot.mentioned reaches the org install only for its own bot; never message text.
    for (subject, expected) in [(Uuid::new_v4(), 0), (bot, 1)] {
        let mut env = meeting_envelope(
            org_scope,
            "messaging.bot.mentioned",
            json!({
            "conversation_id": Uuid::new_v4(), "message_id": Uuid::new_v4(),
            "subject_principal_id": subject, "text": "@calendar book a room" }),
        );
        env["producer_service"] = json!("cfmeet.messaging");
        let r = c
            .internal(
                &wh.producers.messaging,
                Method::POST,
                "/internal/v1/platform/events",
                Some(env),
            )
            .await;
        assert!(r.status.is_success(), "{}", r.json);
        assert_eq!(count_routed(org_grant, "bot.mentioned").await, expected);
    }
    // A direct message to the bot (Messaging: ids only).
    let mut dm = meeting_envelope(
        org_scope,
        "messaging.bot.message-received",
        json!({ "conversation_id": Uuid::new_v4(), "message_id": Uuid::new_v4(),
            "sender_principal_id": Uuid::new_v4(), "subject_principal_id": bot }),
    );
    dm["producer_service"] = json!("cfmeet.messaging");
    let r = c
        .internal(
            &wh.producers.messaging,
            Method::POST,
            "/internal/v1/platform/events",
            Some(dm),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.json);
    assert_eq!(count_routed(org_grant, "bot.message_received").await, 1);
    assert_eq!(
        c.count(
            "SELECT count(*) FROM devplat_webhook_events WHERE payload::text LIKE '%book a room%'"
        )
        .await,
        0
    );
    // An event type the app did not declare is never routed (artifact.ready).
    c.internal(
        &wh.producers.meeting,
        Method::POST,
        "/internal/v1/platform/events",
        Some(meeting_envelope(
            org_scope,
            "meeting-service.artifact.ready",
            json!({ "meeting_id": Uuid::new_v4(), "artifact_id": Uuid::new_v4() }),
        )),
    )
    .await;
    assert_eq!(
        c.count(&format!("SELECT count(*) FROM devplat_webhook_events WHERE environment_id = '{env}' AND event_type = 'artifact.ready'"))
            .await,
        0
    );
    // Uninstall: app.uninstalled, and the install no longer receives events.
    let mut revoked = snapshot(
        org_grant,
        "ORGANIZATION",
        bot,
        org_scope,
        json!(["events:meetings"]),
        "REVOKED",
    );
    revoked["revokeReason"] = json!("UNINSTALLED");
    oauth.feed.lock().unwrap().push(grant_change(4, revoked));
    let r = installations::sync_once(&c.db.conn, oauth.as_ref(), Some(&wh.service))
        .await
        .unwrap();
    assert_eq!(r.applied, 1);
    assert_eq!(count_routed(org_grant, "app.uninstalled").await, 1);
    c.internal(
        &wh.producers.meeting,
        Method::POST,
        "/internal/v1/platform/events",
        Some(meeting_envelope(
            org_scope,
            "meeting-service.meeting.started",
            json!({ "meeting_id": Uuid::new_v4() }),
        )),
    )
    .await;
    assert_eq!(count_routed(org_grant, "meeting.started").await, 1);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_app_installations WHERE status = 'ACTIVE'")
            .await,
        1
    );
}

#[tokio::test]
async fn without_the_install_events_flag_ingestion_routes_nothing_to_installs() {
    let Some((c, wh)) = Control::with_webhooks_configured(webhook_settings(), |s| s).await else {
        return;
    };
    let scope = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    c.db.conn
        .execute_unprepared(&format!(
            "INSERT INTO devplat_app_installations (grant_id, app_id, target_kind, target_tenant_id, \
               target_application_id, target_environment_id, scopes, status, feed_sequence, installed_at, updated_at) \
             VALUES (gen_random_uuid(), gen_random_uuid(), 'ORGANIZATION', '{}', '{}', '{}', '{{events:meetings}}', \
               'ACTIVE', 1, now(), now())",
            scope.0, scope.1, scope.2
        ))
        .await
        .unwrap();
    let r = c
        .internal(
            &wh.producers.meeting,
            Method::POST,
            "/internal/v1/platform/events",
            Some(meeting_envelope(
                scope,
                "meeting-service.meeting.started",
                json!({ "meeting_id": Uuid::new_v4() }),
            )),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.json);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_deliveries")
            .await,
        0
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_events").await,
        1
    );
}

// ---------------------------------------------------------------------------------------
// Flags off
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn with_every_flag_off_nothing_changes() {
    let Some(w) = world(BuildFeatures::default()).await else {
        return;
    };
    let c = &w.c;
    let (owner, _) = c.sessions.sign_in("owner@calendar.example");
    let (_, project, _) = workspace(c, &owner).await;
    for (method, path) in [
        (Method::GET, format!("/v1/projects/{project}/app")),
        (Method::PUT, format!("/v1/projects/{project}/app")),
        (Method::POST, format!("/v1/projects/{project}/app/secret")),
        (Method::POST, format!("/v1/projects/{project}/app/submit")),
        (Method::GET, format!("/v1/projects/{project}/app/grants")),
        (
            Method::PUT,
            format!("/v1/projects/{project}/app/test-accounts"),
        ),
    ] {
        let r = c
            .req(As::User(&owner), method.clone(), &path, Some(manifest()))
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{method} {path}");
    }
    let r = c
        .operator(Method::GET, "/internal/v1/operator/apps", None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // The anonymous paths fall back to the gateway-authenticated /v1 surface.
    for path in [
        "/v1/oauth/metadata",
        "/v1/oauth/userinfo",
        "/v1/app-directory",
        &format!("/v1/app-directory/{project}"),
    ] {
        let r = anon(c, Method::GET, path, &[], "").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    let r = anon(
        c,
        Method::POST,
        "/v1/oauth/token",
        &[FORM],
        "grant_type=refresh_token",
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = anon(c, Method::POST, "/v1/oauth/revoke", &[FORM], "token=x").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(w.oauth.clients.lock().unwrap().is_empty());
    assert_eq!(c.count("SELECT count(*) FROM devplat_apps").await, 0);
}

// ---------------------------------------------------------------------------------------
// ADR 0068 A3: app review from HQ through the admin bridge
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn hq_reviews_and_publishes_an_app_with_two_people_and_no_named_reviewer() {
    let Some(w) = world(all_on()).await else {
        return;
    };
    let c = &w.c;
    let (owner, owner_id) = c.sessions.sign_in("owner@hq-review.example");
    let (_, project, _) = workspace(c, &owner).await;
    let base = format!("/v1/projects/{project}/app");
    let r = c
        .req(As::User(&owner), Method::PUT, &base, Some(manifest()))
        .await;
    assert!(r.status.is_success(), "{}", r.json);
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("{base}/submit"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);

    let hq = ("x-cf-operator", "hq:staff-0001");
    let second = [
        ("x-cf-approver", "hq:staff-0002"),
        ("x-cf-approval-ref", "appr-7"),
        (
            "x-cf-intent-digest",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
    ];
    let case = format!("/internal/v1/operator/verification/cases/cfmeet:APP_REVIEW:{project}");
    let read = c.bridge(&case, &[hq]).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.json);
    assert_eq!(read.json["state"], "OPEN");
    assert_eq!(read.json["evidence"][0]["kind"], "APP_MANIFEST");
    conforms(&format!("{CTRL}VerificationCaseDetail"), &read.json);
    assert!(read.json["details"]["app_version"].as_i64().is_some());

    // The workspace owner's own linked account may not even claim it.
    let key = Uuid::new_v4().to_string();
    let owner_ref = owner_id.to_string();
    let v = read.json["version"].as_i64().unwrap();
    let r = c
        .bridge_send(
            Method::POST,
            &decide(project, "review"),
            &[
                hq,
                ("idempotency-key", &key),
                ("x-cf-operator-user-id", &owner_ref),
            ],
            Some(json!({ "expectedVersion": v })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);

    let mut version = v;
    for (command, two_person, state) in [
        ("review", false, "CLAIMED"),
        ("approve", true, "APPROVED"),
        ("publish", true, "PUBLISHED"),
    ] {
        let key = Uuid::new_v4().to_string();
        let mut headers = vec![hq, ("idempotency-key", key.as_str())];
        if two_person {
            headers.extend(second);
        }
        let r = c
            .bridge_send(
                Method::POST,
                &decide(project, command),
                &headers,
                Some(json!({ "expectedVersion": version })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{command}: {}", r.json);
        let read = c.bridge(&case, &[hq]).await;
        assert_eq!(read.json["state"], state, "{command}");
        let next = read.json["version"].as_i64().unwrap();
        assert!(next > version);
        version = next;
        let rec = c
            .bridge(&format!("/internal/v1/operator/operations/{key}"), &[hq])
            .await;
        assert_eq!(rec.json["source_version"], next.to_string());
    }
    // A two-person decision without its second person is refused.
    let key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &decide(project, "approve"),
            &[hq, ("idempotency-key", &key)],
            Some(json!({ "expectedVersion": version })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
}

// ---------------------------------------------------------------------------------------
// ADR 0068 A4: suspension and its lifting from HQ through the admin bridge
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn hq_suspends_a_published_app_and_lifts_the_suspension_with_two_people() {
    let Some(w) = world(all_on()).await else {
        return;
    };
    let c = &w.c;
    let (owner, _) = c.sessions.sign_in("owner@hq-suspend.example");
    let (_, project, _) = workspace(c, &owner).await;
    publish_app(&w, &owner, project, manifest()).await;
    let hq = ("x-cf-operator", "hq:staff-0001");
    let second = [
        ("x-cf-approver", "hq:staff-0002"),
        ("x-cf-approval-ref", "appr-9"),
        (
            "x-cf-intent-digest",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        ),
    ];
    let v = c
        .count(&format!(
            "SELECT version FROM devplat_apps WHERE app_id = '{project}'"
        ))
        .await;
    // R3: without the second person, refused.
    let key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &decide(project, "suspend"),
            &[hq, ("idempotency-key", &key)],
            Some(json!({ "expectedVersion": v })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    // A stale version.
    let mut headers = vec![hq, ("idempotency-key", key.as_str())];
    headers.extend(second);
    let r = c
        .bridge_send(
            Method::POST,
            &decide(project, "suspend"),
            &headers,
            Some(json!({ "expectedVersion": v - 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    // Suspended: the standard answer, Platform Access told.
    let r = c
        .bridge_send(
            Method::POST,
            &decide(project, "suspend"),
            &headers,
            Some(json!({ "expectedVersion": v, "reasonCode": "POLICY_BREACH" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}AdminCommandResult"), &r.json);
    assert_eq!(r.json["status"], "SUSPENDED");
    assert_eq!(r.json["version"], (v + 1).to_string());
    assert_eq!(r.json["resource"]["appId"], json!(project));
    assert_eq!(
        w.oauth.clients.lock().unwrap().last().unwrap().1["status"],
        "SUSPENDED"
    );
    let replay = c
        .bridge_send(
            Method::POST,
            &decide(project, "suspend"),
            &headers,
            Some(json!({ "expectedVersion": v, "reasonCode": "POLICY_BREACH" })),
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.json);
    assert_eq!(replay.json["status"], "SUSPENDED");
    let rec = c
        .bridge(&format!("/internal/v1/operator/operations/{key}"), &[hq])
        .await;
    assert_eq!(rec.json["source_status"], "SUSPENDED");
    assert_eq!(rec.json["source_version"], (v + 1).to_string());

    // The console never lifts a suspension.
    let r = c
        .operator_as(
            Method::POST,
            &decide(project, "lift-suspension"),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    // Lifting (R3): back to PUBLISHED.
    let lift = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &decide(project, "lift-suspension"),
            &[hq, ("idempotency-key", &lift)],
            Some(json!({ "expectedVersion": v + 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    let mut headers = vec![hq, ("idempotency-key", lift.as_str())];
    headers.extend(second);
    let r = c
        .bridge_send(
            Method::POST,
            &decide(project, "lift-suspension"),
            &headers,
            Some(json!({ "expectedVersion": v + 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}AdminCommandResult"), &r.json);
    assert_eq!(r.json["status"], "PUBLISHED");
    assert_eq!(
        w.oauth.clients.lock().unwrap().last().unwrap().1["status"],
        "PUBLISHED"
    );
    let rec = c
        .bridge(&format!("/internal/v1/operator/operations/{lift}"), &[hq])
        .await;
    assert_eq!(rec.json["source_status"], "PUBLISHED");
    // Not suspended any more: a definite refusal.
    let r = c
        .bridge_send(
            Method::POST,
            &decide(project, "lift-suspension"),
            &[
                hq,
                ("idempotency-key", &Uuid::new_v4().to_string()),
                second[0],
                second[1],
                second[2],
            ],
            Some(json!({ "expectedVersion": v + 2 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
}
