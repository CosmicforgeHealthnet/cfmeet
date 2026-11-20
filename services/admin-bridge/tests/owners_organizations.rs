//! Organizations reads through the full chain (real mTLS, signed HQ request) against a
//! fake Organizations owner that records every call: path and query mapping, the
//! operator attribution, translation into the closed schemas and minimization.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    owners::{OwnerError, OwnerPort},
};
use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};

const ORG: &str = "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a";
const USER: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

/// (owner, path, query, operator) of one owner call.
type Recorded = (Owner, String, Vec<(String, String)>, String);

#[derive(Default)]
struct FakeOrganizations {
    calls: Mutex<Vec<Recorded>>,
    fail: Mutex<Option<OwnerError>>,
}

fn org_json() -> Value {
    json!({
        "id": ORG, "name": "Acme Clinics", "slug": "acme", "lifecycleStatus": "ACTIVE",
        "provisioningState": "READY", "creationSource": "SELF_SERVE", "createdBy": USER,
        "createdAt": "2026-11-01T10:00:00Z", "updatedAt": "2026-11-02T10:00:00.123456Z", "version": 4,
        "archivedAt": null, "memberCount": 12,
        "profile": {
            "organizationType": "HEALTHCARE", "industry": "clinics", "countryCode": "NG",
            "operatingAddress": "12 Secret Street", "publicContactEmail": "hello@acme.example",
            "legalRegistration": {"registeredName": "Acme Ltd", "registrationNumber": "RC-999", "verification": "VERIFIED"}
        }
    })
}

fn audit_json() -> Value {
    json!({
        "id": USER, "seq": 41, "organizationId": ORG, "action": "membership.updated",
        "actor": {"kind": "USER", "userId": USER, "serviceId": "cfmeet.meeting-service"},
        "targetType": "membership", "targetId": null,
        "summary": {"email": "someone@acme.example", "role": "ADMIN"},
        "requestId": "req_x", "correlationId": "corr_y", "occurredAt": "2026-11-03T08:00:00Z"
    })
}

#[async_trait]
impl OwnerPort for FakeOrganizations {
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
        if let Some(e) = *self.fail.lock().unwrap() {
            return Err(e);
        }
        Ok(match path {
            "/internal/v1/admin/organizations" => json!({"items": [org_json()], "nextCursor": ORG}),
            p if p == format!("/internal/v1/operator/organizations/{ORG}") => org_json(),
            p if p.ends_with("/audit-events") => json!({"items": [audit_json()], "nextCursor": 40}),
            p if p.ends_with("/members") => json!({"items": [{
                "id": ORG, "organizationId": ORG, "userId": USER, "role": "OWNER", "status": "ACTIVE",
                "joinedVia": "CREATION", "createdBy": USER, "createdAt": "2026-11-01T10:00:00Z",
                "updatedAt": "2026-11-01T10:00:00Z", "removedAt": null, "version": 1
            }], "nextCursor": null}),
            p if p.ends_with("/teams") => json!({"items": [{
                "id": USER, "organizationId": ORG, "name": "Front desk", "description": "private note: call Ada on 0803",
                "parentTeamId": null, "status": "ACTIVE", "createdBy": USER, "createdAt": "2026-11-01T10:00:00Z",
                "updatedAt": "2026-11-01T10:00:00Z", "archivedAt": null, "version": 2, "memberCount": 3
            }], "nextCursor": null}),
            p if p.ends_with("/service-bindings/calling") => json!({
                "organizationId": ORG, "capability": "primary", "service": "calling", "serviceAccountId": "sa_123",
                "credentialId": "cred_456", "version": 3, "boundBy": USER, "source": "AUTOMATIC",
                "verifiedAt": "2026-11-02T00:00:00Z", "credentialExpiresAt": null,
                "createdAt": "2026-11-01T10:00:00Z", "updatedAt": "2026-11-02T00:00:00Z"
            }),
            _ => return Err(OwnerError::NotFound),
        })
    }
}

