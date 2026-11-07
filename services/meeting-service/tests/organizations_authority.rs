//! Blueprint Section 4 — meeting-service against the Organizations Service.
//!
//! A fake Organizations Service runs on a random local port. It VERIFIES every
//! meeting-service service token (receiver trust bundle built here from the signer's public
//! key; where this build's permission registry does not yet know the `organization.*`
//! permissions, the same Ed25519 signature, issuer, audience, runtime and request-id
//! binding are checked directly), records the headers and bodies it receives and answers
//! with scripted, contract-shaped documents (`docs/api/organizations-internal-v1.md`).
//!
//! Covered: legacy mode unchanged and first-party routes FEATURE_DISABLED; shadow mode
//! answers from the legacy tables and counts an injected mismatch (comparison failures
//! never affect a request); the authoritative directory fails closed (ORGANIZATION_ONLY
//! denied, personal features unaffected); the relay sets the acting user from the verified
//! account and drops client-supplied acting-user headers; the verified-email header only
//! for verified accounts; admin facades require platform administration; the PUT-member
//! facade; the projection lets a space be created for an organization that exists only in
//! Organizations; every first-party route relays (success and error paths).

mod common;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use backend::services::{
    ops_metrics::OPS_METRICS,
    organizations_authority::{
        AuthorityMode, AuthorityStats, OrganizationsAuthority, OrganizationsClient,
        SharedOrganizationsAuthority,
    },
};
use cf_service_auth::{
    InternalPermission, KeyId, PublicJwk, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const ORGANIZATIONS: &str = "cfmeet.organizations";

// ---------------------------------------------------------------------------------------
// Fake Organizations Service
// ---------------------------------------------------------------------------------------

/// Verifies meeting-service's service token as the Organizations receiver would.
enum Verify {
    /// Receiver-authoritative trust bundle (this build knows the organization permissions).
    Policy(ServiceTokenVerifier),
    /// Same checks without the permission registry: Ed25519 signature with the trusted key,
    /// issuer, audience, runtime environment, lifetime and request-id binding.
    Signature(PublicJwk),
}

impl Verify {
    fn for_key(jwk: PublicJwk) -> Self {
        let permissions = [
            "organization.read",
            "organization.write",
            "organization.act-for-user",
            "organization.administer",
        ];
        if permissions
            .iter()
            .all(|p| InternalPermission::parse(p).is_some())
        {
            let bundle = json!({ "bundle_version": 1, "receiver": ORGANIZATIONS, "runtime_environment": "test",
                "trusted_issuers": [{ "issuer": "cfmeet.meeting-service", "audience": ORGANIZATIONS,
                    "runtime_environment": "test", "keys": [jwk], "permissions": permissions }]});
            let organizations = ServiceId::parse(ORGANIZATIONS).unwrap();
            if let Ok(policy) = TrustPolicy::from_json(
                &bundle.to_string(),
                &organizations,
                RuntimeEnvironment::Test,
            ) {
                return Self::Policy(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)));
            }
        }
        Self::Signature(jwk)
    }

    fn verify(&self, headers: &HeaderMap) -> bool {
        match self {
            Self::Policy(v) => v
                .verify_headers(headers)
                .is_ok_and(|ctx| ctx.authenticated_service().as_str() == "cfmeet.meeting-service"),
            Self::Signature(jwk) => {
                let header = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
                let Some(token) =
                    header("x-cf-service-authorization").and_then(|v| v.strip_prefix("Bearer "))
                else {
                    return false;
                };
                if headers.get_all("x-cf-service-authorization").iter().count() != 1 {
                    return false;
                }
                let Ok(key) = jsonwebtoken::DecodingKey::from_ed_components(&jwk.x) else {
                    return false;
                };
                let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
                validation.set_audience(&[ORGANIZATIONS]);
                validation.set_issuer(&["cfmeet.meeting-service"]);
                validation.set_required_spec_claims(&["exp", "nbf", "iat", "iss", "aud"]);
                let Ok(data) = jsonwebtoken::decode::<Value>(token, &key, &validation) else {
                    return false;
                };
                let claims = data.claims;
                data.header.kid.as_deref() == Some(jwk.kid.as_str())
                    && claims["runtime_environment"] == "test"
                    && claims["request_id"].as_str() == header("x-cf-request-id")
                    && claims["correlation_id"].as_str() == header("x-cf-correlation-id")
                    && claims["exp"].as_i64().unwrap_or(0) - claims["iat"].as_i64().unwrap_or(0)
                        <= 300
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    query: Option<String>,
    acting_user: Option<String>,
    verified_email: Option<String>,
    idempotency_key: Option<String>,
    body: Value,
}

struct FakeState {
    verify: Verify,
    requests: Mutex<Vec<Recorded>>,
    rejected: Mutex<u32>,
    /// user → resolve memberships (contract shape).
    memberships: Mutex<HashMap<Uuid, Vec<Value>>>,
    /// organization → document.
    orgs: Mutex<HashMap<Uuid, Value>>,
    /// organization → membership documents.
    members: Mutex<HashMap<Uuid, Vec<Value>>>,
    /// (METHOD, path) → scripted answer.
    overrides: Mutex<HashMap<(String, String), (u16, Value)>>,
    down: AtomicBool,
}

struct Fake {
    addr: SocketAddr,
    state: Arc<FakeState>,
}

fn envelope(code: &str) -> Value {
    json!({ "schemaVersion": 1, "error": { "code": code, "message": format!("{code} (fake)"), "requestId": "fake" } })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn org_doc(id: Uuid, name: &str, slug: &str, status: &str, source: &str) -> Value {
    json!({ "id": id, "name": name, "slug": slug, "lifecycleStatus": status, "provisioningState": "PENDING",
            "creationSource": source, "createdBy": null, "createdAt": "2026-10-04T12:00:00Z",
            "updatedAt": now(), "version": 1, "archivedAt": null, "profile": null, "memberCount": 1 })
}

fn member_doc(org: Uuid, user: Uuid, role: &str) -> Value {
    json!({ "id": Uuid::new_v4(), "organizationId": org, "userId": user, "role": role, "status": "ACTIVE",
            "joinedVia": "IMPORTED", "createdBy": null, "createdAt": "2026-10-04T12:00:00Z",
            "updatedAt": "2026-10-04T12:00:00Z", "removedAt": null, "version": 1 })
}

fn segments(path: &str) -> Vec<&str> {
    path.trim_start_matches("/internal/v1/")
        .split('/')
        .collect()
}

fn id(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

async fn fake_handler(
    State(state): State<Arc<FakeState>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if state.down.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(envelope("SERVICE_UNAVAILABLE")),
        )
            .into_response();
    }
    if !state.verify.verify(&headers) {
        *state.rejected.lock().unwrap() += 1;
        return (
            StatusCode::UNAUTHORIZED,
            Json(envelope("INTERNAL_AUTHENTICATION_FAILED")),
        )
            .into_response();
    }
    let header = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = uri.path().to_string();
    state.requests.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        query: uri.query().map(str::to_string),
        acting_user: header("x-cf-acting-user"),
        verified_email: header("x-cf-acting-user-verified-email"),
        idempotency_key: header("idempotency-key"),
        body: body.clone(),
    });
    if let Some((status, doc)) = state
        .overrides
        .lock()
        .unwrap()
        .get(&(method.to_string(), path.clone()))
        .cloned()
    {
        return (StatusCode::from_u16(status).unwrap(), Json(doc)).into_response();
    }
    let ok = |doc: Value| (StatusCode::OK, Json(doc)).into_response();
    let created = |doc: Value| (StatusCode::CREATED, Json(doc)).into_response();
    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(envelope("ORGANIZATION_NOT_FOUND")),
        )
            .into_response()
    };
    let seg = segments(&path);
    match (method.as_str(), seg.as_slice()) {
        ("POST", ["organizations", "memberships", "resolve"]) => {
            let user = id(body["userId"].as_str().unwrap());
            let memberships = state
                .memberships
                .lock()
                .unwrap()
                .get(&user)
                .cloned()
                .unwrap_or_default();
            ok(json!({ "userId": user, "memberships": memberships }))
        }
        ("POST", ["organizations"]) => {
            let org = Uuid::new_v4();
            let doc = org_doc(
                org,
                body["name"].as_str().unwrap_or("n"),
                body["slug"].as_str().unwrap_or("s"),
                "ACTIVE",
                "SELF_SERVICE",
            );
            state.orgs.lock().unwrap().insert(org, doc.clone());
            created(doc)
        }
        ("GET", ["organizations", o]) => match state.orgs.lock().unwrap().get(&id(o)) {
            Some(doc) => ok(doc.clone()),
            None => not_found(),
        },
        ("PATCH", ["organizations", o]) => match state.orgs.lock().unwrap().get_mut(&id(o)) {
            Some(doc) => {
                if let Some(n) = body["name"].as_str() {
                    doc["name"] = json!(n);
                }
                doc["version"] = json!(doc["version"].as_i64().unwrap() + 1);
                ok(doc.clone())
            }
            None => not_found(),
        },
        ("GET", ["organizations", o, "members"]) => {
            let items = state.members.lock().unwrap().get(&id(o)).cloned();
            match items {
                Some(items) => ok(json!({ "items": items, "nextCursor": null })),
                None => not_found(),
            }
        }
        ("PATCH", ["organizations", o, "members", m]) => {
            let mut members = state.members.lock().unwrap();
            let Some(item) = members
                .get_mut(&id(o))
                .and_then(|l| l.iter_mut().find(|x| x["id"] == json!(id(m))))
            else {
                return not_found();
            };
            if body["expectedVersion"] != item["version"] {
                return (StatusCode::CONFLICT, Json(envelope("VERSION_CONFLICT"))).into_response();
            }
            if let Some(r) = body["role"].as_str() {
                item["role"] = json!(r);
            }
            if let Some(s) = body["status"].as_str() {
                item["status"] = json!(s);
            }
            item["version"] = json!(item["version"].as_i64().unwrap() + 1);
            ok(item.clone())
        }
        ("POST", ["organizations", o, "members", "me", "leave"]) => {
            let mut doc = member_doc(id(o), Uuid::nil(), "MEMBER");
            doc["status"] = json!("REMOVED");
            ok(doc)
        }
        ("POST", ["organizations", o, "ownership-transfer"]) => {
            ok(json!({ "organizationId": id(o), "newOwner": body["toMembershipId"] }))
        }
        ("POST", ["organizations", o, "invitations"]) => created(json!({
            "id": Uuid::new_v4(), "organizationId": id(o), "role": body["role"], "status": "PENDING",
            "inviteeEmailHint": "i***@example.com", "invitedBy": null, "createdAt": now(),
            "expiresAt": now(), "decidedAt": null, "token": "orginv_fake_token" })),
        ("GET", ["organizations", _, "invitations"]) => {
            ok(json!({ "items": [], "nextCursor": null }))
        }
        ("POST", ["organizations", o, "invitations", i, "revoke"]) => ok(json!({
            "id": id(i), "organizationId": id(o), "role": "MEMBER", "status": "REVOKED",
            "inviteeEmailHint": "i***@example.com", "invitedBy": null, "createdAt": now(),
            "expiresAt": now(), "decidedAt": now() })),
        ("POST", ["organization-invitations", "accept"]) => {
            ok(json!({ "membership": member_doc(Uuid::new_v4(), Uuid::new_v4(), "MEMBER") }))
        }
        ("POST", ["organization-invitations", "decline"]) => ok(json!({ "status": "DECLINED" })),
        ("POST", ["organizations", o, action @ ("suspend" | "reactivate" | "archive")]) => {
            let status = match *action {
                "suspend" => "SUSPENDED",
                "reactivate" => "ACTIVE",
                _ => "ARCHIVED",
            };
            match state.orgs.lock().unwrap().get_mut(&id(o)) {
                Some(doc) => {
                    doc["lifecycleStatus"] = json!(status);
                    ok(doc.clone())
                }
                None => not_found(),
            }
        }
        ("GET", ["organizations", _, "audit-events"]) => {
            ok(json!({ "items": [], "nextCursor": null }))
        }
        ("GET", ["organizations", o, "provisioning"]) => ok(json!({
            "organizationId": id(o), "provisioningState": "PENDING", "step": null, "failureClass": null })),
        ("GET", ["users", u, "organizations"]) => {
            let user = id(u);
            let memberships = state
                .memberships
                .lock()
                .unwrap()
                .get(&user)
                .cloned()
                .unwrap_or_default();
            let orgs = state.orgs.lock().unwrap();
            let items: Vec<Value> = memberships
                .iter()
                .filter_map(|m| {
                    let org = orgs.get(&id(m["organizationId"].as_str().unwrap()))?;
                    Some(json!({
                        "organization": { "id": org["id"], "name": org["name"], "slug": org["slug"],
                                          "lifecycleStatus": org["lifecycleStatus"], "provisioningState": "PENDING", "version": 1 },
                        "membership": { "id": m["membershipId"], "organizationId": org["id"], "userId": user,
                                        "role": m["role"], "status": m["status"], "version": 1 },
                        "capabilities": m["capabilities"],
                    }))
                })
                .collect();
            ok(json!({ "userId": user, "items": items }))
        }
        ("GET", ["admin", "organizations"]) => {
            let items: Vec<Value> = state.orgs.lock().unwrap().values().cloned().collect();
            ok(json!({ "items": items, "nextCursor": null }))
        }
        ("POST", ["admin", "organizations"]) => {
            let org = Uuid::new_v4();
            let mut doc = org_doc(
                org,
                body["name"].as_str().unwrap(),
                body["slug"].as_str().unwrap(),
                "ACTIVE",
                "PLATFORM_ADMIN",
            );
            doc["createdBy"] = json!(header("x-cf-acting-user"));
            state.orgs.lock().unwrap().insert(org, doc.clone());
            created(doc)
        }
        ("PATCH", ["admin", "organizations", o]) => {
            match state.orgs.lock().unwrap().get_mut(&id(o)) {
                Some(doc) => {
                    doc["lifecycleStatus"] = body["status"].clone();
                    ok(doc.clone())
                }
                None => not_found(),
            }
        }
        _ => (StatusCode::NOT_FOUND, Json(envelope("NOT_FOUND"))).into_response(),
    }
}

