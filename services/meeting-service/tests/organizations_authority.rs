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
    organization_calling::OrganizationCalling,
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
    /// organization → fake policy `{revision, features:{meetings,messaging,calling},
    /// restrict:{ROLE:[capability]}}` (Section 8 capability reads).
    policies: Mutex<HashMap<Uuid, Value>>,
    down: AtomicBool,
}

/// The contract role → capability matrix (`docs/api/organizations-internal-v1.md` §3).
const CAPABILITIES: &[&str] = &[
    "organization.read",
    "organization.members.read",
    "organization.profile.write",
    "organization.members.invite",
    "organization.members.manage",
    "organization.ownership.transfer",
    "organization.lifecycle.manage",
    "organization.audit.read",
    "organization.policies.manage",
    "organization.teams.manage",
    "meetings.organization.create",
    "meetings.organization.manage",
    "calling.enterprise.manage",
    "messaging.organization.moderate",
];

fn matrix_grants(role: &str, capability: &str) -> bool {
    match capability {
        "organization.read" => true,
        "organization.members.read" | "meetings.organization.create" => role != "GUEST",
        "organization.members.invite" | "organization.teams.manage" => {
            matches!(role, "OWNER" | "ADMIN" | "MANAGER")
        }
        "organization.ownership.transfer" | "organization.lifecycle.manage" => role == "OWNER",
        _ => matches!(role, "OWNER" | "ADMIN"),
    }
}

fn matrix_of(role: &str) -> Vec<String> {
    CAPABILITIES
        .iter()
        .filter(|c| matrix_grants(role, c))
        .map(|c| c.to_string())
        .collect()
}

/// Effective capabilities of a membership, as the fake authority computes them.
fn effective_of(policy: &Value, role: &str, membership: &str, org_status: &str) -> Vec<String> {
    if membership != "ACTIVE" {
        return vec![];
    }
    if org_status != "ACTIVE" {
        return if role == "OWNER" {
            vec!["organization.read".to_string()]
        } else {
            vec![]
        };
    }
    let restricted = |c: &str| {
        policy["restrict"][role]
            .as_array()
            .is_some_and(|l| l.iter().any(|x| x == c))
    };
    let disabled = |c: &str| {
        ["meetings", "messaging", "calling"]
            .iter()
            .any(|f| c.starts_with(&format!("{f}.")) && policy["features"][f] == json!(false))
    };
    matrix_of(role)
        .into_iter()
        .filter(|c| !restricted(c) && !disabled(c))
        .collect()
}

fn default_policy() -> Value {
    json!({ "revision": 0, "features": { "meetings": true, "messaging": true, "calling": true }, "restrict": {} })
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
            // Contract: an actor reads only their own organizations.
            if header("x-cf-acting-user").is_some_and(|a| a != user.to_string()) {
                return (StatusCode::FORBIDDEN, Json(envelope("PERMISSION_DENIED")))
                    .into_response();
            }
            let memberships = state
                .memberships
                .lock()
                .unwrap()
                .get(&user)
                .cloned()
                .unwrap_or_default();
            let orgs = state.orgs.lock().unwrap();
            let policies = state.policies.lock().unwrap();
            let items: Vec<Value> = memberships
                .iter()
                .filter(|m| m["status"] != "REMOVED")
                .filter_map(|m| {
                    let org_id = id(m["organizationId"].as_str().unwrap());
                    let org = orgs.get(&org_id)?;
                    let status = org["lifecycleStatus"].as_str().unwrap_or("ACTIVE");
                    if status == "ARCHIVED" {
                        return None;
                    }
                    let policy = policies.get(&org_id).cloned().unwrap_or_else(default_policy);
                    let role = m["role"].as_str().unwrap_or("GUEST");
                    let membership = m["status"].as_str().unwrap_or("ACTIVE");
                    Some(json!({
                        "organization": { "id": org["id"], "name": org["name"], "slug": org["slug"],
                                          "lifecycleStatus": status, "provisioningState": org["provisioningState"], "version": 1 },
                        "membership": { "id": m["membershipId"], "organizationId": org["id"], "userId": user,
                                        "role": role, "status": membership, "version": 1 },
                        "capabilities": effective_of(&policy, role, membership, status),
                    }))
                })
                .collect();
            ok(json!({ "userId": user, "items": items }))
        }
        ("GET", ["organizations", o, "roles", "capabilities"]) => {
            let org = id(o);
            let status = match fake_member_access(&state, org, header("x-cf-acting-user")) {
                Ok(status) => status,
                Err(response) => return response,
            };
            let policy = state
                .policies
                .lock()
                .unwrap()
                .get(&org)
                .cloned()
                .unwrap_or_else(default_policy);
            let roles: Vec<Value> = ["OWNER", "ADMIN", "MANAGER", "MEMBER", "GUEST"]
                .iter()
                .map(|r| {
                    json!({ "role": r, "capabilities": matrix_of(r),
                            "effectiveCapabilities": effective_of(&policy, r, "ACTIVE", &status) })
                })
                .collect();
            ok(json!({ "organizationId": org, "lifecycleStatus": status,
                       "policyRevision": policy["revision"], "capabilities": CAPABILITIES,
                       "roles": roles, "teamRoles": [] }))
        }
        ("GET", ["organizations", o, "policies", "effective"]) => {
            let org = id(o);
            let status = match fake_member_access(&state, org, header("x-cf-acting-user")) {
                Ok(status) => status,
                Err(response) => return response,
            };
            let policy = state
                .policies
                .lock()
                .unwrap()
                .get(&org)
                .cloned()
                .unwrap_or_else(default_policy);
            let provisioning = state.orgs.lock().unwrap()[&org]["provisioningState"].clone();
            ok(
                json!({ "organizationId": org, "lifecycleStatus": status, "provisioningState": provisioning,
                       "revision": policy["revision"],
                       "constraints": { "enabledFeatures": policy["features"], "combination": "STRICTER_WINS" },
                       "legacyMeetingProfilePolicy": {} }),
            )
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

/// The contract's `organization.read` check for an acting user: unknown organization or no
/// ACTIVE membership → 404; a non-OWNER of a suspended / archived organization → 409.
/// Returns the organization's lifecycle status.
#[allow(clippy::result_large_err)]
fn fake_member_access(
    state: &FakeState,
    org: Uuid,
    actor: Option<String>,
) -> Result<String, Response> {
    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(envelope("ORGANIZATION_NOT_FOUND")),
        )
            .into_response()
    };
    let status = match state.orgs.lock().unwrap().get(&org) {
        Some(doc) => doc["lifecycleStatus"]
            .as_str()
            .unwrap_or("ACTIVE")
            .to_string(),
        None => return Err(not_found()),
    };
    let Some(user) = actor.and_then(|a| Uuid::parse_str(&a).ok()) else {
        return Ok(status);
    };
    let role = state.memberships.lock().unwrap().get(&user).and_then(|l| {
        l.iter()
            .find(|m| m["organizationId"] == json!(org) && m["status"] == "ACTIVE")
            .map(|m| m["role"].as_str().unwrap_or("GUEST").to_string())
    });
    match role {
        None => Err(not_found()),
        Some(role) if status != "ACTIVE" && role != "OWNER" => Err((
            StatusCode::CONFLICT,
            Json(envelope(if status == "ARCHIVED" {
                "ORGANIZATION_ARCHIVED"
            } else {
                "ORGANIZATION_SUSPENDED"
            })),
        )
            .into_response()),
        Some(_) => Ok(status),
    }
}

