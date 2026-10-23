//! Section 50 — the developer control plane through the REAL router with REAL gateway
//! service tokens, against an in-memory Platform Access (delegation, evaluation) and
//! session authority. The real processes are exercised by the API Gateway journeys.

mod common;

use axum::http::{HeaderMap, Method, StatusCode};
use cf_service_auth::{OutboundServiceRequest, ServiceId};
use common::*;
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";
const ERR: &str = "platform-developer/common/v1.schema.json#/$defs/Error";

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

async fn org(c: &Control, token: &str, name: &str) -> Uuid {
    let r = c
        .req(
            As::User(token),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": name })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    conforms(&format!("{CTRL}Organization"), &r.json);
    id(&r.json)
}

/// Invites `email` with `role` and has its owner accept; returns the member id.
async fn join(c: &Control, owner: &str, org: Uuid, email: &str, role: &str) -> (String, Uuid) {
    let (token, _) = c.sessions.sign_in(email);
    let inv = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/organizations/{org}/invitations"),
            Some(json!({ "email": email, "role": role })),
        )
        .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.json);
    conforms(&format!("{CTRL}InvitationWithToken"), &inv.json);
    let acc = c
        .req(
            As::User(&token),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": inv.json["token"] })),
        )
        .await;
    assert_eq!(acc.status, StatusCode::OK, "{}", acc.json);
    conforms(&format!("{CTRL}Member"), &acc.json);
    (token, id(&acc.json))
}

#[tokio::test]
async fn an_organization_maps_to_one_external_platform_access_tenant_with_its_creator_as_owner() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (alice, alice_id) = c.sessions.sign_in("alice@acme.example");
    let o = org(&c, &alice, "Acme").await;
    // Platform Access holds the canonical EXTERNAL tenant; the organization id IS it.
    let t = c.pa.tenants.lock().unwrap()[&o].clone();
    assert_eq!(t.classification, "EXTERNAL");
    let audit = c.pa.audit.lock().unwrap().clone();
    assert_eq!(
        audit[0].0,
        format!("u-{}", alice_id.simple()),
        "the human actor is named"
    );
    let r = c
        .req(
            As::User(&alice),
            Method::GET,
            &format!("/v1/organizations/{o}"),
            None,
        )
        .await;
    assert_eq!(r.json["myRole"], "OWNER");
    assert_eq!(r.json["classification"], "EXTERNAL");
    assert!(
        r.headers.get("x-request-id").is_some(),
        "every response has a request id"
    );
    let list = c
        .req(As::User(&alice), Method::GET, "/v1/organizations", None)
        .await;
    conforms(&format!("{CTRL}OrganizationList"), &list.json);
    assert_eq!(list.json["items"][0]["id"], json!(o));
    // Activity: created, actor and correlation recorded.
    let act = c
        .req(
            As::User(&alice),
            Method::GET,
            &format!("/v1/organizations/{o}/activity"),
            None,
        )
        .await;
    conforms(&format!("{CTRL}ActivityList"), &act.json);
    assert_eq!(act.json["items"][0]["action"], "organization.created");
    assert_eq!(act.json["items"][0]["actorRef"], format!("user:{alice_id}"));
    // Update with optimistic concurrency.
    let up = c.req(As::User(&alice), Method::PATCH, &format!("/v1/organizations/{o}"),
        Some(json!({ "expectedVersion": 1, "displayName": "Acme Health", "supportEmail": "help@acme.example" }))).await;
    assert_eq!(up.status, StatusCode::OK, "{}", up.json);
    assert_eq!(up.json["version"], 2);
    let stale = c
        .req(
            As::User(&alice),
            Method::PATCH,
            &format!("/v1/organizations/{o}"),
            Some(json!({ "expectedVersion": 1, "displayName": "X" })),
        )
        .await;
    assert_eq!(stale.json["error"]["code"], "VERSION_CONFLICT");
    conforms(ERR, &stale.json);
    // Unknown fields (e.g. a classification) are refused.
    let bad = c
        .req(
            As::User(&alice),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "FP", "classification": "FIRST_PARTY" })),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn creation_is_idempotent_per_actor_and_key() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (alice, _) = c.sessions.sign_in("alice@acme.example");
    let body = json!({ "displayName": "Acme" });
    let k = [("idempotency-key", "create-acme-1")];
    let a = c
        .req_with(
            As::User(&alice),
            Method::POST,
            "/v1/organizations",
            Some(body.clone()),
            &k,
        )
        .await;
    let b = c
        .req_with(
            As::User(&alice),
            Method::POST,
            "/v1/organizations",
            Some(body.clone()),
            &k,
        )
        .await;
    assert_eq!(a.status, StatusCode::CREATED);
    assert_eq!(b.status, StatusCode::CREATED);
    assert_eq!(
        a.json["id"], b.json["id"],
        "the replay returns the original organization"
    );
    // Only the replay is marked, as on every CF Meet API.
    assert!(a.headers.get("idempotent-replayed").is_none());
    assert_eq!(b.headers["idempotent-replayed"], "true");
    assert_eq!(c.pa.tenants.lock().unwrap().len(), 1, "one tenant only");
    let reused = c
        .req_with(
            As::User(&alice),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Other" })),
            &k,
        )
        .await;
    assert_eq!(reused.json["error"]["code"], "IDEMPOTENCY_KEY_REUSED");
    // Another actor's identical key is independent.
    let (bob, _) = c.sessions.sign_in("bob@other.example");
    let other = c
        .req_with(
            As::User(&bob),
            Method::POST,
            "/v1/organizations",
            Some(body),
            &k,
        )
        .await;
    assert_ne!(other.json["id"], a.json["id"]);
}