impl Fake {
    async fn start(jwk: PublicJwk) -> Self {
        let state = Arc::new(FakeState {
            verify: Verify::for_key(jwk),
            requests: Mutex::new(vec![]),
            rejected: Mutex::new(0),
            memberships: Mutex::new(HashMap::new()),
            orgs: Mutex::new(HashMap::new()),
            members: Mutex::new(HashMap::new()),
            overrides: Mutex::new(HashMap::new()),
            down: AtomicBool::new(false),
        });
        let router = Router::new()
            .fallback(fake_handler)
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { addr, state }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> Vec<Recorded> {
        self.state.requests.lock().unwrap().clone()
    }

    /// Requests other than the membership resolution every authenticated request makes
    /// in authoritative mode (the directory read of the account authority).
    fn relayed(&self) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.path != "/internal/v1/organizations/memberships/resolve")
            .count()
    }

    fn last(&self, method: &str, path: &str) -> Recorded {
        self.requests()
            .into_iter()
            .rev()
            .find(|r| r.method == method && r.path == path)
            .unwrap_or_else(|| panic!("no {method} {path} reached Organizations"))
    }

    /// An organization known only to Organizations, with `members` (user, role).
    fn add_org(&self, slug: &str, members: &[(Uuid, &str)]) -> Uuid {
        let org = Uuid::new_v4();
        self.state.orgs.lock().unwrap().insert(
            org,
            org_doc(
                org,
                &format!("{slug} Health"),
                slug,
                "ACTIVE",
                "SELF_SERVICE",
            ),
        );
        let mut docs = vec![];
        for (user, role) in members {
            let doc = member_doc(org, *user, role);
            self.state
                .memberships
                .lock()
                .unwrap()
                .entry(*user)
                .or_default()
                .push(json!({ "membershipId": doc["id"], "organizationId": org, "role": role,
                              "status": "ACTIVE", "capabilities": [], "organizationStatus": "ACTIVE" }));
            docs.push(doc);
        }
        self.state.members.lock().unwrap().insert(org, docs);
        org
    }

    fn set_memberships(&self, user: Uuid, memberships: Vec<Value>) {
        self.state
            .memberships
            .lock()
            .unwrap()
            .insert(user, memberships);
    }

    fn script(&self, method: &str, path: &str, status: u16, doc: Value) {
        self.state
            .overrides
            .lock()
            .unwrap()
            .insert((method.to_string(), path.to_string()), (status, doc));
    }
}