impl Fake {
    /// Sets an organization's lifecycle status (ACTIVE, SUSPENDED or ARCHIVED).
    fn set_org_status(&self, org: Uuid, status: &str) {
        self.state.orgs.lock().unwrap().get_mut(&org).unwrap()["lifecycleStatus"] = json!(status);
    }

    /// Sets the status of `user`'s membership of `org`.
    fn set_membership_status(&self, user: Uuid, org: Uuid, status: &str) {
        let mut memberships = self.state.memberships.lock().unwrap();
        for m in memberships.get_mut(&user).unwrap() {
            if m["organizationId"] == json!(org) {
                m["status"] = json!(status);
            }
        }
    }

    fn set_policy(&self, org: Uuid, policy: Value) {
        self.state.policies.lock().unwrap().insert(org, policy);
    }

    fn unscript(&self, method: &str, path: &str) {
        self.state
            .overrides
            .lock()
            .unwrap()
            .remove(&(method.to_string(), path.to_string()));
    }

    async fn start(jwk: PublicJwk) -> Self {
        let state = Arc::new(FakeState {
            verify: Verify::for_key(jwk),
            requests: Mutex::new(vec![]),
            rejected: Mutex::new(0),
            memberships: Mutex::new(HashMap::new()),
            orgs: Mutex::new(HashMap::new()),
            members: Mutex::new(HashMap::new()),
            overrides: Mutex::new(HashMap::new()),
            policies: Mutex::new(HashMap::new()),
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

// ---------------------------------------------------------------------------------------
// Blueprint Section 8 — workspace context (per request; ADR 0056 §5)
// ---------------------------------------------------------------------------------------

fn workspace_ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_string())
        .collect()
}

fn find_workspace<'a>(page: &'a Value, id: &str) -> Option<&'a Value> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == id)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

fn granted_capabilities(caps: &Value) -> Vec<String> {
    caps["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["granted"] == true)
        .map(|c| c["capability"].as_str().unwrap().to_string())
        .collect()
}

fn reason_of(caps: &Value, capability: &str) -> String {
    caps["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["capability"] == capability)
        .unwrap_or_else(|| panic!("{capability} missing: {caps}"))["reason"]
        .as_str()
        .unwrap()
        .to_string()
}

fn caps_uri(workspace: impl std::fmt::Display) -> String {
    format!("/api/v2/me/workspaces/{workspace}/capabilities")
}

/// The same denial for an organization of someone else, an unknown id and a malformed id.
async fn assert_not_found(app: &TestApp, user: &User, workspaces: &[String]) {
    let mut messages = std::collections::BTreeSet::new();
    for w in workspaces {
        let (s, v) = app.get(caps_uri(w), Auth::Bearer(&user.token)).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::NOT_FOUND, "ORGANIZATION_NOT_FOUND"),
            "{w}: {v}"
        );
        assert!(v["error"]["requestId"].is_string(), "{v}");
        messages.insert(v["error"]["message"].as_str().unwrap().to_string());
    }
    assert_eq!(messages.len(), 1, "non-enumerating: {messages:?}");
}

#[tokio::test]
async fn s8_legacy_workspaces_follow_the_meeting_directory() {
    let app = some!(app_with(None).await);
    let admin = app.admin("platform").await;
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let other = app.user("other").await;
    let outsider = app.user("outsider").await;
    let a = create_legacy_org(&app, &admin, "ws-legacy-a", &owner).await;
    let b = create_legacy_org(&app, &admin, "ws-legacy-b", &other).await;
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/organizations/{a}/members/{}", member.id),
            Auth::Bearer(&owner.token),
            Some(json!({ "role": "MEMBER", "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let (s, page) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{page}");
    assert_eq!(page["authority"], "LEGACY_DIRECTORY");
    assert_eq!(
        workspace_ids(&page),
        vec!["personal".to_string(), a.to_string()]
    );
    assert!(page["nextCursor"].is_null());
    let personal = &page["items"][0];
    assert_eq!(personal["kind"], "PERSONAL");
    assert_eq!(personal["displayName"], "Personal");
    assert!(personal["organizationId"].is_null() && personal["role"].is_null());
    assert_eq!(
        strings(&personal["capabilities"]["granted"]),
        vec!["meetings.personal.create"],
        "no organization creation before the Organizations Service is authoritative"
    );
    let wa = &page["items"][1];
    assert_eq!(wa["kind"], "ORGANIZATION");
    assert_eq!(uuid(&wa["organizationId"]), a);
    assert_eq!(wa["displayName"], "ws-legacy-a");
    assert_eq!(wa["slug"], "ws-legacy-a");
    assert_eq!(wa["role"], "OWNER");
    assert_eq!(wa["organizationStatus"], "ACTIVE");
    assert!(
        wa["provisioningState"].is_null(),
        "no provisioning in legacy mode"
    );
    assert_eq!(wa["capabilities"]["canAdminister"], true);
    assert_eq!(wa["capabilities"]["canInviteMembers"], false);
    assert_eq!(wa["capabilities"]["canCreateMeetings"], true);

    let (s, page) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&member.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    let wa = find_workspace(&page, &a.to_string()).unwrap();
    assert_eq!(wa["role"], "MEMBER");
    assert_eq!(
        strings(&wa["capabilities"]["granted"]),
        vec!["organization.read", "meetings.organization.create"]
    );
    assert_eq!(wa["capabilities"]["canAdminister"], false);
    assert!(find_workspace(&page, &b.to_string()).is_none());

    let (s, caps) = app.get(caps_uri(a), Auth::Bearer(&member.token)).await;
    assert_eq!(s, StatusCode::OK, "{caps}");
    assert_eq!(caps["workspaceId"], a.to_string());
    assert_eq!(caps["kind"], "ORGANIZATION");
    assert_eq!(caps["authority"], "LEGACY_DIRECTORY");
    assert_eq!(caps["role"], "MEMBER");
    assert!(caps["policyRevision"].is_null());
    assert_eq!(
        caps["features"],
        json!({ "meetings": true, "messaging": true, "calling": true })
    );
    assert_eq!(
        caps["capabilities"].as_array().unwrap().len(),
        CAPABILITIES.len()
    );
    assert_eq!(
        reason_of(&caps, "organization.members.read"),
        "CAPABILITY_NOT_GRANTED"
    );
    assert_eq!(
        reason_of(&caps, "organization.members.invite"),
        "FEATURE_DISABLED"
    );
    assert_eq!(reason_of(&caps, "meetings.organization.create"), "ALLOWED");

    let (s, caps) = app
        .get(caps_uri("personal"), Auth::Bearer(&member.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{caps}");
    assert_eq!(caps["kind"], "PERSONAL");
    assert_eq!(reason_of(&caps, "meetings.personal.create"), "ALLOWED");
    assert_eq!(reason_of(&caps, "organizations.create"), "FEATURE_DISABLED");
    assert_eq!(
        caps["personalCommunicationsCapabilities"],
        "/api/v2/communications/me/capabilities"
    );

    // Isolation and non-enumeration: another organization, an unknown id, a malformed id.
    assert_not_found(
        &app,
        &owner,
        &[
            b.to_string(),
            Uuid::new_v4().to_string(),
            "not-a-uuid".into(),
        ],
    )
    .await;
    let (s, page) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&outsider.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(workspace_ids(&page), vec!["personal".to_string()]);

    // Paging: one item per page, PERSONAL first.
    let (s, first) = app
        .get("/api/v2/me/workspaces?limit=1", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(workspace_ids(&first), vec!["personal".to_string()]);
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    let (s, second) = app
        .get(
            format!("/api/v2/me/workspaces?limit=1&cursor={cursor}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(workspace_ids(&second), vec![a.to_string()]);
    assert!(second["nextCursor"].is_null());
    for bad in ["limit=0", "limit=201", "cursor=bogus", "active=true"] {
        let (s, v) = app
            .get(
                format!("/api/v2/me/workspaces?{bad}"),
                Auth::Bearer(&owner.token),
            )
            .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{bad}"
        );
    }
    let (s, v) = app.get("/api/v2/me/workspaces", Auth::None).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
    );

    // A suspended organization is listed with its status and grants nothing.
    let (s, v) = app
        .call(
            Method::PATCH,
            &format!("/api/v2/admin/organizations/{a}"),
            Auth::Bearer(&admin.token),
            Some(json!({ "status": "SUSPENDED" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for user in [&owner, &member] {
        let (_, page) = app
            .get("/api/v2/me/workspaces", Auth::Bearer(&user.token))
            .await;
        let wa = find_workspace(&page, &a.to_string()).unwrap();
        assert_eq!(wa["organizationStatus"], "SUSPENDED");
        assert!(strings(&wa["capabilities"]["granted"]).is_empty());
        let (s, caps) = app.get(caps_uri(a), Auth::Bearer(&user.token)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(caps["organizationStatus"], "SUSPENDED");
        assert!(granted_capabilities(&caps).is_empty());
        assert_eq!(
            caps["features"],
            json!({ "meetings": false, "messaging": false, "calling": false })
        );
        assert!(caps["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["reason"] == "ORGANIZATION_SUSPENDED"));
    }
}

#[tokio::test]
async fn s8_shadow_mode_serves_the_legacy_workspaces() {
    let (signer, fake) = organizations().await;
    let shadow = authority(&signer, &fake.url(), AuthorityMode::Shadow);
    let app = some!(app_with(Some(shadow)).await);
    let admin = app.admin("platform").await;
    let owner = app.user("owner").await;
    let org = create_legacy_org(&app, &admin, "ws-shadow", &owner).await;
    // Organizations knows another organization for the owner: never served in shadow mode.
    let elsewhere = fake.add_org("ws-shadow-elsewhere", &[(owner.id, "OWNER")]);
    let (s, page) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{page}");
    assert_eq!(page["authority"], "LEGACY_DIRECTORY");
    assert_eq!(
        workspace_ids(&page),
        vec!["personal".to_string(), org.to_string()]
    );
    let (s, caps) = app.get(caps_uri(org), Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(caps["authority"], "LEGACY_DIRECTORY");
    assert_eq!(reason_of(&caps, "organization.members.manage"), "ALLOWED");
    assert_not_found(&app, &owner, &[elsewhere.to_string()]).await;
    // Shadow mode only compares memberships in the background; no workspace read reaches
    // Organizations.
    assert!(fake
        .requests()
        .iter()
        .all(|r| r.method == "POST" && r.path == "/internal/v1/organizations/memberships/resolve"));
}

#[tokio::test]
async fn s8_authoritative_workspaces_role_status_and_isolation_matrix() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let owner = app.user("owner").await;
    let org_admin = app.user("orgadmin").await;
    let manager = app.user("manager").await;
    let member = app.user("member").await;
    let guest = app.user("guest").await;
    let other_owner = app.user("otherowner").await;
    let outsider = app.user("outsider").await;
    let one = fake.add_org(
        "ws-one",
        &[
            (owner.id, "OWNER"),
            (org_admin.id, "ADMIN"),
            (manager.id, "MANAGER"),
            (member.id, "MEMBER"),
            (guest.id, "GUEST"),
        ],
    );
    let two = fake.add_org(
        "ws-two",
        &[(other_owner.id, "OWNER"), (manager.id, "MEMBER")],
    );

    for (user, role) in [
        (&owner, "OWNER"),
        (&org_admin, "ADMIN"),
        (&manager, "MANAGER"),
        (&member, "MEMBER"),
        (&guest, "GUEST"),
    ] {
        let (s, page) = app
            .get("/api/v2/me/workspaces", Auth::Bearer(&user.token))
            .await;
        assert_eq!(s, StatusCode::OK, "{role}: {page}");
        assert_eq!(page["authority"], "ORGANIZATIONS_SERVICE");
        assert_eq!(page["items"][0]["id"], "personal");
        assert_eq!(
            strings(&page["items"][0]["capabilities"]["granted"]),
            vec!["meetings.personal.create", "organizations.create"]
        );
        let w = find_workspace(&page, &one.to_string()).unwrap();
        assert_eq!(w["role"], role);
        assert_eq!(w["organizationStatus"], "ACTIVE");
        assert_eq!(w["provisioningState"], "PENDING");
        assert_eq!(w["displayName"], "ws-one Health");
        assert_eq!(
            strings(&w["capabilities"]["granted"]),
            matrix_of(role),
            "{role}"
        );
        assert_eq!(
            find_workspace(&page, &two.to_string()).is_some(),
            role == "MANAGER",
            "{role} sees only their own organizations"
        );
        let users = fake.last(
            "GET",
            &format!("/internal/v1/users/{}/organizations", user.id),
        );
        assert_eq!(users.acting_user, Some(user.id.to_string()));

        let (s, caps) = app.get(caps_uri(one), Auth::Bearer(&user.token)).await;
        assert_eq!(s, StatusCode::OK, "{role}: {caps}");
        assert_eq!(caps["authority"], "ORGANIZATIONS_SERVICE");
        assert_eq!(caps["role"], role);
        assert_eq!(caps["organizationStatus"], "ACTIVE");
        assert_eq!(caps["provisioningState"], "PENDING");
        assert_eq!(caps["policyRevision"], 0);
        assert_eq!(
            caps["features"],
            json!({ "meetings": true, "messaging": true, "calling": true })
        );
        assert_eq!(granted_capabilities(&caps), matrix_of(role), "{role}");
        for c in caps["capabilities"].as_array().unwrap() {
            if c["granted"] == false {
                assert_eq!(c["reason"], "CAPABILITY_NOT_GRANTED", "{role} {c}");
            }
        }
        for path in ["roles/capabilities", "policies/effective"] {
            let r = fake.last("GET", &format!("/internal/v1/organizations/{one}/{path}"));
            assert_eq!(r.acting_user, Some(user.id.to_string()), "{path}");
        }
    }

    // One person, two organizations, two roles: each context is computed on its own.
    let (_, caps) = app.get(caps_uri(two), Auth::Bearer(&manager.token)).await;
    assert_eq!(caps["role"], "MEMBER");
    assert_eq!(granted_capabilities(&caps), matrix_of("MEMBER"));

    // Isolation and non-enumeration.
    for (user, target) in [
        (&other_owner, one.to_string()),
        (&outsider, one.to_string()),
        (&owner, two.to_string()),
    ] {
        assert_not_found(
            &app,
            user,
            &[target, Uuid::new_v4().to_string(), "x".into()],
        )
        .await;
    }
    let (_, page) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&outsider.token))
        .await;
    assert_eq!(workspace_ids(&page), vec!["personal".to_string()]);

    // A suspended membership is not a workspace.
    fake.set_membership_status(member.id, one, "SUSPENDED");
    let (_, page) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&member.token))
        .await;
    assert!(find_workspace(&page, &one.to_string()).is_none());
    assert_not_found(&app, &member, &[one.to_string()]).await;

    // A suspended organization is listed and grants nothing (OWNERs keep reading it).
    fake.set_org_status(one, "SUSPENDED");
    let before = fake.requests().len();
    for (user, role) in [(&owner, "OWNER"), (&org_admin, "ADMIN"), (&guest, "GUEST")] {
        let expected: Vec<String> = if role == "OWNER" {
            vec!["organization.read".into()]
        } else {
            vec![]
        };
        let (_, page) = app
            .get("/api/v2/me/workspaces", Auth::Bearer(&user.token))
            .await;
        let w = find_workspace(&page, &one.to_string()).unwrap();
        assert_eq!(w["organizationStatus"], "SUSPENDED");
        assert_eq!(strings(&w["capabilities"]["granted"]), expected, "{role}");
        assert_eq!(w["capabilities"]["canCreateMeetings"], false);
        let (s, caps) = app.get(caps_uri(one), Auth::Bearer(&user.token)).await;
        assert_eq!(s, StatusCode::OK, "{role}: {caps}");
        assert_eq!(caps["organizationStatus"], "SUSPENDED");
        assert!(caps["policyRevision"].is_null());
        assert_eq!(
            caps["features"],
            json!({ "meetings": false, "messaging": false, "calling": false })
        );
        assert_eq!(granted_capabilities(&caps), expected, "{role}");
        for c in caps["capabilities"].as_array().unwrap() {
            if c["granted"] == false {
                assert_eq!(c["reason"], "ORGANIZATION_SUSPENDED", "{role} {c}");
            }
        }
    }
    assert!(
        fake.requests()[before..]
            .iter()
            .all(|r| !r.path.ends_with("/roles/capabilities")
                && !r.path.ends_with("/policies/effective")),
        "a suspended organization needs no role or policy question"
    );

    // An archived organization is no workspace at all.
    fake.set_org_status(one, "ARCHIVED");
    let (_, page) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&owner.token))
        .await;
    assert!(find_workspace(&page, &one.to_string()).is_none());
    assert_not_found(&app, &owner, &[one.to_string()]).await;
    assert_eq!(*fake.state.rejected.lock().unwrap(), 0);
}

#[tokio::test]
async fn s8_authoritative_policy_restricted_capabilities() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let owner = app.user("owner").await;
    let manager = app.user("manager").await;
    let member = app.user("member").await;
    let org = fake.add_org(
        "ws-policy",
        &[
            (owner.id, "OWNER"),
            (manager.id, "MANAGER"),
            (member.id, "MEMBER"),
        ],
    );
    fake.set_policy(
        org,
        json!({ "revision": 4,
                "features": { "meetings": true, "messaging": true, "calling": false },
                "restrict": { "MANAGER": ["organization.members.invite"],
                              "MEMBER": ["meetings.organization.create"] } }),
    );

    let (s, caps) = app.get(caps_uri(org), Auth::Bearer(&manager.token)).await;
    assert_eq!(s, StatusCode::OK, "{caps}");
    assert_eq!(caps["policyRevision"], 4);
    assert_eq!(
        caps["features"],
        json!({ "meetings": true, "messaging": true, "calling": false })
    );
    assert_eq!(
        reason_of(&caps, "organization.members.invite"),
        "POLICY_RESTRICTED"
    );
    assert_eq!(reason_of(&caps, "organization.teams.manage"), "ALLOWED");
    assert_eq!(
        reason_of(&caps, "organization.audit.read"),
        "CAPABILITY_NOT_GRANTED"
    );
    let (_, page) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&manager.token))
        .await;
    let w = find_workspace(&page, &org.to_string()).unwrap();
    assert_eq!(w["capabilities"]["canInviteMembers"], false);
    assert!(!strings(&w["capabilities"]["granted"])
        .contains(&"organization.members.invite".to_string()));

    let (_, caps) = app.get(caps_uri(org), Auth::Bearer(&member.token)).await;
    assert_eq!(
        reason_of(&caps, "meetings.organization.create"),
        "POLICY_RESTRICTED"
    );
    let (_, page) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&member.token))
        .await;
    assert_eq!(
        find_workspace(&page, &org.to_string()).unwrap()["capabilities"]["canCreateMeetings"],
        false
    );

    // A disabled feature removes its capabilities for every role, OWNER included.
    let (_, caps) = app.get(caps_uri(org), Auth::Bearer(&owner.token)).await;
    assert_eq!(
        reason_of(&caps, "calling.enterprise.manage"),
        "POLICY_RESTRICTED"
    );
    assert_eq!(reason_of(&caps, "organization.policies.manage"), "ALLOWED");
}

#[tokio::test]
async fn s8_authoritative_workspaces_fail_closed() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let owner = app.user("owner").await;
    let org = fake.add_org("ws-closed", &[(owner.id, "OWNER")]);
    let (s, _) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK);

