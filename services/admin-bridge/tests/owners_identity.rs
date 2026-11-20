//! Identity reads and the Platform Access partner reads through the full chain (real
//! mTLS, signed HQ request) against a fake Platform Access that records every call: paths,
//! query mapping (kind filter, window, cursor), the operator attribution, strict
//! translation and minimization (no secret material, no external reference).

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    owners::{OwnerError, OwnerPort},
};
use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};

const TENANT: &str = "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a";
const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const T0: &str = "2026-11-01T10:00:00.123456+00:00";
const REF: &str = "ada@partner.example";

/// (owner, path, query, operator) of one owner call.
type Recorded = (Owner, String, Vec<(String, String)>, String);

#[derive(Default)]
struct FakePlatformAccess {
    calls: Mutex<Vec<Recorded>>,
}

fn page(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "nextCursor": next})
}

fn principal() -> Value {
    // An owner that leaked the reference would be caught by the strict translation:
    // unknown fields are simply not copied.
    json!({"id": ID, "tenantId": TENANT, "applicationId": ID, "environmentId": ID,
           "kind": "EXTERNAL_APPLICATION_PRINCIPAL", "linkedServiceAccountId": null, "status": "ACTIVE",
           "tombstonedAt": null, "revision": 1, "createdAt": T0, "updatedAt": T0, "externalReference": REF})
}