/// meeting-service's signer and a fake Organizations Service trusting its key.
async fn organizations() -> (Arc<ServiceTokenSigner>, Fake) {
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
        ServiceSigningKey::generate(KeyId::parse("meeting-service-s4-test-1").unwrap()),
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let fake = Fake::start(signer.public_jwk()).await;
    (signer, fake)
}

fn authority(
    signer: &Arc<ServiceTokenSigner>,
    url: &str,
    mode: AuthorityMode,
) -> SharedOrganizationsAuthority {
    Arc::new(OrganizationsAuthority::new(
        mode,
        OrganizationsClient::new(signer.clone(), url, Duration::from_secs(2)).unwrap(),
    ))
}

async fn app_with(authority: Option<SharedOrganizationsAuthority>) -> Option<TestApp> {
    TestApp::with(TestOptions {
        organizations_authority: authority,
        ..TestOptions::default()
    })
    .await
}

macro_rules! some {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => return,
        }
    };
}

async fn wait_for(counter: &std::sync::atomic::AtomicU64, at_least: u64) -> u64 {
    for _ in 0..100 {
        let v = AuthorityStats::get(counter);
        if v >= at_least {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    AuthorityStats::get(counter)
}

/// Every first-party route: (method, path template, body). `{o}`, `{m}`, `{i}` are filled.
fn first_party_routes(o: Uuid, m: Uuid, i: Uuid) -> Vec<(Method, String, Option<Value>)> {
    vec![
        (
            Method::POST,
            "/api/v2/organizations".into(),
            Some(json!({ "name": "Globex", "slug": "globex" })),
        ),
        (Method::GET, format!("/api/v2/organizations/{o}"), None),
        (
            Method::PATCH,
            format!("/api/v2/organizations/{o}"),
            Some(json!({ "expectedVersion": 1, "name": "Acme Renamed" })),
        ),
        (
            Method::PATCH,
            format!("/api/v2/organizations/{o}/members/{m}"),
            Some(json!({ "expectedVersion": 1, "role": "MANAGER" })),
        ),
        (
            Method::POST,
            format!("/api/v2/organizations/{o}/members/me/leave"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v2/organizations/{o}/ownership-transfer"),
            Some(json!({ "toMembershipId": m, "expectedVersion": 1 })),
        ),
        (
            Method::POST,
            format!("/api/v2/organizations/{o}/invitations"),
            Some(json!({ "email": "invitee@example.com", "role": "MEMBER" })),
        ),
        (
            Method::GET,
            format!("/api/v2/organizations/{o}/invitations?status=PENDING"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v2/organizations/{o}/invitations/{i}/revoke"),
            None,
        ),
        (
            Method::POST,
            "/api/v2/organization-invitations/accept".into(),
            Some(json!({ "token": "orginv_fake_token" })),
        ),
        (
            Method::POST,
            "/api/v2/organization-invitations/decline".into(),
            Some(json!({ "token": "orginv_fake_token" })),
        ),
        (
            Method::POST,
            format!("/api/v2/organizations/{o}/suspend"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("/api/v2/organizations/{o}/reactivate"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("/api/v2/organizations/{o}/archive"),
            Some(json!({})),
        ),
        (
            Method::GET,
            format!("/api/v2/organizations/{o}/audit-events?limit=10"),
            None,
        ),
        (
            Method::GET,
            format!("/api/v2/organizations/{o}/provisioning"),
            None,
        ),
        (
            Method::GET,
            "/api/v2/admin/organizations?limit=10".into(),
            None,
        ),
    ]
}

async fn create_legacy_org(app: &TestApp, admin: &User, slug: &str, owner: &User) -> Uuid {
    let (s, v) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&admin.token),
            json!({ "name": slug, "slug": slug, "ownerUserId": owner.id }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    uuid(&v["id"])
}

// ---------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------

/// The client signs every call; the fake receiver accepts meeting-service's key and
/// refuses any other (no database needed).
#[tokio::test]
async fn s4_client_calls_are_signed_and_verified() {
    let (signer, fake) = organizations().await;
    let user = Uuid::new_v4();
    let org = fake.add_org("signed-acme", &[(user, "MANAGER")]);
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let resolved = live.resolve_memberships(user).await.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].organization_id, org);
    assert!(resolved[0].grants_meeting_membership());
    let snapshot = live.organization_snapshot(org).await.unwrap().unwrap();
    assert_eq!(snapshot.slug, "signed-acme");
    assert!(live
        .organization_snapshot(Uuid::new_v4())
        .await
        .unwrap()
        .is_none());
    assert_eq!(*fake.state.rejected.lock().unwrap(), 0);
    let r = fake.last("POST", "/internal/v1/organizations/memberships/resolve");
    assert_eq!(
        r.acting_user, None,
        "membership resolution is a service read"
    );
    assert_eq!(r.body, json!({ "userId": user }));

    // A token from any other key is refused by the receiver and grants nothing.
    let impostor = Arc::new(ServiceTokenSigner::new(
        ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
        ServiceSigningKey::generate(KeyId::parse("impostor-1").unwrap()),
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let forged = authority(&impostor, &fake.url(), AuthorityMode::Authoritative);
    assert!(forged.resolve_memberships(user).await.is_err());
    assert_eq!(*fake.state.rejected.lock().unwrap(), 1);
}

#[tokio::test]
async fn s4_legacy_mode_is_unchanged_and_first_party_routes_are_disabled() {
    let app = some!(app_with(None).await);
    let admin = app.admin("platform").await;
    let owner = app.user("owner").await;
    let org = create_legacy_org(&app, &admin, "legacy-acme", &owner).await;
    let (s, mine) = app
        .get("/api/v2/me/organizations", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(uuid(&mine[0]["organization"]["id"]), org);
    assert_eq!(mine[0]["role"], "OWNER");
    assert!(
        mine[0].get("organizationRole").is_none(),
        "legacy shape: {mine}"
    );
    for (method, uri, body) in first_party_routes(org, Uuid::new_v4(), Uuid::new_v4()) {
        let (s, v) = app
            .call(method.clone(), &uri, Auth::Bearer(&admin.token), body)
            .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::NOT_FOUND, "FEATURE_DISABLED"),
            "{method} {uri}: {v}"
        );
    }
}

#[tokio::test]
async fn s4_shadow_mode_answers_from_legacy_and_counts_mismatches() {
    let (signer, fake) = organizations().await;
    let shadow = authority(&signer, &fake.url(), AuthorityMode::Shadow);
    let app = some!(app_with(Some(shadow.clone())).await);
    let admin = app.admin("platform").await;
    let owner = app.user("owner").await;
    let insider = app.user("insider").await;
    // meeting-service is still the only writer: the legacy routes write locally.
    let org = create_legacy_org(&app, &admin, "shadow-acme", &owner).await;
    // Organizations agrees about the owner and disagrees about the insider (role).
    fake.set_memberships(
        owner.id,
        vec![
            json!({ "membershipId": Uuid::new_v4(), "organizationId": org, "role": "OWNER",
                     "status": "ACTIVE", "capabilities": [], "organizationStatus": "ACTIVE" }),
        ],
    );
    fake.set_memberships(
        insider.id,
        vec![
            json!({ "membershipId": Uuid::new_v4(), "organizationId": org, "role": "ADMIN",
                     "status": "ACTIVE", "capabilities": [], "organizationStatus": "ACTIVE" }),
        ],
    );
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/organizations/{org}/members/{}", insider.id),
            Auth::Bearer(&owner.token),
            Some(json!({ "role": "MEMBER", "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let mismatches_before = AuthorityStats::get(&shadow.stats.shadow_mismatches);
    let global_before = OPS_METRICS
        .organizations_shadow_mismatches
        .load(Ordering::Relaxed);
    let (s, mine) = app
        .get("/api/v2/me/organizations", Auth::Bearer(&insider.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(uuid(&mine[0]["organization"]["id"]), org);
    assert_eq!(mine[0]["role"], "MEMBER", "the legacy answer is served");
    assert!(
        wait_for(&shadow.stats.shadow_mismatches, mismatches_before + 1).await > mismatches_before,
        "the injected mismatch is counted"
    );
    assert!(
        OPS_METRICS
            .organizations_shadow_mismatches
            .load(Ordering::Relaxed)
            > global_before,
        "exposed on the internal metrics"
    );
    assert!(AuthorityStats::get(&shadow.stats.shadow_comparisons) >= 1);

    // ORGANIZATION_ONLY still decided by the legacy tables.
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "org", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": org }),
        )
        .await;
    let (s, v) = app.join(cid, Auth::Bearer(&insider.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // A failing comparison never affects the request.
    fake.state.down.store(true, Ordering::SeqCst);
    let failures_before = AuthorityStats::get(&shadow.stats.shadow_failures);
    let (s, mine) = app
        .get("/api/v2/me/organizations", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(uuid(&mine[0]["organization"]["id"]), org);
    assert!(wait_for(&shadow.stats.shadow_failures, failures_before + 1).await > failures_before);

    // Shadow mode only ever reads: no write, and no first-party route, reached Organizations.
    assert!(fake
        .requests()
        .iter()
        .all(|r| r.method == "POST" && r.path == "/internal/v1/organizations/memberships/resolve"));
    assert_eq!(
        *fake.state.rejected.lock().unwrap(),
        0,
        "every token verified"
    );
    let (s, v) = app
        .post(
            "/api/v2/organizations",
            Auth::Bearer(&owner.token),
            json!({ "name": "x", "slug": "xyz" }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "FEATURE_DISABLED"));
}

#[tokio::test]
async fn s4_authoritative_directory_fails_closed() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live.clone())).await);
    let owner = app.user("owner").await;
    let insider = app.user("insider").await;
    let late = app.user("late").await;
    let guest = app.user("guest").await;
    // The organization exists only in Organizations.
    let org = fake.add_org(
        "only-in-orgs",
        &[
            (owner.id, "OWNER"),
            (insider.id, "MANAGER"),
            (late.id, "MEMBER"),
            (guest.id, "GUEST"),
        ],
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM organizations WHERE id = '{org}'"
        ))
        .await,
        0
    );

    // Projection: the space can be created; the local row is the authoritative snapshot.
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "org", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": org }),
        )
        .await;
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM organizations WHERE id = '{org}' AND slug = 'only-in-orgs' AND status = 'ACTIVE'"
        ))
        .await,
        1
    );
    assert!(AuthorityStats::get(&live.stats.projection_upserts) >= 1);
    let snapshot_read = fake.last("GET", &format!("/internal/v1/organizations/{org}"));
    assert_eq!(
        snapshot_read.acting_user, None,
        "snapshot is a service read"
    );

    // MANAGER counts as a member; GUEST does not.
    let (s, v) = app.join(cid, Auth::Bearer(&insider.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app.join(cid, Auth::Bearer(&guest.token), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED"),
        "{v}"
    );

    // Organizations unavailable → no organization membership at all.
    fake.state.down.store(true, Ordering::SeqCst);
    let failures = AuthorityStats::get(&live.stats.authority_failures);
    let (s, v) = app.join(cid, Auth::Bearer(&late.token), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED"),
        "fails closed: {v}"
    );
    assert!(AuthorityStats::get(&live.stats.authority_failures) > failures);
    // Organization spaces cannot be created; personal features are unaffected.
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&owner.token),
            json!({ "title": "org2", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": org }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        "{v}"
    );
    let personal = app
        .create_space(&late, json!({ "title": "personal" }))
        .await;
    assert!(personal["id"].is_string());
    let (s, v) = app
        .get("/api/v2/me/organizations", Auth::Bearer(&late.token))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE")
    );

    // Truly down (connection refused): same answer.
    let dead = authority(&signer, "http://127.0.0.1:1", AuthorityMode::Authoritative);
    let app = app.reconfigure(TestOptions {
        organizations_authority: Some(dead.clone()),
        ..TestOptions::default()
    });
    let (s, v) = app.join(cid, Auth::Bearer(&late.token), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED"),
        "{v}"
    );
    assert!(AuthorityStats::get(&dead.stats.authority_failures) >= 1);
    let (s, v) = app
        .get(
            format!("/api/v2/organizations/{org}"),
            Auth::Bearer(&late.token),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE")
    );
    let personal = app
        .create_space(&late, json!({ "title": "still personal" }))
        .await;
    assert!(personal["id"].is_string());
}

#[tokio::test]
async fn s4_relay_acts_only_for_the_verified_account() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let user = app.user("verified").await;
    let victim = app.user("victim").await;
    let org = fake.add_org("relay-acme", &[(user.id, "OWNER"), (victim.id, "MEMBER")]);

    let bearer = format!("Bearer {}", user.token);
    let key = Uuid::new_v4().to_string();
    let (s, v) = app
        .call_with_headers(
            Method::POST,
            &format!("/api/v2/organizations/{org}/invitations"),
            &[
                ("authorization", &bearer),
                ("x-cf-acting-user", &victim.id.to_string()),
                ("x-cf-acting-user-verified-email", "victim@example.com"),
                ("idempotency-key", &key),
            ],
            json!({ "email": "Invitee@Example.com", "role": "MEMBER" }).to_string(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["token"], "orginv_fake_token");
    let r = fake.last(
        "POST",
        &format!("/internal/v1/organizations/{org}/invitations"),
    );
    assert_eq!(
        r.acting_user,
        Some(user.id.to_string()),
        "actor from the token"
    );
    assert_eq!(
        r.verified_email,
        Some(user.email.to_ascii_lowercase()),
        "verified email from the account, never the client header"
    );
    assert_eq!(r.idempotency_key, Some(key));
    assert_eq!(
        r.body["email"], "Invitee@Example.com",
        "body relayed unchanged"
    );

    // An account without a verified email: no verified-email header at all.
    let unverified = app.user("unverified").await;
    sea_orm::ConnectionTrait::execute_unprepared(
        &app.db.conn,
        &format!(
            "UPDATE users SET status = 'pending_verification' WHERE id = '{}'",
            unverified.id
        ),
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", unverified.token);
    let (s, v) = app
        .call_with_headers(
            Method::POST,
            "/api/v2/organization-invitations/accept",
            &[
                ("authorization", &bearer),
                ("x-cf-acting-user-verified-email", "invitee@example.com"),
            ],
            json!({ "token": "orginv_fake_token" }).to_string(),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let r = fake.last("POST", "/internal/v1/organization-invitations/accept");
    assert_eq!(r.acting_user, Some(unverified.id.to_string()));
    assert_eq!(
        r.verified_email, None,
        "unverified account: no verified email"
    );

    // No credential → nothing reaches Organizations.
    let before = fake.relayed();
    let (s, v) = app
        .call_with_headers(
            Method::GET,
            &format!("/api/v2/organizations/{org}"),
            &[("x-cf-acting-user", &user.id.to_string())],
            String::new(),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
    );
    assert_eq!(fake.relayed(), before);
    // Unknown query parameters are refused before relaying.
    let (s, v) = app
        .get(
            format!(
                "/api/v2/organizations/{org}/invitations?userId={}",
                victim.id
            ),
            Auth::Bearer(&user.token),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    assert_eq!(fake.relayed(), before);
    // Every relayed request carried a token Organizations verified.
    assert_eq!(*fake.state.rejected.lock().unwrap(), 0);
    assert!(fake
        .requests()
        .iter()
        .all(|r| r.path.starts_with("/internal/v1/")));
}

#[tokio::test]
async fn s4_admin_facades_require_platform_admin() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let admin = app.admin("platform").await;
    let user = app.user("user").await;
    let owner = app.user("owner").await;

    let body = json!({ "name": "Facade Health", "slug": "facade-health", "ownerUserId": owner.id });
    let (s, v) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&user.token),
            body.clone(),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    let (s, v) = app
        .get("/api/v2/admin/organizations", Auth::Bearer(&user.token))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    assert!(
        fake.requests().iter().all(|r| !r.path.contains("/admin/")),
        "refused before relaying"
    );

    let (s, created) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&admin.token),
            body,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let org = uuid(&created["id"]);
    assert_eq!(created["slug"], "facade-health");
    assert_eq!(created["status"], "ACTIVE");
    assert!(
        created["createdAt"].is_string(),
        "legacy shape kept: {created}"
    );
    let r = fake.last("POST", "/internal/v1/admin/organizations");
    assert_eq!(r.acting_user, Some(admin.id.to_string()));
    assert_eq!(r.body["ownerUserId"], json!(owner.id));
    // meeting-service did not write a second copy of the organization: only the projection.
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM organizations WHERE id = '{org}'"
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM organization_memberships WHERE organization_id = '{org}'"
        ))
        .await,
        0,
        "memberships are never written locally"
    );

    let (s, v) = app
        .patch(
            format!("/api/v2/admin/organizations/{org}"),
            Auth::Bearer(&user.token),
            json!({ "status": "SUSPENDED" }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    let (s, v) = app
        .patch(
            format!("/api/v2/admin/organizations/{org}"),
            Auth::Bearer(&admin.token),
            json!({ "status": "SUSPENDED" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "SUSPENDED");
    assert_eq!(
        fake.last("PATCH", &format!("/internal/v1/admin/organizations/{org}"))
            .body,
        json!({ "status": "SUSPENDED" })
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM organizations WHERE id = '{org}' AND status = 'SUSPENDED'"
        ))
        .await,
        1,
        "projection follows the authority"
    );

    let (s, v) = app
        .get(
            "/api/v2/admin/organizations?status=SUSPENDED",
            Auth::Bearer(&admin.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["id"] == json!(org)));
    assert_eq!(
        fake.last("GET", "/internal/v1/admin/organizations")
            .query
            .as_deref(),
        Some("status=SUSPENDED")
    );
    // A relayed refusal keeps its contract code.
    fake.script(
        "POST",
        "/internal/v1/admin/organizations",
        409,
        envelope("SLUG_TAKEN"),
    );
    let (s, v) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&admin.token),
            json!({ "name": "Dup", "slug": "facade-health" }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "SLUG_TAKEN"));
}