    let unavailable = |s: StatusCode, v: &Value, what: &str| {
        assert_eq!(
            (s, code(v)),
            (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE"),
            "{what}: {v}"
        );
        assert!(v["error"]["requestId"].is_string(), "{what}: {v}");
        assert!(v.get("items").is_none() && v.get("capabilities").is_none());
    };

    // Organizations unreachable: never a partial list, never a capability.
    fake.state.down.store(true, Ordering::SeqCst);
    let (s, v) = app
        .get("/api/v2/me/workspaces", Auth::Bearer(&owner.token))
        .await;
    unavailable(s, &v, "list while down");
    let (s, v) = app.get(caps_uri(org), Auth::Bearer(&owner.token)).await;
    unavailable(s, &v, "capabilities while down");
    // The personal context needs no organization answer.
    let (s, caps) = app
        .get(caps_uri("personal"), Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{caps}");
    assert_eq!(reason_of(&caps, "organizations.create"), "ALLOWED");
    fake.state.down.store(false, Ordering::SeqCst);

    // Server errors, a refused service credential and malformed answers.
    let users = format!("/internal/v1/users/{}/organizations", owner.id);
    for (status, doc) in [
        (500, envelope("INTERNAL_ERROR")),
        (403, envelope("ACTOR_NOT_ALLOWED")),
        (401, envelope("INTERNAL_AUTHENTICATION_FAILED")),
        (200, json!({ "userId": owner.id })),
    ] {
        fake.script("GET", &users, status, doc.clone());
        let (s, v) = app
            .get("/api/v2/me/workspaces", Auth::Bearer(&owner.token))
            .await;
        unavailable(s, &v, &format!("list {status} {doc}"));
        let (s, v) = app.get(caps_uri(org), Auth::Bearer(&owner.token)).await;
        unavailable(s, &v, &format!("capabilities {status} {doc}"));
    }
    fake.unscript("GET", &users);

    let roles = format!("/internal/v1/organizations/{org}/roles/capabilities");
    let policy = format!("/internal/v1/organizations/{org}/policies/effective");
    for (path, status, doc) in [
        (&roles, 500, envelope("INTERNAL_ERROR")),
        (&roles, 200, json!({ "organizationId": org })),
        (
            &policy,
            200,
            json!({ "organizationId": org, "lifecycleStatus": "ACTIVE", "revision": 1,
                    "constraints": { "enabledFeatures": { "meetings": true } } }),
        ),
        (
            &policy,
            200,
            json!({ "organizationId": Uuid::new_v4(), "lifecycleStatus": "ACTIVE", "revision": 1,
                    "constraints": { "enabledFeatures": { "meetings": true, "messaging": true, "calling": true } } }),
        ),
        (&policy, 503, envelope("SERVICE_UNAVAILABLE")),
    ] {
        fake.script("GET", path, status, doc.clone());
        let (s, v) = app.get(caps_uri(org), Auth::Bearer(&owner.token)).await;
        unavailable(s, &v, &format!("{path} {status} {doc}"));
        fake.unscript("GET", path);
    }
    // The membership ended between the reads: the authority's refusal is relayed.
    fake.script("GET", &roles, 404, envelope("ORGANIZATION_NOT_FOUND"));
    let (s, v) = app.get(caps_uri(org), Auth::Bearer(&owner.token)).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "ORGANIZATION_NOT_FOUND")
    );
    fake.unscript("GET", &roles);
    let (s, caps) = app.get(caps_uri(org), Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{caps}");
    assert_eq!(granted_capabilities(&caps), matrix_of("OWNER"));
    assert_eq!(*fake.state.rejected.lock().unwrap(), 0);
}

// ---------------------------------------------------------------------------------------
// Blueprint Section 12 follow-up — Meeting write paths enforce the organization policy
// (the same decision `GET /api/v2/me/workspaces/{id}/capabilities` reports).
// ---------------------------------------------------------------------------------------

async fn create_org_space(
    app: &TestApp,
    user: &User,
    org: Uuid,
    body: Value,
) -> (StatusCode, Value) {
    let mut body = body;
    body["organizationId"] = json!(org);
    app.post("/api/v2/meeting-spaces", Auth::Bearer(&user.token), body)
        .await
}

async fn admit_one(
    app: &TestApp,
    host: &User,
    conference: Uuid,
    pid: &Value,
) -> (StatusCode, Value) {
    app.post(
        &format!("/api/v2/conferences/{conference}/waiting-room/admit"),
        Auth::Bearer(&host.token),
        json!({ "participantIds": [pid] }),
    )
    .await
}

#[tokio::test]
async fn s12_organization_meeting_creation_follows_the_capabilities_read() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let guest = app.user("guest").await;
    let org = fake.add_org(
        "s12-create",
        &[
            (owner.id, "OWNER"),
            (member.id, "MEMBER"),
            (guest.id, "GUEST"),
        ],
    );
    let body = json!({ "title": "org meeting", "accessPolicy": "ORGANIZATION_ONLY" });