async fn harness() -> (Harness, Arc<FakeOrganizations>) {
    let fake = Arc::new(FakeOrganizations::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    (h, fake)
}

fn last_call(fake: &FakeOrganizations) -> Recorded {
    fake.calls
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("an owner call")
}

#[tokio::test]
async fn organizations_search_translates_and_minimizes() {
    let (h, fake) = harness().await;
    let c = call(
        "/organizations/organizations",
        &format!("{}&limit=10&q=acme", scoped("support-review")),
        "organizations.organizations.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (owner, path, query, operator) = last_call(&fake);
    assert_eq!(
        (owner, path.as_str(), operator.as_str()),
        (
            Owner::Organizations,
            "/internal/v1/admin/organizations",
            "hq:staff-0001"
        )
    );
    assert_eq!(
        query,
        vec![
            ("q".to_string(), "acme".to_string()),
            ("limit".to_string(), "10".to_string())
        ]
    );
    let item = &v["result"]["items"][0];
    assert_eq!(item["organization_id"], ORG);
    assert_eq!(
        item["updated_at"], "2026-11-02T10:00:00Z",
        "times are normalized to UTC seconds"
    );
    assert_eq!(v["result"]["next_cursor"], ORG);
    assert!(item.get("profile").is_none() && item.get("createdBy").is_none());
}

#[tokio::test]
async fn organizations_detail_never_relays_contact_details() {
    let (h, _) = harness().await;
    let c = call(
        &format!("/organizations/organizations/{ORG}"),
        &scoped("support-review"),
        "organizations.organizations.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"];
    assert_eq!(r["legal_verification"], "VERIFIED");
    assert_eq!(r["country_code"], "NG");
    let text = r.to_string();
    for secret in ["Secret Street", "hello@acme.example", "RC-999", "Acme Ltd"] {
        assert!(!text.contains(secret), "{secret} must never reach HQ");
    }
}

#[tokio::test]
async fn organizations_audit_maps_the_window_and_cursor() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/organizations/organizations/{ORG}/audit"),
        &format!(
            "{}&limit=5&cursor=41&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z",
            scoped("support-review")
        ),
        "organizations.audit.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last_call(&fake);
    assert_eq!(
        path,
        format!("/internal/v1/admin/organizations/{ORG}/audit-events")
    );
    let q: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        q,
        [
            ("before", "41"),
            ("limit", "5"),
            ("from", "2026-11-01T00:00:00Z"),
            ("to", "2026-11-10T00:00:00Z")
        ]
    );
    let e = &v["result"]["items"][0];
    assert_eq!(e["sequence"], 41);
    assert!(
        e.get("summary").is_none(),
        "the owner's free-form summary is not relayed"
    );
    assert!(!v.to_string().contains("someone@acme.example"));
    assert_eq!(v["result"]["next_cursor"], "40");
    // a non-numeric cursor is the caller's error, never sent to the owner
    let calls = fake.calls.lock().unwrap().len();
    let bad = call(
        &format!("/organizations/organizations/{ORG}/audit"),
        &format!(
            "{}&cursor=abc&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z",
            scoped("support-review")
        ),
        "organizations.audit.read",
        &["cfmeet.support"],
    );
    assert_eq!(code(&send(&h, &bad).await.1), "QUERY_INVALID");
    assert_eq!(fake.calls.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn organizations_global_audit_is_served() {
    let (h, fake) = harness().await;
    let c = call(
        "/organizations/audit",
        &format!(
            "{}&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z",
            scoped("support-review")
        ),
        "organizations.global-audit.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(last_call(&fake).1, "/internal/v1/operator/audit-events");
}

#[tokio::test]
async fn organizations_members_are_opaque_user_ids() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/organizations/organizations/{ORG}/members"),
        &scoped("support-review"),
        "organizations.members.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        last_call(&fake).1,
        format!("/internal/v1/operator/organizations/{ORG}/members")
    );
    let m = &v["result"]["items"][0];
    assert_eq!(m["user_id"], USER);
    assert!(m.get("email").is_none() && m.get("createdBy").is_none());
    assert!(v["result"]["next_cursor"].is_null());
}

#[tokio::test]
async fn owner_failures_map_to_honest_errors_and_are_audited() {
    let (h, fake) = harness().await;
    let detail = || {
        call(
            &format!("/organizations/organizations/{ORG}"),
            &scoped("support-review"),
            "organizations.organizations.read",
            &["cfmeet.support"],
        )
    };
    *fake.fail.lock().unwrap() = Some(OwnerError::NotFound);
    assert_eq!(code(&send(&h, &detail()).await.1), "RESOURCE_NOT_FOUND");
    *fake.fail.lock().unwrap() = Some(OwnerError::Unavailable);
    let (status, v) = send(&h, &detail()).await;
    assert_eq!((status, code(&v)), (503, "OWNER_UNAVAILABLE"));
    assert!(v.get("result").is_none());
    *fake.fail.lock().unwrap() = Some(OwnerError::Contract);
    assert_eq!(code(&send(&h, &detail()).await.1), "INTERNAL");
    // every one of them was authorized and audited before the owner call
    assert!(
        h.store
            .audit_entries()
            .iter()
            .filter(|e| e.0.decision == "ALLOWED")
            .count()
            >= 3
    );
}

#[tokio::test]
async fn the_module_switch_gates_owner_reads() {
    let fake = Arc::new(FakeOrganizations::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: false,
        ..Default::default()
    })
    .await;
    let c = call(
        &format!("/organizations/organizations/{ORG}"),
        &scoped("support-review"),
        "organizations.organizations.read",
        &["cfmeet.support"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "CAPABILITY_DISABLED");
    assert!(
        fake.calls.lock().unwrap().is_empty(),
        "no owner call when the module is off"
    );
}

#[tokio::test]
async fn organizations_teams_drop_free_text() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/organizations/organizations/{ORG}/teams"),
        &scoped("support-review"),
        "organizations.teams.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        last_call(&fake).1,
        format!("/internal/v1/operator/organizations/{ORG}/teams")
    );
    assert_eq!(v["result"]["items"][0]["name"], "Front desk");
    assert!(
        !v.to_string().contains("0803"),
        "team descriptions are free text and never relayed"
    );
}

#[tokio::test]
async fn organizations_bindings_treat_absent_as_unbound() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/organizations/organizations/{ORG}/bindings"),
        &scoped("support-review"),
        "organizations.bindings.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let calls: Vec<String> = fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.1.clone())
        .collect();
    assert!(
        calls
            .iter()
            .any(|p| p.ends_with("/service-bindings/calling"))
            && calls
                .iter()
                .any(|p| p.ends_with("/service-bindings/messaging"))
    );
    let items = v["result"]["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "messaging answered 404: not bound, not an error"
    );
    assert_eq!(items[0]["service"], "calling");
    let text = v.to_string();
    assert!(!text.contains("sa_123") && !text.contains("cred_456"));
}