#[tokio::test]
async fn invitations_are_one_time_bound_to_the_verified_email_and_expire() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (alice, _) = c.sessions.sign_in("alice@acme.example");
    let o = org(&c, &alice, "Acme").await;
    let inv = c
        .req(
            As::User(&alice),
            Method::POST,
            &format!("/v1/organizations/{o}/invitations"),
            Some(json!({ "email": "Dev@Acme.Example", "role": "DEVELOPER" })),
        )
        .await;
    let token = inv.json["token"].as_str().unwrap().to_string();
    assert_eq!(inv.json["email"], "dev@acme.example");
    // The token is never readable again and only its digest is stored.
    let list = c
        .req(
            As::User(&alice),
            Method::GET,
            &format!("/v1/organizations/{o}/invitations"),
            None,
        )
        .await;
    conforms(&format!("{CTRL}InvitationList"), &list.json);
    assert!(list.json["items"][0].get("token").is_none());
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_invitations WHERE token_digest = '{token}'"
        ))
        .await,
        0
    );
    // A duplicate pending invitation is refused.
    let dup = c
        .req(
            As::User(&alice),
            Method::POST,
            &format!("/v1/organizations/{o}/invitations"),
            Some(json!({ "email": "dev@acme.example", "role": "ADMIN" })),
        )
        .await;
    assert_eq!(dup.json["error"]["code"], "INVITATION_PENDING");
    // Another verified email cannot accept it.
    let (mallory, _) = c.sessions.sign_in("mallory@evil.example");
    let m = c
        .req(
            As::User(&mallory),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": token })),
        )
        .await;
    assert_eq!(m.json["error"]["code"], "INVITATION_EMAIL_MISMATCH");
    // The invited person accepts once.
    let (dev, _) = c.sessions.sign_in("dev@acme.example");
    let ok = c
        .req(
            As::User(&dev),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": token })),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.json);
    assert_eq!(ok.json["role"], "DEVELOPER");
    let again = c
        .req(
            As::User(&dev),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": token })),
        )
        .await;
    assert_eq!(again.json["error"]["code"], "INVITATION_INVALID");
    // Expired invitations are refused and become EXPIRED.
    let inv2 = c
        .req(
            As::User(&alice),
            Method::POST,
            &format!("/v1/organizations/{o}/invitations"),
            Some(json!({ "email": "late@acme.example", "role": "ANALYST" })),
        )
        .await;
    c.db.conn.execute_unprepared(&format!(
        "ALTER TABLE devplat_invitations DISABLE TRIGGER tr_devplat_invitations_guard; \
         UPDATE devplat_invitations SET created_at = now() - interval '9 days', expires_at = now() - interval '1 day' WHERE id = '{}'; \
         ALTER TABLE devplat_invitations ENABLE TRIGGER tr_devplat_invitations_guard;", inv2.json["id"].as_str().unwrap())).await.unwrap();
    let (late, _) = c.sessions.sign_in("late@acme.example");
    let r = c
        .req(
            As::User(&late),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": inv2.json["token"] })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "INVITATION_INVALID");
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_invitations WHERE id = '{}' AND status = 'EXPIRED'",
            inv2.json["id"].as_str().unwrap()
        ))
        .await,
        1
    );
    // Revocation.
    let inv3 = c
        .req(
            As::User(&alice),
            Method::POST,
            &format!("/v1/organizations/{o}/invitations"),
            Some(json!({ "email": "gone@acme.example", "role": "ANALYST" })),
        )
        .await;
    let rv = c
        .req(
            As::User(&alice),
            Method::POST,
            &format!(
                "/v1/organizations/{o}/invitations/{}/revoke",
                inv3.json["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
    assert_eq!(rv.json["status"], "REVOKED");
    let (gone, _) = c.sessions.sign_in("gone@acme.example");
    let r = c
        .req(
            As::User(&gone),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": inv3.json["token"] })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "INVITATION_INVALID");
    // An idempotent replay of an invitation never re-reveals its token.
    let k = [("idempotency-key", "inv-replay")];
    let first = c
        .req_with(
            As::User(&alice),
            Method::POST,
            &format!("/v1/organizations/{o}/invitations"),
            Some(json!({ "email": "re@acme.example", "role": "ANALYST" })),
            &k,
        )
        .await;
    assert!(first.json["token"].is_string());
    let replay = c
        .req_with(
            As::User(&alice),
            Method::POST,
            &format!("/v1/organizations/{o}/invitations"),
            Some(json!({ "email": "re@acme.example", "role": "ANALYST" })),
            &k,
        )
        .await;
    assert_eq!(replay.json["id"], first.json["id"]);
    assert!(replay.json.get("token").is_none());
    // Activity never records the email address.
    assert_eq!(c.count("SELECT count(*) FROM devplat_activity_events WHERE summary::text ILIKE '%@acme.example%'").await, 0);
}

#[tokio::test]
async fn the_section_50_role_matrix_is_enforced_and_denials_are_recorded() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let o = org(&c, &owner, "Acme").await;
    let (admin, _) = join(&c, &owner, o, "admin@acme.example", "ADMIN").await;
    let (dev, dev_member) = join(&c, &owner, o, "dev@acme.example", "DEVELOPER").await;
    let (analyst, _) = join(&c, &owner, o, "analyst@acme.example", "ANALYST").await;
    let (billing, _) = join(&c, &owner, o, "billing@acme.example", "BILLING").await;
    let projects = format!("/v1/organizations/{o}/projects");
    // Projects: OWNER / ADMIN create; DEVELOPER / ANALYST / BILLING cannot.
    let p = c
        .req(
            As::User(&admin),
            Method::POST,
            &projects,
            Some(json!({ "displayName": "Web" })),
        )
        .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.json);
    for who in [&dev, &analyst, &billing] {
        let r = c
            .req(
                As::User(who),
                Method::POST,
                &projects,
                Some(json!({ "displayName": "No" })),
            )
            .await;
        assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED", "{}", r.json);
    }
    // Everyone may read projects.
    for who in [&owner, &admin, &dev, &analyst, &billing] {
        assert_eq!(
            c.req(As::User(who), Method::GET, &projects, None)
                .await
                .status,
            StatusCode::OK
        );
    }
    // Membership: only the OWNER (ADMIN cannot invite or change roles).
    let r = c
        .req(
            As::User(&admin),
            Method::POST,
            &format!("/v1/organizations/{o}/invitations"),
            Some(json!({ "email": "x@acme.example", "role": "OWNER" })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    let r = c
        .req(
            As::User(&dev),
            Method::PATCH,
            &format!("/v1/organizations/{o}/members/{dev_member}"),
            Some(json!({ "role": "OWNER" })),
        )
        .await;
    assert_eq!(
        r.json["error"]["code"], "PERMISSION_DENIED",
        "no self-escalation"
    );
    // Activity: OWNER / ADMIN / ANALYST read; DEVELOPER / BILLING cannot.
    let act = format!("/v1/organizations/{o}/activity");
    for who in [&owner, &admin, &analyst] {
        assert_eq!(
            c.req(As::User(who), Method::GET, &act, None).await.status,
            StatusCode::OK
        );
    }
    for who in [&dev, &billing] {
        assert_eq!(
            c.req(As::User(who), Method::GET, &act, None).await.json["error"]["code"],
            "PERMISSION_DENIED"
        );
    }
    // Commercial: OWNER / ADMIN / BILLING read; billing account: OWNER / BILLING.
    let com = format!("/v1/organizations/{o}/commercial");
    assert_eq!(
        c.req(As::User(&billing), Method::GET, &com, None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        c.req(As::User(&dev), Method::GET, &com, None).await.json["error"]["code"],
        "PERMISSION_DENIED"
    );
    let ba = format!("/v1/organizations/{o}/billing-account");
    let r = c.req(As::User(&billing), Method::PUT, &ba, Some(json!({ "classification": "RETAIL", "currency": "NGN", "country": "NG", "externalBillingRef": "crm:acme-001" }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}BillingAccount"), &r.json);
    let r = c
        .req(
            As::User(&admin),
            Method::PUT,
            &ba,
            Some(json!({ "classification": "RETAIL" })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    // Customers never self-assign internal cost allocation, and payment data is refused.
    let r = c
        .req(
            As::User(&owner),
            Method::PUT,
            &ba,
            Some(json!({ "classification": "INTERNAL_COST_CENTER" })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "POLICY_VIOLATION");
    let r = c
        .req(
            As::User(&owner),
            Method::PUT,
            &ba,
            Some(json!({ "classification": "RETAIL", "cardNumber": "4242424242424242" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Denied mutations are recorded (outcome DENIED), reads are not.
    assert!(c.count(&format!("SELECT count(*) FROM devplat_activity_events WHERE tenant_id = '{o}' AND outcome = 'DENIED'")).await >= 5);
}

#[tokio::test]
async fn an_organization_always_keeps_an_owner_and_ownership_transfers_atomically() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let o = org(&c, &owner, "Acme").await;
    let members = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}/members"),
            None,
        )
        .await;
    conforms(&format!("{CTRL}MemberList"), &members.json);
    let owner_member = id(&members.json["items"][0]);
    // The last owner can neither be demoted nor removed nor leave.
    let r = c
        .req(
            As::User(&owner),
            Method::PATCH,
            &format!("/v1/organizations/{o}/members/{owner_member}"),
            Some(json!({ "role": "ADMIN" })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "LAST_OWNER");
    let r = c
        .req(
            As::User(&owner),
            Method::DELETE,
            &format!("/v1/organizations/{o}/members/{owner_member}"),
            None,
        )
        .await;
    assert_eq!(r.json["error"]["code"], "LAST_OWNER");
    // Transfer: the target becomes OWNER, the caller ADMIN.
    let (dev, dev_member) = join(&c, &owner, o, "dev@acme.example", "DEVELOPER").await;
    let t = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{o}/ownership-transfer"),
            Some(json!({ "memberId": dev_member })),
        )
        .await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.json);
    assert_eq!(t.json["role"], "OWNER");
    let me = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}"),
            None,
        )
        .await;
    assert_eq!(me.json["myRole"], "ADMIN");
    // The former owner lost membership authority; the new owner has it.
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{o}/ownership-transfer"),
            Some(json!({ "memberId": owner_member })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    let r = c
        .req(
            As::User(&dev),
            Method::DELETE,
            &format!("/v1/organizations/{o}/members/{owner_member}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    // Concurrent demotion of two co-owners never leaves zero owners.
    let (co, co_member) = join(&c, &dev, o, "co@acme.example", "OWNER").await;
    let new_owner_member = dev_member;
    let pa_path = format!("/v1/organizations/{o}/members/{co_member}");
    let pb_path = format!("/v1/organizations/{o}/members/{new_owner_member}");
    let a = c.req(
        As::User(&dev),
        Method::PATCH,
        &pa_path,
        Some(json!({ "role": "ADMIN" })),
    );
    let b = c.req(
        As::User(&co),
        Method::PATCH,
        &pb_path,
        Some(json!({ "role": "ADMIN" })),
    );
    let (a, b) = tokio::join!(a, b);
    let codes = [a.status, b.status];
    assert!(codes.contains(&StatusCode::OK), "{} {}", a.json, b.json);
    assert_eq!(c.count(&format!("SELECT count(*) FROM devplat_workspace_members WHERE tenant_id = '{o}' AND role = 'OWNER' AND status = 'ACTIVE'")).await, 1);
    // A member may leave (invited by whichever co-owner is still OWNER).
    let still_owner = if a.status == StatusCode::OK {
        &dev
    } else {
        &co
    };
    let (leaver, leaver_member) = join(&c, still_owner, o, "leaver@acme.example", "ANALYST").await;
    let r = c
        .req(
            As::User(&leaver),
            Method::DELETE,
            &format!("/v1/organizations/{o}/members/{leaver_member}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        c.req(
            As::User(&leaver),
            Method::GET,
            &format!("/v1/organizations/{o}"),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn tenants_are_isolated_without_existence_leaks() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (a, _) = c.sessions.sign_in("a@a.example");
    let (b, _) = c.sessions.sign_in("b@b.example");
    let oa = org(&c, &a, "A").await;
    let ob = org(&c, &b, "B").await;
    let pa = c
        .req(
            As::User(&a),
            Method::POST,
            &format!("/v1/organizations/{oa}/projects"),
            Some(json!({ "displayName": "A app" })),
        )
        .await;
    let pa_id = id(&pa.json);
    let b_members = c
        .req(
            As::User(&b),
            Method::GET,
            &format!("/v1/organizations/{ob}/members"),
            None,
        )
        .await;
    let b_member = id(&b_members.json["items"][0]);
    for (m, path) in [
        (Method::GET, format!("/v1/organizations/{oa}")),
        (Method::GET, format!("/v1/organizations/{oa}/members")),
        (Method::GET, format!("/v1/organizations/{oa}/activity")),
        (Method::GET, format!("/v1/projects/{pa_id}")),
        (Method::GET, format!("/v1/organizations/{}", Uuid::new_v4())),
    ] {
        let r = c.req(As::User(&b), m, &path, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(r.json["error"]["code"], "NOT_FOUND");
    }
    // Cross-tenant mutation attempts: same NOT_FOUND, nothing changes.
    let r = c
        .req(
            As::User(&b),
            Method::PATCH,
            &format!("/v1/projects/{pa_id}"),
            Some(json!({ "expectedVersion": 1, "displayName": "pwn" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // A member id of tenant B through tenant A's path (A's owner): NOT_FOUND (no escalation).
    let r = c
        .req(
            As::User(&a),
            Method::PATCH,
            &format!("/v1/organizations/{oa}/members/{b_member}"),
            Some(json!({ "role": "BILLING" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(c.count(&format!("SELECT count(*) FROM devplat_workspace_members WHERE id = '{b_member}' AND role = 'OWNER'")).await, 1);
}

#[tokio::test]
async fn projects_and_environments_delegate_to_platform_access_and_never_delete_domain_data() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let o = org(&c, &owner, "Acme").await;
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "Patient app", "labels": { "team": "mobile" } })),
        )
        .await;
    conforms(&format!("{CTRL}Project"), &p.json);
    let pid = id(&p.json);
    assert_eq!(
        c.pa.apps.lock().unwrap()[&pid].tenant,
        o,
        "project = Platform Access application"
    );
    // Environments: SANDBOX and TEST; PRODUCTION is the production-access workflow's.
    let envs = format!("/v1/projects/{pid}/environments");
    let sbx = c
        .req(
            As::User(&owner),
            Method::POST,
            &envs,
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    assert_eq!(sbx.status, StatusCode::CREATED, "{}", sbx.json);
    conforms(&format!("{CTRL}Environment"), &sbx.json);
    let sbx_id = id(&sbx.json);
    let test = c
        .req(
            As::User(&owner),
            Method::POST,
            &envs,
            Some(json!({ "kind": "TEST", "displayName": "QA" })),
        )
        .await;
    assert_eq!(test.status, StatusCode::CREATED);
    let prod = c
        .req(
            As::User(&owner),
            Method::POST,
            &envs,
            Some(json!({ "kind": "PRODUCTION" })),
        )
        .await;
    assert_eq!(prod.json["error"]["code"], "POLICY_VIOLATION");
    let list = c.req(As::User(&owner), Method::GET, &envs, None).await;
    conforms(&format!("{CTRL}EnvironmentList"), &list.json);
    assert_eq!(list.json["items"].as_array().unwrap().len(), 2);
    let one = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{sbx_id}"),
            None,
        )
        .await;
    assert_eq!(one.json["authorityStatus"], "ACTIVE");
    // Suspend / resume a non-production environment (Platform Access first).
    let s = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{sbx_id}/suspend"),
            None,
        )
        .await;
    assert_eq!(s.json["status"], "SUSPENDED");
    assert_eq!(c.pa.env_status(sbx_id), "SUSPENDED");
    let s = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{sbx_id}/resume"),
            None,
        )
        .await;
    assert_eq!(s.json["status"], "ACTIVE");
    assert_eq!(c.pa.env_status(sbx_id), "ACTIVE");
    // Entitlements: a SANDBOX is capped below the platform default.
    let e = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{sbx_id}/entitlements"),
            None,
        )
        .await;
    conforms(&format!("{CTRL}Entitlements"), &e.json);
    assert_eq!(
        e.json["limitingLevel"]["apiRequestsPerMinute"],
        "ENVIRONMENT"
    );
    // Metadata update and archive: the application is suspended, rows are kept.
    let up = c
        .req(
            As::User(&owner),
            Method::PATCH,
            &format!("/v1/projects/{pid}"),
            Some(
                json!({ "expectedVersion": 1, "description": "Telehealth", "supportEmail": null }),
            ),
        )
        .await;
    assert_eq!(up.json["description"], "Telehealth");
    let ar = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{pid}/archive"),
            None,
        )
        .await;
    assert_eq!(ar.json["status"], "ARCHIVED");
    assert_eq!(c.pa.app_status(pid), "SUSPENDED");
    assert_eq!(
        c.req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{pid}/resume"),
            None
        )
        .await
        .json["error"]["code"],
        "INVALID_STATE"
    );
    assert_eq!(
        c.req(
            As::User(&owner),
            Method::POST,
            &envs,
            Some(json!({ "kind": "SANDBOX" }))
        )
        .await
        .json["error"]["code"],
        "INVALID_STATE"
    );
    // Rows are never deleted (database-enforced).
    assert!(c
        .db
        .conn
        .execute_unprepared(&format!(
            "DELETE FROM devplat_project_profiles WHERE application_id = '{pid}'"
        ))
        .await
        .is_err());
    let act = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}/activity"),
            None,
        )
        .await;
    let archived = act.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "project.archived")
        .unwrap();
    assert_eq!(archived["summary"]["domainDataDeleted"], false);
}

#[tokio::test]
async fn lifecycle_states_are_enforced_by_the_control_plane_and_platform_access() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let o = org(&c, &owner, "Acme").await;
    let (_, analyst_member) = join(&c, &owner, o, "an@acme.example", "ANALYST").await;
    let state = |s: &str, r: &str| json!({ "state": s, "reason": r });
    // RESTRICTED: new projects disabled, membership hygiene still possible, machine access kept.
    let r = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/state"),
            Some(state("RESTRICTED", "BILLING_REVIEW")),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(c.pa.tenant_status(o), "ACTIVE");
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "No" })),
        )
        .await;
    assert_eq!(p.json["error"]["code"], "ORGANIZATION_RESTRICTED");
    let rm = c
        .req(
            As::User(&owner),
            Method::DELETE,
            &format!("/v1/organizations/{o}/members/{analyst_member}"),
            None,
        )
        .await;
    assert_eq!(rm.status, StatusCode::NO_CONTENT);
    // SUSPENDED: Platform Access suspends the tenant (machine auth refused); reads remain.
    let r = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/state"),
            Some(state("SUSPENDED", "ABUSE_REVIEW")),
        )
        .await;
    assert_eq!(r.json["state"], "SUSPENDED");
    assert_eq!(c.pa.tenant_status(o), "SUSPENDED");
    assert_eq!(
        c.req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}/activity"),
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    let u = c
        .req(
            As::User(&owner),
            Method::PATCH,
            &format!("/v1/organizations/{o}"),
            Some(json!({ "expectedVersion": 3, "displayName": "X" })),
        )
        .await;
    assert_eq!(u.json["error"]["code"], "ORGANIZATION_SUSPENDED");
    // Reactivation restores machine access after the state is recorded.
    let r = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/state"),
            Some(state("ACTIVE", "REVIEW_CLEARED")),
        )
        .await;
    assert_eq!(r.json["state"], "ACTIVE");
    assert_eq!(c.pa.tenant_status(o), "ACTIVE");
    // Invalid transitions and reasons are refused.
    let r = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/state"),
            Some(state("INVITED", "X")),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "INVALID_STATE");
    // The owner closes it: terminal, machine access withdrawn first.
    let cl = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{o}/close"),
            None,
        )
        .await;
    assert_eq!(cl.json["state"], "CLOSED");
    assert_eq!(c.pa.tenant_status(o), "SUSPENDED");
    let r = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/state"),
            Some(state("ACTIVE", "REOPEN")),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "INVALID_STATE");
    // Only operators reach the operator API.
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/state"),
            Some(state("ACTIVE", "X")),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn machine_credentials_use_platform_access_scopes_within_their_own_tenant_only() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let o = org(&c, &owner, "Acme").await;
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "Web" })),
        )
        .await;
    let pid = id(&p.json);
    let env = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{pid}/environments"),
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    let env_id = id(&env.json);
    let reader = c.pa.machine(
        env_id,
        &["organizations.read", "projects.read", "audit.read"],
    );
    let r = c
        .req(As::Machine(&reader), Method::GET, "/v1/organizations", None)
        .await;
    assert_eq!(r.json["items"][0]["id"], json!(o), "{}", r.json);
    assert_eq!(
        c.req(
            As::Machine(&reader),
            Method::GET,
            &format!("/v1/organizations/{o}/projects"),
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        c.req(
            As::Machine(&reader),
            Method::GET,
            &format!("/v1/organizations/{o}/activity"),
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    // Missing scope.
    let r = c
        .req(
            As::Machine(&reader),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "M" })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
    // Human-only operations are never available to machines, whatever their scopes.
    let r = c
        .req(
            As::Machine(&reader),
            Method::POST,
            &format!("/v1/organizations/{o}/invitations"),
            Some(json!({ "email": "x@y.z", "role": "OWNER" })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    let r = c
        .req(
            As::Machine(&reader),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "M" })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    // projects.manage creates projects; Platform Access audits the service account.
    let manager = c.pa.machine(env_id, &["projects.manage"]);
    let r = c
        .req(
            As::Machine(&manager),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "Automation" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let last = c.pa.audit.lock().unwrap().last().unwrap().0.clone();
    assert_eq!(
        last,
        format!("s-{}", manager.initiating_service_account_id().simple())
    );
    // A credential revoked after the gateway's check stops working on EVERY route at
    // once, even those whose handler would need no scope evaluation.
    let doomed =
        c.pa.machine(env_id, &["organizations.read", "projects.read"]);
    let path = format!("/v1/organizations/{o}/projects");
    let r = c.req(As::Machine(&doomed), Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::OK);
    c.pa.revoke(&doomed);
    for path in ["/v1/organizations".to_string(), path] {
        let r = c.req(As::Machine(&doomed), Method::GET, &path, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    // Another tenant's organization: NOT_FOUND.
    let (other, _) = c.sessions.sign_in("o@other.example");
    let ob = org(&c, &other, "Other").await;
    assert_eq!(
        c.req(
            As::Machine(&reader),
            Method::GET,
            &format!("/v1/organizations/{ob}"),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // A suspended tenant's credential is refused (SAFETY_ONLY is not authority here).
    c.operator(
        Method::POST,
        &format!("/internal/v1/operator/organizations/{o}/state"),
        Some(json!({ "state": "SUSPENDED", "reason": "TEST" })),
    )
    .await;
    assert_eq!(
        c.req(As::Machine(&reader), Method::GET, "/v1/organizations", None)
            .await
            .json["error"]["code"],
        "AUTHENTICATION_REQUIRED"
    );
}

#[tokio::test]
async fn unauthenticated_forged_and_unavailable_callers_fail_closed() {
    let Some(c) = Control::new().await else {
        return;
    };
    // Gateway token without session or delegated context.
    assert_eq!(
        c.req(As::Nobody, Method::GET, "/v1/organizations", None)
            .await
            .json["error"]["code"],
        "AUTHENTICATION_REQUIRED"
    );
    // A session the authority rejects.
    assert_eq!(
        c.req(As::User("forged"), Method::GET, "/v1/organizations", None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // No gateway token at all (a direct call): refused before anything else.
    let r = c
        .req_with(As::Nobody, Method::GET, "/v1/organizations", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Session authority down: 503, never anonymous access.
    let (alice, _) = c.sessions.sign_in("alice@acme.example");
    c.sessions
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        c.req(As::User(&alice), Method::GET, "/v1/organizations", None)
            .await
            .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    c.sessions
        .down
        .store(false, std::sync::atomic::Ordering::SeqCst);
    // Platform Access down: creation fails closed and leaves nothing behind.
    c.pa.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = c
        .req(
            As::User(&alice),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Down" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_organizations").await,
        0
    );
}

#[tokio::test]
async fn commercial_plans_are_versioned_and_assignments_keep_an_immutable_history() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let o = org(&c, &owner, "Acme").await;
    let plan = |code: &str, rpm: u64| {
        json!({ "code": code, "displayName": "Starter", "entitlementProfileRef": "profile.starter",
        "entitlements": { "apiRequestsPerMinute": rpm, "apiRequestsPerDay": 50000, "concurrentCalls": 10 } })
    };
    let v1 = c
        .operator(
            Method::POST,
            "/internal/v1/operator/commercial-plans",
            Some(plan("starter", 300)),
        )
        .await;
    assert_eq!(v1.status, StatusCode::CREATED, "{}", v1.json);
    conforms(&format!("{CTRL}CommercialPlan"), &v1.json);
    let v2 = c
        .operator(
            Method::POST,
            "/internal/v1/operator/commercial-plans",
            Some(plan("starter", 900)),
        )
        .await;
    assert_eq!(v2.json["version"], 2);
    let strict = c
        .operator(
            Method::POST,
            "/internal/v1/operator/commercial-plans",
            Some(plan("project-limited", 50)),
        )
        .await;
    // Prices are not a plan attribute.
    let bad = c.operator(Method::POST, "/internal/v1/operator/commercial-plans", Some(json!({ "code": "x1", "displayName": "X", "entitlementProfileRef": "p.x", "entitlements": { "pricePerMinute": 1 } }))).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    // Plan versions are immutable in the database.
    assert!(c
        .db
        .conn
        .execute_unprepared(&format!(
            "UPDATE devplat_commercial_plans SET entitlements = '{{}}' WHERE id = '{}'",
            v1.json["id"].as_str().unwrap()
        ))
        .await
        .is_err());
    // Assign v1, then v2 (supersedes).
    let a1 = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/plan-assignments"),
            Some(json!({ "planId": v1.json["id"], "source": "SELF_SERVICE" })),
        )
        .await;
    assert_eq!(a1.status, StatusCode::CREATED, "{}", a1.json);
    let a2 = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/plan-assignments"),
            Some(json!({ "planId": v2.json["id"], "source": "CONTRACT" })),
        )
        .await;
    assert_eq!(a2.status, StatusCode::CREATED);
    let hist = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}/plan-assignments"),
            None,
        )
        .await;
    conforms(&format!("{CTRL}PlanAssignmentList"), &hist.json);
    let items = hist.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["supersededBy"], a2.json["id"]);
    assert!(items[0]["effectiveUntil"].is_string());
    let com = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}/commercial"),
            None,
        )
        .await;
    conforms(&format!("{CTRL}OrganizationCommercial"), &com.json);
    assert_eq!(com.json["planAssignment"]["planVersion"], 2);
    // A stricter project plan wins; the environment cap wins over both in SANDBOX.
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "Web" })),
        )
        .await;
    let pid = id(&p.json);
    c.operator(
        Method::POST,
        &format!("/internal/v1/operator/organizations/{o}/plan-assignments"),
        Some(json!({ "planId": strict.json["id"], "projectId": pid, "source": "CONTRACT" })),
    )
    .await;
    let test = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{pid}/environments"),
            Some(json!({ "kind": "TEST" })),
        )
        .await;
    let e = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!(
                "/v1/environments/{}/entitlements",
                test.json["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
    assert_eq!(e.json["apiRequestsPerMinute"], 50);
    assert_eq!(
        e.json["limitingLevel"]["apiRequestsPerMinute"],
        "PROJECT_PLAN"
    );
    assert_eq!(e.json["apiRequestsPerDay"], 50000);
    assert_eq!(e.json["domainEntitlements"]["concurrentCalls"], 10);
    // The gateway reads the same snapshot internally.
    let plans = c
        .req(As::User(&owner), Method::GET, "/v1/commercial-plans", None)
        .await;
    conforms(&format!("{CTRL}CommercialPlanList"), &plans.json);
    // First-party internal cost allocation is operator-only.
    let cc = c
        .operator(
            Method::PUT,
            &format!("/internal/v1/operator/organizations/{o}/cost-center"),
            Some(json!({ "costCenterRef": "cf.platform" })),
        )
        .await;
    assert_eq!(cc.json["classification"], "INTERNAL_COST_CENTER");
    let r = c
        .req(
            As::User(&owner),
            Method::PUT,
            &format!("/v1/organizations/{o}/billing-account"),
            Some(json!({ "classification": "RETAIL" })),
        )
        .await;
    assert_eq!(
        r.json["error"]["code"], "POLICY_VIOLATION",
        "customers cannot undo internal allocation"
    );
}

#[tokio::test]
async fn first_party_organizations_are_explicit_operator_registrations() {
    let Some(c) = Control::new().await else {
        return;
    };
    let tenant = c.pa.first_party_tenant();
    let r = c.operator(Method::POST, "/internal/v1/operator/organizations",
        Some(json!({ "tenantId": tenant, "displayName": "Care product", "ownerEmail": "lead@cf.example" }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    conforms(&format!("{CTRL}FirstPartyRegistration"), &r.json);
    assert_eq!(r.json["organization"]["state"], "INVITED");
    // Not usable before the owner accepts.
    let (lead, _) = c.sessions.sign_in("lead@cf.example");
    let ok = c
        .req(
            As::User(&lead),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": r.json["ownerInvitation"]["token"] })),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.json);
    let o = c
        .req(
            As::User(&lead),
            Method::GET,
            &format!("/v1/organizations/{tenant}"),
            None,
        )
        .await;
    assert_eq!(o.json["state"], "ACTIVE");
    assert_eq!(o.json["classification"], "FIRST_PARTY");
    // An EXTERNAL tenant can never be registered as first party.
    let (ext, _) = c.sessions.sign_in("e@ext.example");
    let external = org(&c, &ext, "Ext").await;
    let r = c.operator(Method::POST, "/internal/v1/operator/organizations",
        Some(json!({ "tenantId": external, "displayName": "Fake FP", "ownerEmail": "e@ext.example" }))).await;
    assert_eq!(r.json["error"]["code"], "POLICY_VIOLATION");
    let act = c
        .req(
            As::User(&lead),
            Method::GET,
            &format!("/v1/organizations/{tenant}/activity"),
            None,
        )
        .await;
    assert!(act.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["action"] == "organization.first_party_registered"
            && e["actorKind"] == "OPERATOR"));
}

#[tokio::test]
async fn activity_history_is_immutable_and_paginated() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let o = org(&c, &owner, "Acme").await;
    for i in 0..5 {
        c.req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": format!("P{i}") })),
        )
        .await;
    }
    assert!(c
        .db
        .conn
        .execute_unprepared("UPDATE devplat_activity_events SET outcome = 'FAILED'")
        .await
        .is_err());
    assert!(c
        .db
        .conn
        .execute_unprepared("DELETE FROM devplat_activity_events")
        .await
        .is_err());
    assert!(c
        .db
        .conn
        .execute_unprepared("TRUNCATE devplat_activity_events")
        .await
        .is_err());
    let p1 = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}/activity?limit=4"),
            None,
        )
        .await;
    assert_eq!(p1.json["items"].as_array().unwrap().len(), 4);
    let cursor = p1.json["nextCursor"].as_str().unwrap();
    let p2 = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}/activity?limit=4&cursor={cursor}"),
            None,
        )
        .await;
    assert_eq!(p2.json["items"].as_array().unwrap().len(), 2);
    assert!(p2.json["nextCursor"].is_null());
    let bad = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}/activity?limit=500"),
            None,
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    // A secret-looking summary key is refused by the database itself.
    let r = c.db.conn.execute_unprepared(&format!(
        "INSERT INTO devplat_activity_events (id, tenant_id, actor_kind, actor_ref, action, target_type, target_id, outcome, request_id, correlation_id, summary, occurred_at) \
         VALUES ('{}', '{o}', 'USER', 'user:x', 'x.y', 'X', 'x', 'SUCCEEDED', 'r', 'c', '{{\"signingSecret\":\"whsec\"}}', now())", Uuid::new_v4())).await;
    assert!(r.is_err());
}