    // Allow: default policy — the read says ALLOWED and creation succeeds.
    let (_, caps) = app.get(caps_uri(org), Auth::Bearer(&member.token)).await;
    assert_eq!(reason_of(&caps, "meetings.organization.create"), "ALLOWED");
    let (s, v) = create_org_space(&app, &member, org, body.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let space = uuid(&v["id"]);

    // Deny (policy): hostCreation raised above MEMBER — the read reports
    // POLICY_RESTRICTED and creation and duplication into the organization are refused.
    fake.set_policy(
        org,
        json!({ "revision": 2,
                "features": { "meetings": true, "messaging": true, "calling": true },
                "restrict": { "MEMBER": ["meetings.organization.create"] } }),
    );
    let (_, caps) = app.get(caps_uri(org), Auth::Bearer(&member.token)).await;
    assert_eq!(
        reason_of(&caps, "meetings.organization.create"),
        "POLICY_RESTRICTED"
    );
    let (s, v) = create_org_space(&app, &member, org, body.clone()).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "ORGANIZATION_POLICY_RESTRICTED"),
        "{v}"
    );
    let (s, v) = app
        .post(
            &format!("/api/v2/meeting-spaces/{space}/duplicate"),
            Auth::Bearer(&member.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "ORGANIZATION_POLICY_RESTRICTED"),
        "duplicate into the organization: {v}"
    );
    // The OWNER's role is not restricted: allowed, exactly as the read says.
    let (_, caps) = app.get(caps_uri(org), Auth::Bearer(&owner.token)).await;
    assert_eq!(reason_of(&caps, "meetings.organization.create"), "ALLOWED");
    let (s, v) = create_org_space(&app, &owner, org, body.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    // Personal meetings are never subject to organization policy.
    let personal = app
        .create_space(&member, json!({ "title": "personal" }))
        .await;
    assert!(personal["id"].is_string());

    // Deny (feature): features.meetings = false removes meetings for every role.
    fake.set_policy(
        org,
        json!({ "revision": 3,
                "features": { "meetings": false, "messaging": true, "calling": true },
                "restrict": {} }),
    );
    let (_, caps) = app.get(caps_uri(org), Auth::Bearer(&owner.token)).await;
    assert_eq!(caps["features"]["meetings"], false);
    assert_eq!(
        reason_of(&caps, "meetings.organization.create"),
        "POLICY_RESTRICTED"
    );
    let (s, v) = create_org_space(&app, &owner, org, body.clone()).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "ORGANIZATION_POLICY_RESTRICTED"),
        "{v}"
    );

    // Not held by the role at all (GUEST): the directory does not count a GUEST as a
    // meeting member, so the pre-existing membership check refuses first.
    fake.set_policy(org, default_policy());
    let (_, caps) = app.get(caps_uri(org), Auth::Bearer(&guest.token)).await;
    assert_eq!(
        reason_of(&caps, "meetings.organization.create"),
        "CAPABILITY_NOT_GRANTED"
    );
    let (s, v) = create_org_space(&app, &guest, org, body.clone()).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        "{v}"
    );

    // Suspended organization: nothing is created.
    let (s, v) = create_org_space(&app, &member, org, body.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "allowed again: {v}");
    fake.set_org_status(org, "SUSPENDED");
    let (s, v) = create_org_space(&app, &owner, org, body.clone()).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "ORGANIZATION_SUSPENDED"),
        "{v}"
    );
    assert_eq!(*fake.state.rejected.lock().unwrap(), 0);
}