#[async_trait]
impl OwnerPort for FakePlatformAccess {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        operator: &str,
    ) -> Result<Value, OwnerError> {
        self.calls.lock().unwrap().push((
            owner,
            path.to_string(),
            query
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            operator.to_string(),
        ));
        let p = path.trim_start_matches("/internal/v1/operator");
        Ok(match p {
            "/tenants" => page(
                vec![
                    json!({"id": TENANT, "slug": "acme", "displayName": "Acme", "classification": "EXTERNAL",
                    "status": "ACTIVE", "billingCurrency": "USD", "revision": 2, "createdAt": T0, "updatedAt": T0}),
                ],
                json!(TENANT),
            ),
            x if x.ends_with("/applications") => page(
                vec![
                    json!({"id": ID, "tenantId": TENANT, "slug": "app", "displayName": "App", "status": "ACTIVE",
                    "revision": 1, "createdAt": T0, "updatedAt": T0}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/environments") => page(
                vec![
                    json!({"id": ID, "applicationId": ID, "kind": "SANDBOX", "status": "ACTIVE",
                    "liveTelecomAllowed": false, "revision": 1, "createdAt": T0, "updatedAt": T0}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/service-accounts") => page(
                vec![
                    json!({"id": ID, "applicationId": ID, "environmentId": ID, "name": "backend", "status": "ACTIVE",
                    "revokedAt": null, "revision": 1, "createdAt": T0, "updatedAt": T0, "description": "call Ada on 0803"}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/scope-grants") => page(
                vec![
                    json!({"applicationId": ID, "scope": "calls.read", "allowedEnvironmentKinds": null,
                    "grantBasis": "STANDARD", "grantedAt": T0}),
                ],
                json!(format!("{ID}:calls.read")),
            ),
            x if x.ends_with("/credentials") => page(
                vec![
                    json!({"id": ID, "publicId": "k_0123456789abcdef", "lastFour": "a1b2", "applicationId": ID,
                    "environmentId": ID, "serviceAccountId": ID, "status": "ACTIVE", "scopes": ["calls.read"],
                    "notBefore": T0, "expiresAt": T0, "revokedAt": null, "supersededAt": null, "suspendedAt": null,
                    "suspensionReason": null, "lastUsedAt": T0, "revision": 1, "createdAt": T0,
                    "verifier": "deadbeef", "label": "prod key for Ada"}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/principals") || x == "/principals" => {
                page(vec![principal()], Value::Null)
            }
            x if x.ends_with("/origins") => page(
                vec![
                    json!({"environmentId": ID, "applicationId": ID, "environmentKind": "PRODUCTION",
                    "origins": ["https://app.acme.example"], "revision": 3, "updatedAt": T0}),
                ],
                Value::Null,
            ),
            "/oauth-apps" => page(
                vec![
                    json!({"applicationId": ID, "tenantId": TENANT, "name": "Notes bot", "distribution": "PUBLIC",
                    "status": "PUBLISHED", "userScopes": ["meetings.read"], "botScopes": [], "redirectUriCount": 2,
                    "revision": 1, "createdAt": T0, "updatedAt": T0, "redirectUris": ["https://secret.example/cb"]}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/grants") => page(
                vec![
                    json!({"id": ID, "target": "USER", "accountId": ID, "organizationId": null,
                    "installedByAccountId": ID, "environmentId": ID, "principalId": ID, "scopes": ["meetings.read"],
                    "status": "ACTIVE", "revokedAt": null, "revokeReason": null, "createdAt": T0, "updatedAt": T0}),
                ],
                Value::Null,
            ),
            "/audit" => page(
                vec![
                    json!({"id": ID, "actor": "hq:staff-0002", "correlationId": "op_0123456789abcdef", "action": "CREDENTIAL_REVOKED",
                    "targetKind": "CREDENTIAL", "targetId": ID, "tenantId": TENANT, "applicationId": null,
                    "environmentId": null, "occurredAt": T0, "detail": {"reason": "leaked"}}),
                ],
                json!("1790000000123456_6f1c2a3b4d5e4f608a7b9c0d1e2f3a4b"),
            ),
            _ => return Err(OwnerError::NotFound),
        })
    }
}

async fn harness() -> (Harness, Arc<FakePlatformAccess>) {
    let fake = Arc::new(FakePlatformAccess::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    (h, fake)
}

fn last(fake: &FakePlatformAccess) -> Recorded {
    fake.calls
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("an owner call")
}

fn query_of(r: &Recorded) -> Vec<(&str, &str)> {
    r.2.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

const ADMIN: &[&str] = &["cfmeet.security_admin"];

#[tokio::test]
async fn identity_tenants_search_translates() {
    let (h, fake) = harness().await;
    let c = call(
        "/identity/tenants",
        &format!("{}&limit=10&q=acme", scoped("support-review")),
        "identity.tenants.read",
        ADMIN,
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let r = last(&fake);
    assert_eq!(
        (r.0, r.1.as_str(), r.3.as_str()),
        (
            Owner::PlatformAccess,
            "/internal/v1/operator/tenants",
            "hq:staff-0001"
        )
    );
    assert_eq!(query_of(&r), [("q", "acme"), ("limit", "10")]);
    assert_eq!(v["result"]["items"][0]["tenant_id"], TENANT);
    assert_eq!(
        v["result"]["items"][0]["created_at"],
        "2026-11-01T10:00:00Z"
    );
    assert_eq!(v["result"]["next_cursor"], TENANT);
}

#[tokio::test]
async fn identity_tenant_lists_translate_strictly() {
    let (h, fake) = harness().await;
    for (cap, perm, path, key) in [
        (
            "applications",
            "identity.applications.read",
            "applications",
            "application_id",
        ),
        (
            "environments",
            "identity.environments.read",
            "environments",
            "environment_id",
        ),
        (
            "service-accounts",
            "identity.service-accounts.read",
            "service-accounts",
            "service_account_id",
        ),
    ] {
        let c = call(
            &format!("/identity/tenants/{TENANT}/{cap}"),
            &scoped("support-review"),
            perm,
            ADMIN,
        );
        let (status, v) = send(&h, &c).await;
        assert_eq!(status, 200, "{cap}: {v}");
        assert_eq!(
            last(&fake).1,
            format!("/internal/v1/operator/tenants/{TENANT}/{path}")
        );
        assert_eq!(v["result"]["items"][0][key], ID, "{cap}");
        assert!(
            !v.to_string().contains("0803"),
            "free-text descriptions are never relayed"
        );
    }
    // A non-UUID tenant names nothing and never reaches the owner.
    let calls = fake.calls.lock().unwrap().len();
    let c = call(
        "/identity/tenants/not-a-uuid/applications",
        &scoped("support-review"),
        "identity.applications.read",
        ADMIN,
    );
    assert_eq!(code(&send(&h, &c).await.1), "RESOURCE_NOT_FOUND");
    assert_eq!(fake.calls.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn identity_scope_grants_check_the_cursor() {
    let (h, fake) = harness().await;
    let ok = call(
        &format!("/identity/tenants/{TENANT}/scope-grants"),
        &format!("{}&cursor={ID}:calls.read", scoped("support-review")),
        "identity.scope-grants.read",
        ADMIN,
    );
    let (status, v) = send(&h, &ok).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        query_of(&last(&fake)),
        [("cursor", format!("{ID}:calls.read").as_str())]
    );
    assert_eq!(v["result"]["items"][0]["scope"], "calls.read");
    assert!(v["result"]["items"][0]["allowed_environment_kinds"].is_null());
    let calls = fake.calls.lock().unwrap().len();
    let bad = call(
        &format!("/identity/tenants/{TENANT}/scope-grants"),
        &format!("{}&cursor=no-colon", scoped("support-review")),
        "identity.scope-grants.read",
        ADMIN,
    );
    assert_eq!(code(&send(&h, &bad).await.1), "QUERY_INVALID");
    assert_eq!(
        fake.calls.lock().unwrap().len(),
        calls,
        "a bad cursor never reaches the owner"
    );
}

#[tokio::test]
async fn identity_keys_never_carry_secret_material() {
    let (h, _) = harness().await;
    let c = call(
        &format!("/identity/tenants/{TENANT}/keys"),
        &scoped("support-review"),
        "identity.keys.read",
        ADMIN,
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let k = &v["result"]["items"][0];
    assert_eq!(k["last4"], "a1b2");
    assert_eq!(k["public_id"], "k_0123456789abcdef");
    let text = v.to_string();
    assert!(
        !text.contains("deadbeef") && !text.contains("prod key for Ada"),
        "no verifier, no label"
    );
}

#[tokio::test]
async fn identity_records_never_echo_the_reference() {
    let (h, fake) = harness().await;
    let c = call(
        "/identity/records",
        &format!("{}&q=ada%40partner.example", scoped("support-review")),
        "identity.records.read",
        ADMIN,
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let r = last(&fake);
    assert_eq!(r.1, "/internal/v1/operator/principals");
    assert_eq!(query_of(&r), [("q", REF)]);
    assert_eq!(v["result"]["items"][0]["principal_id"], ID);
    assert!(
        !v.to_string().contains(REF),
        "the external reference is never relayed"
    );
}

#[tokio::test]
async fn identity_partner_users_filter_the_kind() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/identity/tenants/{TENANT}/partner-users"),
        &format!("{}&q={ID}", scoped("support-review")),
        "identity.partner-users.read",
        ADMIN,
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let r = last(&fake);
    assert_eq!(
        r.1,
        format!("/internal/v1/operator/tenants/{TENANT}/principals")
    );
    assert_eq!(
        query_of(&r),
        [("kind", "EXTERNAL_APPLICATION_PRINCIPAL"), ("q", ID)]
    );
    assert!(!v.to_string().contains(REF));
}

#[tokio::test]
async fn identity_oauth_apps_and_grants_translate() {
    let (h, fake) = harness().await;
    let c = call(
        "/identity/oauth-apps",
        &format!("{}&q=notes", scoped("support-review")),
        "identity.oauth-apps.read",
        ADMIN,
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["items"][0]["redirect_uri_count"], 2);
    assert!(
        !v.to_string().contains("secret.example"),
        "redirect URIs are never relayed"
    );
    let g = call(
        &format!("/identity/oauth-apps/{ID}/grants"),
        &scoped("support-review"),
        "identity.app-grants.read",
        ADMIN,
    );
    let (status, v) = send(&h, &g).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        last(&fake).1,
        format!("/internal/v1/operator/oauth-apps/{ID}/grants")
    );
    assert_eq!(v["result"]["items"][0]["grant_id"], ID);
}

#[tokio::test]
async fn identity_audit_maps_the_window() {
    let (h, fake) = harness().await;
    let c = call(
        "/identity/audit",
        &format!(
            "{}&limit=5&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z",
            scoped("support-review")
        ),
        "identity.audit.read",
        ADMIN,
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        query_of(&last(&fake)),
        [
            ("since", "2026-11-01T00:00:00Z"),
            ("until", "2026-11-10T00:00:00Z"),
            ("limit", "5")
        ]
    );
    assert!(v["result"]["items"][0].get("detail").is_none());
    assert!(!v.to_string().contains("leaked"));
    assert_eq!(
        v["result"]["next_cursor"],
        "1790000000123456_6f1c2a3b4d5e4f608a7b9c0d1e2f3a4b"
    );
    let bad = call(
        "/identity/audit",
        &format!(
            "{}&cursor=abc&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z",
            scoped("support-review")
        ),
        "identity.audit.read",
        ADMIN,
    );
    assert_eq!(code(&send(&h, &bad).await.1), "QUERY_INVALID");
}

#[tokio::test]
async fn partners_identity_reads_use_the_workspace_tenant() {
    let (h, fake) = harness().await;
    let ops = &["cfmeet.partner_ops"];
    for (cap, perm, owner_path, key) in [
        ("keys", "partners.keys.read", "credentials", "credential_id"),
        (
            "service-accounts",
            "partners.service-accounts.read",
            "service-accounts",
            "service_account_id",
        ),
        (
            "websites",
            "partners.websites.read",
            "origins",
            "environment_id",
        ),
    ] {
        let c = call(
            &format!("/partners/workspaces/{TENANT}/{cap}"),
            &scoped("support-review"),
            perm,
            ops,
        );
        let (status, v) = send(&h, &c).await;
        assert_eq!(status, 200, "{cap}: {v}");
        let r = last(&fake);
        assert_eq!(
            (r.0, r.1.clone()),
            (
                Owner::PlatformAccess,
                format!("/internal/v1/operator/tenants/{TENANT}/{owner_path}")
            )
        );
        assert_eq!(v["result"]["items"][0][key], ID, "{cap}");
    }
    let c = call(
        &format!("/partners/workspaces/{TENANT}/users"),
        &format!("{}&q={ID}", scoped("support-review")),
        "partners.users.read",
        ops,
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        query_of(&last(&fake)),
        [("kind", "EXTERNAL_APPLICATION_PRINCIPAL"), ("q", ID)]
    );
    assert!(!v.to_string().contains(REF));
}