#[tokio::test]
async fn s4_member_facades_relay_existing_members_and_require_invitations() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let manager = app.user("manager").await;
    let stranger = app.user("stranger").await;
    let org = fake.add_org(
        "members-acme",
        &[
            (owner.id, "OWNER"),
            (member.id, "MEMBER"),
            (manager.id, "MANAGER"),
        ],
    );

    // Existing member → PATCH of that membership with its version.
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/organizations/{org}/members/{}", member.id),
            Auth::Bearer(&owner.token),
            Some(json!({ "role": "ADMIN", "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["role"].as_str(), v["organizationRole"].as_str()),
        (Some("ADMIN"), Some("ADMIN"))
    );
    assert_eq!(uuid(&v["userId"]), member.id);
    let patch = fake
        .requests()
        .into_iter()
        .rev()
        .find(|r| r.method == "PATCH")
        .expect("membership PATCH relayed");
    assert!(patch
        .path
        .starts_with(&format!("/internal/v1/organizations/{org}/members/")));
    assert_eq!(patch.body, json!({ "expectedVersion": 1, "role": "ADMIN" }));
    assert_eq!(patch.acting_user, Some(owner.id.to_string()));

    // A MANAGER re-sent as MEMBER keeps the role: nothing to change, nothing relayed.
    let patches = fake
        .requests()
        .iter()
        .filter(|r| r.method == "PATCH")
        .count();
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/organizations/{org}/members/{}", manager.id),
            Auth::Bearer(&owner.token),
            Some(json!({ "role": "MEMBER", "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["role"].as_str(), v["organizationRole"].as_str()),
        (Some("MEMBER"), Some("MANAGER"))
    );
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|r| r.method == "PATCH")
            .count(),
        patches
    );

    // A user who is not a member must be invited.
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/organizations/{org}/members/{}", stranger.id),
            Auth::Bearer(&owner.token),
            Some(json!({ "role": "MEMBER", "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "INVITATION_REQUIRED"),
        "{v}"
    );
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|r| r.method == "PATCH")
            .count(),
        patches
    );

    // Legacy reads keep their fields (additive organizationRole / version).
    let (s, list) = app
        .get(
            format!("/api/v2/organizations/{org}/members"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 3);
    for item in list {
        for field in [
            "id",
            "organizationId",
            "userId",
            "role",
            "status",
            "createdAt",
            "updatedAt",
        ] {
            assert!(item.get(field).is_some(), "{field} missing: {item}");
        }
    }
    assert_eq!(
        fake.last("GET", &format!("/internal/v1/organizations/{org}/members"))
            .acting_user,
        Some(owner.id.to_string())
    );
    let (s, mine) = app
        .get("/api/v2/me/organizations", Auth::Bearer(&manager.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert_eq!(uuid(&mine[0]["organization"]["id"]), org);
    assert_eq!(mine[0]["organization"]["name"], "members-acme Health");
    assert_eq!(mine[0]["organization"]["status"], "ACTIVE");
    assert!(mine[0]["organization"]["createdAt"].is_string());
    assert_eq!(
        (
            mine[0]["role"].as_str(),
            mine[0]["organizationRole"].as_str()
        ),
        (Some("MEMBER"), Some("MANAGER"))
    );
    // Non-members are refused by Organizations (non-enumerating).
    let (s, v) = app
        .get(
            format!("/api/v2/organizations/{}/members", Uuid::new_v4()),
            Auth::Bearer(&stranger.token),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "ORGANIZATION_NOT_FOUND")
    );
}

#[tokio::test]
async fn s4_every_first_party_route_relays_in_authoritative_mode() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let admin = app.admin("platform").await;
    let org = fake.add_org("routes-acme", &[(admin.id, "OWNER")]);
    let membership = fake.state.members.lock().unwrap()[&org][0]["id"]
        .as_str()
        .map(id)
        .unwrap();
    let invitation = Uuid::new_v4();
    for (method, uri, body) in first_party_routes(org, membership, invitation) {
        let (s, v) = app
            .call(
                method.clone(),
                &uri,
                Auth::Bearer(&admin.token),
                body.clone(),
            )
            .await;
        assert!(s.is_success(), "{method} {uri}: {s} {v}");
        let internal = uri
            .split('?')
            .next()
            .unwrap()
            .replacen("/api/v2/", "/internal/v1/", 1);
        let r = fake.last(method.as_str(), &internal);
        assert_eq!(r.acting_user, Some(admin.id.to_string()), "{uri}");
        if let Some(b) = body {
            assert_eq!(r.body, b, "{uri}");
        }
    }
    // Projection refreshed from relayed organization documents.
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM organizations WHERE id = '{org}' AND name = 'Acme Renamed'"
        ))
        .await,
        1
    );

    // Error paths: contract refusals keep their code; internal-auth and server failures
    // are SERVICE_UNAVAILABLE.
    let leave = format!("/internal/v1/organizations/{org}/members/me/leave");
    fake.script("POST", &leave, 409, envelope("LAST_OWNER"));
    let (s, v) = app
        .post(
            format!("/api/v2/organizations/{org}/members/me/leave"),
            Auth::Bearer(&admin.token),
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "LAST_OWNER"));
    fake.script("POST", &leave, 403, envelope("ACTOR_NOT_ALLOWED"));
    let (s, v) = app
        .post(
            format!("/api/v2/organizations/{org}/members/me/leave"),
            Auth::Bearer(&admin.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE")
    );
    fake.script("POST", &leave, 500, envelope("INTERNAL_ERROR"));
    let (s, v) = app
        .post(
            format!("/api/v2/organizations/{org}/members/me/leave"),
            Auth::Bearer(&admin.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE")
    );
    let (s, v) = app
        .post_raw("/api/v2/organizations", Auth::Bearer(&admin.token), "[1,2]")
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    let (s, v) = app
        .get(
            format!("/api/v2/organizations/{}", Uuid::new_v4()),
            Auth::Bearer(&admin.token),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "ORGANIZATION_NOT_FOUND")
    );
    assert_eq!(*fake.state.rejected.lock().unwrap(), 0);
}