#[tokio::test]
async fn s12_organization_meeting_creation_fails_closed_when_the_policy_read_fails() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let owner = app.user("owner").await;
    let org = fake.add_org("s12-closed", &[(owner.id, "OWNER")]);
    let body = json!({ "title": "org meeting", "accessPolicy": "ORGANIZATION_ONLY" });
    let roles = format!("/internal/v1/organizations/{org}/roles/capabilities");
    let policy = format!("/internal/v1/organizations/{org}/policies/effective");
    let users = format!("/internal/v1/users/{}/organizations", owner.id);
    for (path, status, doc) in [
        (&roles, 503, envelope("SERVICE_UNAVAILABLE")),
        (&roles, 500, envelope("INTERNAL_ERROR")),
        (&policy, 503, envelope("SERVICE_UNAVAILABLE")),
        (
            &policy,
            200,
            json!({ "organizationId": org, "lifecycleStatus": "ACTIVE", "revision": 1,
                    "constraints": { "enabledFeatures": { "meetings": true } } }),
        ),
        (&users, 401, envelope("INTERNAL_AUTHENTICATION_FAILED")),
    ] {
        fake.script("GET", path, status, doc.clone());
        let (s, v) = create_org_space(&app, &owner, org, body.clone()).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE"),
            "{path} {status}: {v}"
        );
        fake.unscript("GET", path);
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_spaces WHERE organization_id = '{org}'"
        ))
        .await,
        0,
        "nothing was created while the decision was unavailable"
    );
    // Fully down: the directory already counts no membership (pre-existing behaviour).
    fake.state.down.store(true, Ordering::SeqCst);
    let (s, _) = create_org_space(&app, &owner, org, body.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    fake.state.down.store(false, Ordering::SeqCst);
    let (s, v) = create_org_space(&app, &owner, org, body).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

#[tokio::test]
async fn s12_admission_and_admit_follow_the_organization_meetings_feature() {
    let (signer, fake) = organizations().await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let app = some!(app_with(Some(live)).await);
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let late = app.user("late").await;
    let org = fake.add_org(
        "s12-admit",
        &[
            (owner.id, "OWNER"),
            (member.id, "MEMBER"),
            (late.id, "MEMBER"),
        ],
    );

    // ORGANIZATION_ONLY: membership grants entry only while meetings are on.
    let (_, org_only) = app
        .live_conference(
            &owner,
            json!({ "title": "org", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": org }),
        )
        .await;
    let (s, v) = app
        .join(org_only, Auth::Bearer(&member.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "allowed: {v}");
    fake.set_policy(
        org,
        json!({ "revision": 5,
                "features": { "meetings": false, "messaging": true, "calling": true },
                "restrict": {} }),
    );
    let (s, v) = app
        .join(org_only, Auth::Bearer(&late.token), json!({}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED"),
        "meetings turned off: the membership grants nothing: {v}"
    );
    // Policy read unavailable during a join that relies on membership → 503.
    fake.set_policy(org, default_policy());
    let policy = format!("/internal/v1/organizations/{org}/policies/effective");
    fake.script("GET", &policy, 503, envelope("SERVICE_UNAVAILABLE"));
    let (s, v) = app
        .join(org_only, Auth::Bearer(&late.token), json!({}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE"),
        "{v}"
    );
    fake.unscript("GET", &policy);
    let (s, v) = app
        .join(org_only, Auth::Bearer(&late.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "allowed again: {v}");

    // Host admit into an organization's meeting.
    let (_, waiting) = app
        .live_conference(
            &owner,
            json!({ "title": "lobby", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE",
                    "organizationId": org }),
        )
        .await;
    let mut waiting_ids = vec![];
    for name in ["guest-a", "guest-b", "guest-c"] {
        let (s, v) = app
            .join(waiting, Auth::None, json!({ "displayName": name }))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["participant"]["admissionState"], "WAITING", "{v}");
        waiting_ids.push(v["participant"]["id"].clone());
    }
    // Allow.
    let (s, v) = admit_one(&app, &owner, waiting, &waiting_ids[0]).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["changed"].as_array().unwrap().len(), 1);
    // Deny: meetings turned off by the organization.
    fake.set_policy(
        org,
        json!({ "revision": 6,
                "features": { "meetings": false, "messaging": true, "calling": true },
                "restrict": {} }),
    );
    let (s, v) = admit_one(&app, &owner, waiting, &waiting_ids[1]).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "ORGANIZATION_POLICY_RESTRICTED"),
        "{v}"
    );
    // Organizations down: 503, never an admit.
    fake.set_policy(org, default_policy());
    fake.state.down.store(true, Ordering::SeqCst);
    let (s, v) = admit_one(&app, &owner, waiting, &waiting_ids[1]).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE"),
        "{v}"
    );
    fake.state.down.store(false, Ordering::SeqCst);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id = '{waiting}' \
             AND admission_state = 'ADMITTED' AND user_id IS NULL"
        ))
        .await,
        1,
        "refused admits changed nothing"
    );
    let (s, v) = admit_one(&app, &owner, waiting, &waiting_ids[1]).await;
    assert_eq!(s, StatusCode::OK, "allowed again: {v}");
}