#[tokio::test]
async fn api_request_usage_batches_are_counted_exactly_once() {
    let Some(c) = Control::new().await else {
        return;
    };
    let env = Uuid::new_v4();
    let gw = |method: Method, path: String, body: Option<Value>| {
        let signed = c
            .peers
            .gateway
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::developer_platform(),
            ))
            .unwrap();
        let mut headers = HeaderMap::new();
        signed.apply_to(&mut headers);
        c.raw(method, path, headers, body)
    };
    let batch = Uuid::new_v4();
    let body = json!({ "batchId": batch, "entries": [
        { "environmentId": env, "day": "2026-10-23", "count": 7 },
        { "environmentId": env, "day": "2026-10-22", "count": 3 } ] });
    let r = gw(
        Method::POST,
        "/internal/v1/usage/api-requests".into(),
        Some(body.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["outcome"], "RECORDED");
    // A replay after an unknown outcome is acknowledged, never counted twice.
    let r = gw(
        Method::POST,
        "/internal/v1/usage/api-requests".into(),
        Some(body),
    )
    .await;
    assert_eq!(r.json["outcome"], "ALREADY_RECORDED");
    let more = json!({ "batchId": Uuid::new_v4(), "entries": [{ "environmentId": env, "day": "2026-10-23", "count": 5 }] });
    gw(
        Method::POST,
        "/internal/v1/usage/api-requests".into(),
        Some(more),
    )
    .await;
    let d = gw(
        Method::GET,
        format!("/internal/v1/usage/api-requests/{env}/2026-10-23"),
        None,
    )
    .await;
    assert_eq!(d.json["count"], 12);
    let d = gw(
        Method::GET,
        format!("/internal/v1/usage/api-requests/{env}/2026-10-21"),
        None,
    )
    .await;
    assert_eq!(d.json["count"], 0);
    // Invalid batches are refused whole.
    for bad in [
        json!({ "batchId": Uuid::new_v4(), "entries": [] }),
        json!({ "batchId": Uuid::new_v4(), "entries": [{ "environmentId": env, "day": "2026-13-01", "count": 1 }] }),
        json!({ "batchId": Uuid::new_v4(), "entries": [{ "environmentId": env, "day": "2026-10-23", "count": 0 }] }),
        json!({ "batchId": Uuid::new_v4(), "entries": [{ "environmentId": env, "day": "2026-10-23", "count": 1, "path": "/v1/x" }] }),
    ] {
        let r = gw(
            Method::POST,
            "/internal/v1/usage/api-requests".into(),
            Some(bad),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    }
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_api_usage_batches")
            .await,
        2
    );
    // Only the gateway's developer-usage.record grant reaches these routes.
    let r = c
        .operator(
            Method::GET,
            &format!("/internal/v1/usage/api-requests/{env}/2026-10-23"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = c
        .raw(
            Method::GET,
            format!("/internal/v1/usage/api-requests/{env}/2026-10-23"),
            HeaderMap::new(),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Counters never shrink and are never deleted.
    let db = &c.db.conn;
    let shrink = db
        .execute(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("UPDATE devplat_api_usage_daily SET request_count = 1 WHERE environment_id = '{env}'"),
        ))
        .await;
    assert!(shrink.is_err());
    let delete = db
        .execute(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "DELETE FROM devplat_api_usage_batches".to_string(),
        ))
        .await;
    assert!(delete.is_err());
}

#[tokio::test]
async fn reference_only_environments_never_receive_production_limits_by_default() {
    let Some(c) = Control::new().await else {
        return;
    };
    let env = Uuid::new_v4();
    c.db.conn
        .execute_unprepared(&format!(
            "INSERT INTO devplat_environment_refs (id, tenant_id, application_id, environment_id, first_seen_at, last_seen_at) \
             VALUES ('{}', '{}', '{}', '{env}', now(), now())",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4()
        ))
        .await
        .unwrap();
    let get = |q: &'static str| {
        let signed = c
            .peers
            .gateway
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::developer_platform(),
            ))
            .unwrap();
        let mut headers = HeaderMap::new();
        signed.apply_to(&mut headers);
        c.raw(
            Method::GET,
            format!("/internal/v1/entitlements/environments/{env}{q}"),
            headers,
            None,
        )
    };
    let r = get("").await;
    assert_eq!(r.json["apiRequestsPerMinute"], 120, "{}", r.json);
    assert_eq!(
        r.json["limitingLevel"]["apiRequestsPerMinute"],
        "ENVIRONMENT"
    );
    let r = get("?kind=PRODUCTION").await;
    assert_eq!(r.json["apiRequestsPerMinute"], 600);
    assert_eq!(r.json["environmentKind"], "PRODUCTION");
    let r = get("?kind=TEST").await;
    assert_eq!(r.json["apiRequestsPerMinute"], 300);
    assert_eq!(get("?kind=STAGING").await.status, StatusCode::BAD_REQUEST);
    assert_eq!(get("?other=1").await.status, StatusCode::BAD_REQUEST);
}