#[tokio::test]
async fn s12_legacy_mode_meeting_writes_are_unchanged() {
    // No Organizations authority: the legacy directory decides exactly as before and no
    // policy read is attempted.
    let app = some!(app_with(None).await);
    let admin = app.admin("platform").await;
    let owner = app.user("owner").await;
    let (s, v) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&admin.token),
            json!({ "name": "s12 legacy", "slug": "s12-legacy", "ownerUserId": owner.id }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let org = uuid(&v["id"]);
    let (s, v) = create_org_space(
        &app,
        &owner,
        org,
        json!({ "title": "legacy org meeting", "accessPolicy": "ORGANIZATION_ONLY" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

// ---------------------------------------------------------------------------------------
// Blueprint Section 12 follow-up — per-organization Enterprise Calling relay
// (CAL-007 / ORG-012 / CAL-009). Fake Calling + Platform Access peers verify Meeting
// Service's token with receiver-authoritative trust bundles and record the signed
// delegated context.
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PeerRecord {
    method: String,
    path: String,
    query: Option<String>,
    authorization: bool,
    idempotency_key: Option<String>,
    tenant: Uuid,
    application: Uuid,
    environment: Uuid,
    service_account: Uuid,
    credential: Option<Uuid>,
    principal: Option<(Uuid, String)>,
    body: Value,
}

struct PeerState {
    calling: ServiceTokenVerifier,
    platform_access: ServiceTokenVerifier,
    records: Mutex<Vec<PeerRecord>>,
    rejected: Mutex<u32>,
    principal: Uuid,
    /// Scripted Calling answer (status, body) for every customer path.
    calling_answer: Mutex<Option<(u16, Value)>>,
    /// Scripted Platform Access answer for principal ensure.
    ensure_answer: Mutex<Option<(u16, Value)>>,
}

struct Peers {
    addr: SocketAddr,
    state: Arc<PeerState>,
}

fn peer_verifier(jwk: &PublicJwk, receiver: &str, permission: &str) -> ServiceTokenVerifier {
    let bundle = json!({ "bundle_version": 1, "receiver": receiver, "runtime_environment": "test",
        "trusted_issuers": [{ "issuer": "cfmeet.meeting-service", "audience": receiver,
            "runtime_environment": "test", "keys": [jwk], "permissions": [permission] }]});
    let policy = TrustPolicy::from_json(
        &bundle.to_string(),
        &ServiceId::parse(receiver).unwrap(),
        RuntimeEnvironment::Test,
    )
    .expect("the permission registry accepts the bundle");
    ServiceTokenVerifier::new(policy, Arc::new(SystemClock))
}

async fn peer_handler(
    State(state): State<Arc<PeerState>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let to_platform_access = path.starts_with("/internal/v1/principals/");
    let (verifier, permission) = if to_platform_access {
        (
            &state.platform_access,
            InternalPermission::PlatformPrincipalEnsure,
        )
    } else {
        (
            &state.calling,
            InternalPermission::CallingRequestActForOrganization,
        )
    };
    let Ok(ctx) = verifier.verify_headers(&headers) else {
        *state.rejected.lock().unwrap() += 1;
        return (
            StatusCode::UNAUTHORIZED,
            Json(envelope("AUTHENTICATION_FAILED")),
        )
            .into_response();
    };
    let Some(delegated) = ctx.delegated_platform_context().cloned() else {
        *state.rejected.lock().unwrap() += 1;
        return (
            StatusCode::UNAUTHORIZED,
            Json(envelope("AUTHENTICATION_FAILED")),
        )
            .into_response();
    };
    assert!(ctx.require_permission(permission).is_ok());
    let header = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state.records.lock().unwrap().push(PeerRecord {
        method: method.to_string(),
        path: path.clone(),
        query: uri.query().map(str::to_string),
        authorization: headers.contains_key("authorization"),
        idempotency_key: header("idempotency-key"),
        tenant: delegated.tenant_id(),
        application: delegated.application_id(),
        environment: delegated.application_environment_id(),
        service_account: delegated.initiating_service_account_id(),
        credential: delegated.initiating_credential_id(),
        principal: delegated.initiating_principal().map(|p| {
            (
                p.principal_id,
                serde_json::to_value(p.principal_kind)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        }),
        body: body.clone(),
    });
    if to_platform_access {
        if let Some((status, doc)) = state.ensure_answer.lock().unwrap().clone() {
            return (StatusCode::from_u16(status).unwrap(), Json(doc)).into_response();
        }
        return Json(
            json!({ "principal": { "id": state.principal, "kind": "NATIVE_CF_USER" },
                            "created": true }),
        )
        .into_response();
    }
    if let Some((status, doc)) = state.calling_answer.lock().unwrap().clone() {
        return (StatusCode::from_u16(status).unwrap(), Json(doc)).into_response();
    }
    match method.as_str() {
        "POST" => (
            StatusCode::CREATED,
            Json(json!({ "id": Uuid::new_v4(), "echo": body })),
        )
            .into_response(),
        "DELETE" => StatusCode::NO_CONTENT.into_response(),
        _ => Json(json!({ "data": [], "path": path })).into_response(),
    }
}

impl Peers {
    async fn start(jwk: PublicJwk) -> Self {
        let state = Arc::new(PeerState {
            calling: peer_verifier(
                &jwk,
                "cfmeet.calling",
                "calling-request.act-for-organization",
            ),
            platform_access: peer_verifier(
                &jwk,
                "cfmeet.platform-access",
                "platform-principal.ensure",
            ),
            records: Mutex::new(vec![]),
            rejected: Mutex::new(0),
            principal: Uuid::new_v4(),
            calling_answer: Mutex::new(None),
            ensure_answer: Mutex::new(None),
        });
        let router = Router::new()
            .fallback(peer_handler)
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { addr, state }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn records(&self) -> Vec<PeerRecord> {
        self.state.records.lock().unwrap().clone()
    }

    fn calling_records(&self) -> Vec<PeerRecord> {
        self.records()
            .into_iter()
            .filter(|r| !r.path.starts_with("/internal/"))
            .collect()
    }
}

struct CallingWorld {
    app: TestApp,
    fake: Fake,
    peers: Peers,
    org: Uuid,
    owner: User,
    member: User,
    triple: (Uuid, Uuid, Uuid),
    binding: (Uuid, Uuid),
}

impl CallingWorld {
    fn scope_doc(&self, bound: bool) -> Value {
        json!({ "organizationId": self.org, "capability": "primary", "tenantId": self.triple.0,
                "applicationId": self.triple.1, "environmentId": self.triple.2,
                "mappingVersion": 1, "verifiedAt": now(),
                "serviceBindings": { "calling": if bound {
                    json!({ "serviceAccountId": self.binding.0, "credentialId": self.binding.1, "version": 1 })
                } else { Value::Null } } })
    }

    fn bind(&self, bound: bool) {
        self.fake.script(
            "POST",
            "/internal/v1/organizations/scopes/resolve",
            200,
            self.scope_doc(bound),
        );
    }

    async fn call(
        &self,
        method: Method,
        user: &User,
        rest: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        self.app
            .call(
                method,
                &format!("/api/v2/organizations/{}/calling/{rest}", self.org),
                Auth::Bearer(&user.token),
                body,
            )
            .await
    }
}

async fn calling_world(enabled: bool) -> Option<CallingWorld> {
    let (signer, fake) = organizations().await;
    let peers = Peers::start(signer.public_jwk()).await;
    let live = authority(&signer, &fake.url(), AuthorityMode::Authoritative);
    let relay = enabled.then(|| {
        Arc::new(
            OrganizationCalling::new(
                signer.clone(),
                &peers.url(),
                &peers.url(),
                Duration::from_secs(3),
            )
            .unwrap(),
        )
    });
    let app = TestApp::with(TestOptions {
        organizations_authority: Some(live),
        organization_calling: relay,
        ..TestOptions::default()
    })
    .await?;
    let owner = app.user("enterprise-owner").await;
    let member = app.user("enterprise-member").await;
    let org = fake.add_org("s12-calling", &[(owner.id, "OWNER"), (member.id, "MEMBER")]);
    let world = CallingWorld {
        app,
        fake,
        peers,
        org,
        owner,
        member,
        triple: (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()),
        binding: (Uuid::new_v4(), Uuid::new_v4()),
    };
    world.bind(true);
    Some(world)
}

#[tokio::test]
async fn s12_organization_calling_uses_the_organization_scope_and_the_acting_member() {
    let w = some!(calling_world(true).await);
    let id = Uuid::new_v4();

    // Allow: every method reaches Calling in the ORGANIZATION's scope, with its own
    // credential (id only) and the acting member's principal — never a customer credential.
    let (s, v) = w
        .call(
            Method::GET,
            &w.owner,
            "v1/enterprise/extensions?limit=5",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = w
        .app
        .call_with_headers(
            Method::POST,
            &format!(
                "/api/v2/organizations/{}/calling/v1/enterprise/queues",
                w.org
            ),
            &[
                ("authorization", &format!("Bearer {}", w.owner.token)),
                ("content-type", "application/json"),
                ("idempotency-key", "queue-create-1"),
            ],
            json!({ "name": "Front desk" }).to_string(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["echo"]["name"], "Front desk");
    let (s, v) = w
        .call(
            Method::PUT,
            &w.owner,
            &format!("v1/enterprise/ring-groups/{id}/members"),
            Some(json!({ "members": [] })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = w
        .call(
            Method::PATCH,
            &w.owner,
            &format!("v1/enterprise/policies/{id}"),
            Some(json!({ "expectedVersion": 1 })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = w
        .call(Method::DELETE, &w.owner, &format!("v1/webhooks/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let calls = w.peers.calling_records();
    assert_eq!(
        calls.iter().map(|r| r.method.as_str()).collect::<Vec<_>>(),
        ["GET", "POST", "PUT", "PATCH", "DELETE"]
    );
    for r in &calls {
        assert!(!r.authorization, "no customer credential is ever sent");
        assert_eq!((r.tenant, r.application, r.environment), w.triple);
        assert_eq!(r.service_account, w.binding.0);
        assert_eq!(r.credential, Some(w.binding.1));
        assert_eq!(
            r.principal,
            Some((w.peers.state.principal, "NATIVE_CF_USER".to_string()))
        );
    }
    assert_eq!(calls[0].path, "/v1/enterprise/extensions");
    assert_eq!(calls[0].query.as_deref(), Some("limit=5"));
    assert_eq!(calls[1].idempotency_key.as_deref(), Some("queue-create-1"));
    assert_eq!(calls[1].body["name"], "Front desk");
    // The member's principal is ensured once in the organization's environment (no
    // credential in that call) and cached; it is not the personal-scope principal.
    let ensures: Vec<PeerRecord> = w
        .peers
        .records()
        .into_iter()
        .filter(|r| r.path == "/internal/v1/principals/ensure")
        .collect();
    assert_eq!(ensures.len(), 1);
    assert_eq!(
        (ensures[0].tenant, ensures[0].environment),
        (w.triple.0, w.triple.2)
    );
    assert_eq!(ensures[0].credential, None);
    assert_eq!(
        ensures[0].body["externalReference"],
        format!("cfmeet-user:{}", w.owner.id)
    );
    assert_eq!(
        w.app
            .scalar(&format!(
                "SELECT count(*) FROM communication_principal_links WHERE subject_id = '{}' \
                 AND environment_id = '{}'",
                w.owner.id, w.triple.2
            ))
            .await,
        1
    );
    // Organizations was asked with the verified acting user only.
    let resolve = w
        .fake
        .last("POST", "/internal/v1/organizations/scopes/resolve");
    assert_eq!(resolve.acting_user, Some(w.owner.id.to_string()));
    assert_eq!(resolve.body["organizationId"], json!(w.org));
    // Calling's own refusals are relayed with their envelope.
    *w.peers.state.calling_answer.lock().unwrap() = Some((
        404,
        json!({ "error": { "code": "NOT_FOUND", "message": "x" } }),
    ));
    let (s, v) = w
        .call(
            Method::GET,
            &w.owner,
            &format!("v1/enterprise/extensions/{id}"),
            None,
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("NOT_FOUND"))
    );
    assert_eq!(*w.peers.state.rejected.lock().unwrap(), 0);
}

#[tokio::test]
async fn s12_organization_calling_admin_rights_come_from_organizations() {
    let w = some!(calling_world(true).await);
    let forwarded = |w: &CallingWorld| w.peers.calling_records().len();

    // Deny: a MEMBER does not hold calling.enterprise.manage (the read agrees).
    let (_, caps) = w
        .app
        .get(caps_uri(w.org), Auth::Bearer(&w.member.token))
        .await;
    assert_eq!(
        reason_of(&caps, "calling.enterprise.manage"),
        "CAPABILITY_NOT_GRANTED"
    );
    let (s, v) = w
        .call(Method::GET, &w.member, "v1/enterprise/extensions", None)
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        "{v}"
    );
    // Deny: the organization turned Calling off — even for its OWNER.
    w.fake.set_policy(
        w.org,
        json!({ "revision": 2, "features": { "meetings": true, "messaging": true, "calling": false },
                "restrict": {} }),
    );
    let (_, caps) = w
        .app
        .get(caps_uri(w.org), Auth::Bearer(&w.owner.token))
        .await;
    assert_eq!(
        reason_of(&caps, "calling.enterprise.manage"),
        "POLICY_RESTRICTED"
    );
    let (s, v) = w
        .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "ORGANIZATION_POLICY_RESTRICTED"),
        "{v}"
    );
    w.fake.set_policy(w.org, default_policy());
    // Not an organization administration route: never forwarded.
    for rest in [
        "v1/calls",
        "v1/calling/endpoints",
        "v1/privacy-relay/contexts",
        "v1/enterprise/../calls",
    ] {
        let (s, v) = w.call(Method::GET, &w.owner, rest, None).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::NOT_FOUND, "ENDPOINT_NOT_FOUND"),
            "{rest}: {v}"
        );
    }
    // A foreign / unknown / malformed organization: the same non-enumerating answer.
    let outsider = w.app.user("outsider").await;
    let (s, v) = w
        .call(Method::GET, &outsider, "v1/enterprise/extensions", None)
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "ORGANIZATION_NOT_FOUND"),
        "{v}"
    );
    let (s, v) = w
        .app
        .get(
            "/api/v2/organizations/not-a-uuid/calling/v1/enterprise/extensions",
            Auth::Bearer(&w.owner.token),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "ORGANIZATION_NOT_FOUND"),
        "{v}"
    );
    // Not set up yet: no Calling binding, or the scope is still provisioning.
    w.bind(false);
    let (s, v) = w
        .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "ORGANIZATION_CALLING_DISABLED"),
        "{v}"
    );
    w.fake.script(
        "POST",
        "/internal/v1/organizations/scopes/resolve",
        409,
        envelope("PROVISIONING_PENDING"),
    );
    let (s, v) = w
        .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "PROVISIONING_PENDING"),
        "{v}"
    );
    // Bad bodies are refused before anything is forwarded.
    w.bind(true);
    let (s, v) = w
        .app
        .post_raw(
            &format!(
                "/api/v2/organizations/{}/calling/v1/enterprise/queues",
                w.org
            ),
            Auth::Bearer(&w.owner.token),
            "[1,2]",
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
        "{v}"
    );
    assert_eq!(forwarded(&w), 0, "nothing denied ever reached Calling");
    // Unauthenticated.
    let (s, v) = w
        .app
        .get(
            format!(
                "/api/v2/organizations/{}/calling/v1/enterprise/extensions",
                w.org
            ),
            Auth::None,
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
    );
}

#[tokio::test]
async fn s12_organization_calling_fails_closed() {
    let w = some!(calling_world(true).await);
    let unavailable = |s: StatusCode, v: &Value, what: &str| {
        assert_eq!(
            (s, code(v)),
            (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE"),
            "{what}: {v}"
        );
    };
    // Organizations down: 503, never forwarded.
    w.fake.state.down.store(true, Ordering::SeqCst);
    let (s, v) = w
        .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
        .await;
    unavailable(s, &v, "organizations down");
    w.fake.state.down.store(false, Ordering::SeqCst);
    assert!(w.peers.calling_records().is_empty());
    // Scope resolution unavailable.
    w.fake.script(
        "POST",
        "/internal/v1/organizations/scopes/resolve",
        503,
        envelope("SERVICE_UNAVAILABLE"),
    );
    let (s, v) = w
        .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
        .await;
    unavailable(s, &v, "scope resolution unavailable");
    w.bind(true);
    // Platform Access unavailable / refusing the organization principal.
    *w.peers.state.ensure_answer.lock().unwrap() = Some((503, envelope("SERVICE_UNAVAILABLE")));
    let (s, v) = w
        .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
        .await;
    unavailable(s, &v, "platform access unavailable");
    *w.peers.state.ensure_answer.lock().unwrap() =
        Some((409, envelope("DELEGATED_CONTEXT_INVALID")));
    let (s, v) = w
        .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "ORGANIZATION_CALLING_DISABLED"),
        "{v}"
    );
    *w.peers.state.ensure_answer.lock().unwrap() = None;
    // Calling refuses the forward (flag off, trust bundle, revoked credential) or fails.
    for (status, doc) in [
        (401, envelope("AUTHENTICATION_FAILED")),
        (500, envelope("INTERNAL_ERROR")),
        (503, envelope("SERVICE_UNAVAILABLE")),
    ] {
        *w.peers.state.calling_answer.lock().unwrap() = Some((status, doc));
        let (s, v) = w
            .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
            .await;
        unavailable(s, &v, &format!("calling {status}"));
    }
    *w.peers.state.calling_answer.lock().unwrap() = None;
    let (s, v) = w
        .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
        .await;
    assert_eq!(s, StatusCode::OK, "recovers: {v}");
}

#[tokio::test]
async fn s12_organization_calling_is_disabled_by_default() {
    // ORGANIZATION_CALLING_SCOPE=shared (no relay): the shared-credential path stays in
    // use and the relay answers 409 without contacting anyone.
    let w = some!(calling_world(false).await);
    let (s, v) = w
        .call(Method::GET, &w.owner, "v1/enterprise/extensions", None)
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "ORGANIZATION_CALLING_DISABLED"),
        "{v}"
    );
    assert!(w.peers.records().is_empty());
    // Legacy organizations mode: also disabled.
    let app = some!(app_with(None).await);
    let user = app.user("legacy").await;
    let (s, v) = app
        .get(
            format!(
                "/api/v2/organizations/{}/calling/v1/enterprise/extensions",
                Uuid::new_v4()
            ),
            Auth::Bearer(&user.token),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "ORGANIZATION_CALLING_DISABLED"),
        "{v}"
    );
}
