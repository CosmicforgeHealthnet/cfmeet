//! Partner and app reads through the full chain (real mTLS, signed HQ request) against a
//! fake Developer Platform that records every call: paths, query mapping, the operator
//! attribution, strict translation and minimization.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    owners::{OwnerError, OwnerPort},
};
use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};

const WS: &str = "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a";
const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const T0: &str = "2026-11-01T10:00:00.123456+00:00";

/// (owner, path, query, operator) of one owner call.
type Recorded = (Owner, String, Vec<(String, String)>, String);

#[derive(Default)]
struct FakeDeveloperPlatform {
    calls: Mutex<Vec<Recorded>>,
}

fn org() -> Value {
    json!({"id": WS, "displayName": "Acme Partner", "state": "ACTIVE", "stateReason": null,
           "supportEmail": "help@acme.example", "createdAt": T0, "updatedAt": T0, "closedAt": null,
           "version": 3, "myRole": null, "createdBy": format!("user:{ID}"), "platformNameSyncPending": false})
}

fn page(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "nextCursor": next})
}

#[async_trait]
impl OwnerPort for FakeDeveloperPlatform {
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
            "/organizations" => page(vec![org()], json!("ay0xMjM0NTY3ODkwOmFiYw")),
            x if x == format!("/organizations/{WS}") => {
                let mut o = org();
                o["activeMembers"] = json!(4);
                o["projects"] = json!(2);
                o
            }
            "/activity" => page(
                vec![
                    json!({"id": ID, "scope": "ORGANIZATION", "organizationId": WS, "operator": "hq:staff-0002",
                "action": "organization.state_changed", "targetType": "ORGANIZATION", "targetId": WS, "outcome": "SUCCEEDED",
                "requestId": "req_x", "correlationId": "corr_x", "summary": {"reason": "contact help@acme.example"}, "occurredAt": T0}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/projects") => page(
                vec![
                    json!({"projectId": ID, "displayName": "Mobile", "status": "ACTIVE", "statusReason": null,
                "environments": 2, "createdAt": T0, "updatedAt": T0, "archivedAt": null, "version": 1}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/environments") => page(
                vec![
                    json!({"environmentId": ID, "projectId": ID, "kind": "SANDBOX", "displayName": "Sandbox",
                "status": "ACTIVE", "productionAccessState": "SANDBOX_READY", "createdAt": T0, "updatedAt": T0, "version": 1}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/members") => page(
                vec![
                    json!({"membershipId": ID, "userId": ID, "role": "OWNER", "status": "ACTIVE",
                "joinedVia": "CREATION", "createdAt": T0, "updatedAt": T0, "removedAt": null, "version": 1}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/invitations") => page(
                vec![
                    json!({"invitationId": ID, "role": "DEVELOPER", "status": "PENDING",
                "createdAt": T0, "expiresAt": T0, "decidedAt": null}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/push-providers") => page(
                vec![
                    json!({"pushProviderId": ID, "projectId": ID, "environmentId": ID,
                "environmentKind": "PRODUCTION", "provider": "APNS", "apnsKeyId": "ABCDE12345", "apnsTeamId": "TEAM123456",
                "apnsBundleId": "com.acme.app", "apnsHost": "PRODUCTION", "fcmProjectId": null,
                "fingerprint": "a".repeat(64), "verification": "VERIFIED", "lastVerifiedAt": T0, "lastErrorCode": null,
                "createdAt": T0, "updatedAt": T0, "state": "DISABLED", "disabledAt": T0, "version": 4}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/usage") => {
                let mut v = page(
                    vec![
                        json!({"projectId": ID, "environmentId": ID, "environmentKind": "PRODUCTION",
                        "usageType": "MESSAGE_SENT", "unit": "MESSAGE", "quantity": 120, "factCount": 7,
                        "organizationName": "Acme Partner"}),
                    ],
                    json!("c2VxOjUw"),
                );
                v["from"] = json!("2026-11-01");
                v["to"] = json!("2026-11-09");
                v
            }
            "/incidents" => page(
                vec![
                    json!({"incidentRecordId": ID, "organizationId": WS, "incidentRef": "INC-2026-114",
                    "occurredAt": T0, "recordedAt": T0, "summary": "call help@acme.example"}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/webhooks") => page(
                vec![
                    json!({"subscriptionId": ID, "projectId": ID, "environmentId": ID,
                "environmentKind": "PRODUCTION", "endpointHost": "hooks.acme.example", "eventTypes": ["messaging.conversation.created"],
                "status": "ACTIVE", "consecutiveFailures": 0, "lastSuccessAt": T0, "lastFailureAt": null, "disabledReason": null,
                "origin": "NATIVE", "createdAt": T0, "updatedAt": T0}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/webhook-deliveries") => page(
                vec![
                    json!({"deliveryId": ID, "subscriptionId": ID, "eventId": ID,
                "environmentId": ID, "status": "FAILED", "attemptCount": 5, "lastHttpStatus": 500, "failureCategory": "HTTP_5XX",
                "createdAt": T0, "completedAt": T0}),
                ],
                Value::Null,
            ),
            "/app-search" => page(
                vec![
                    json!({"appId": ID, "organizationId": WS, "name": "Calendar Sync", "distribution": "PUBLIC",
                "status": "PUBLISHED", "currentVersion": 2, "publishedVersion": 2, "publishedAt": T0, "createdAt": T0, "updatedAt": T0}),
                ],
                Value::Null,
            ),
            x if x == format!("/apps/{ID}") => {
                json!({"appId": ID, "clientId": ID, "projectId": ID, "organizationId": WS, "status": "IN_REVIEW",
                "currentVersion": 2, "submittedVersion": 2, "reviewedVersion": null, "publishedVersion": 1, "liveVersion": 1,
                "pendingChanges": true, "testAccountIds": [ID], "secretIssuedAt": T0,
                "manifest": {"name": "Calendar Sync", "distribution": "PUBLIC", "redirectUris": ["https://app.acme.example/cb"], "privacyUrl": "https://acme.example/privacy"},
                "submittedManifest": {"name": "Calendar Sync"}, "developerName": "Acme Partner", "listedInDirectory": true,
                "review": {"reasonCode": null, "decidedAt": T0}, "publishedAt": T0, "createdAt": T0, "updatedAt": T0,
                "history": [{"fromStatus": null, "toStatus": "DRAFT", "version": 1, "actorKind": "USER", "reasonCode": null, "occurredAt": T0},
                            {"fromStatus": "DRAFT", "toStatus": "SUBMITTED", "version": 2, "actorKind": "USER", "reasonCode": null, "occurredAt": T0}]})
            }
            x if x.ends_with("/versions") => page(
                vec![
                    json!({"version": 2, "name": "Calendar Sync", "distribution": "PUBLIC", "createdAt": T0}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/installations") => page(
                vec![
                    json!({"grantId": ID, "targetKind": "ORGANIZATION", "organizationId": WS,
                "principalId": null, "scopeCount": 3, "status": "ACTIVE", "revokedReason": null, "installedAt": T0, "updatedAt": T0}),
                ],
                Value::Null,
            ),
            _ => return Err(OwnerError::NotFound),
        })
    }
}

async fn harness() -> (Harness, Arc<FakeDeveloperPlatform>) {
    let fake = Arc::new(FakeDeveloperPlatform::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    (h, fake)
}

fn last_call(fake: &FakeDeveloperPlatform) -> Recorded {
    fake.calls
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("an owner call")
}

fn window_query(purpose: &str) -> String {
    format!(
        "{}&limit=10&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z",
        scoped(purpose)
    )
}

#[tokio::test]
async fn partners_workspaces_search_drops_the_support_address() {
    let (h, fake) = harness().await;
    let c = call(
        "/partners/workspaces",
        &format!("{}&limit=5&q=acme", scoped("support-review")),
        "partners.workspaces.read",
        &["cfmeet.partner_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (owner, path, query, operator) = last_call(&fake);
    assert_eq!(
        (owner, path.as_str(), operator.as_str()),
        (
            Owner::DeveloperPlatform,
            "/internal/v1/operator/organizations",
            "hq:staff-0001"
        )
    );
    assert_eq!(
        query,
        vec![
            ("query".into(), "acme".into()),
            ("limit".into(), "5".into())
        ]
    );
    let w = &v["result"]["items"][0];
    assert_eq!(w["workspace_id"], WS);
    assert_eq!(w["created_at"], "2026-11-01T10:00:00Z");
    assert_eq!(v["result"]["next_cursor"], "ay0xMjM0NTY3ODkwOmFiYw");
    let text = v.to_string();
    assert!(
        !text.contains("help@acme.example") && !text.contains("user:"),
        "no support address, no creator ref"
    );
}

#[tokio::test]
async fn partners_workspace_detail_carries_counts() {
    let (h, _) = harness().await;
    let c = call(
        &format!("/partners/workspaces/{WS}"),
        &scoped("support-review"),
        "partners.workspaces.read",
        &["cfmeet.partner_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (
            v["result"]["active_members"].clone(),
            v["result"]["projects"].clone()
        ),
        (json!(4), json!(2))
    );
    assert!(!v.to_string().contains("help@acme.example"));
    // A non-UUID workspace id names nothing: 404 without an owner call.
    let bad = call(
        "/partners/workspaces/not-a-uuid",
        &scoped("support-review"),
        "partners.workspaces.read",
        &["cfmeet.partner_ops"],
    );
    assert_eq!(code(&send(&h, &bad).await.1), "RESOURCE_NOT_FOUND");
}

#[tokio::test]
async fn partners_activity_maps_window_and_workspace() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/partners/workspaces/{WS}/staff-activity"),
        &window_query("support-review"),
        "partners.workspace-activity.read",
        &["cfmeet.partner_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last_call(&fake);
    assert_eq!(path, "/internal/v1/operator/activity");
    let q: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        q,
        [
            ("organizationId", WS),
            ("since", "2026-11-01T00:00:00Z"),
            ("until", "2026-11-10T00:00:00Z"),
            ("limit", "10")
        ]
    );
    let e = &v["result"]["items"][0];
    assert_eq!(e["operator"], "hq:staff-0002");
    assert!(e.get("summary").is_none() && !v.to_string().contains("help@acme.example"));

    let d = call(
        &format!("/partners/workspaces/{WS}/webhook-deliveries"),
        &window_query("support-review"),
        "partners.webhook-deliveries.read",
        &["cfmeet.partner_ops"],
    );
    let (status, v) = send(&h, &d).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last_call(&fake);
    assert_eq!(
        path,
        format!("/internal/v1/operator/organizations/{WS}/webhook-deliveries")
    );
    assert!(query.contains(&("since".into(), "2026-11-01T00:00:00Z".into())));
    assert_eq!(v["result"]["items"][0]["last_http_status"], 500);
}

#[tokio::test]
async fn partners_lists_translate_strictly() {
    let (h, fake) = harness().await;
    for (path, perm, owner_suffix) in [
        (
            format!("/partners/workspaces/{WS}/projects"),
            "partners.projects.read",
            "/projects",
        ),
        (
            format!("/partners/workspaces/{WS}/environments"),
            "partners.environments.read",
            "/environments",
        ),
        (
            format!("/partners/workspaces/{WS}/members"),
            "partners.members.read",
            "/members",
        ),
        (
            format!("/partners/workspaces/{WS}/invitations"),
            "partners.invitations.read",
            "/invitations",
        ),
        (
            format!("/apps/apps/{ID}/versions"),
            "apps.versions.read",
            "/versions",
        ),
        (
            format!("/apps/apps/{ID}/installs"),
            "apps.installs.read",
            "/installations",
        ),
    ] {
        let roles: &[&'static str] = if perm.starts_with("apps.") {
            &["cfmeet.verification.reviewer"]
        } else {
            &["cfmeet.partner_ops"]
        };
        let purpose = if perm.starts_with("apps.") {
            "verification-review"
        } else {
            "support-review"
        };
        let c = call(&path, &scoped(purpose), perm, roles);
        let (status, v) = send(&h, &c).await;
        assert_eq!(status, 200, "{path}: {v}");
        assert!(last_call(&fake).1.ends_with(owner_suffix), "{path}");
        assert_eq!(v["result"]["items"].as_array().unwrap().len(), 1, "{path}");
        assert!(v["result"]["next_cursor"].is_null());
    }
}

#[tokio::test]
async fn partners_push_keys_are_details_only() {
    let (h, _) = harness().await;
    let c = call(
        &format!("/partners/workspaces/{WS}/push-keys"),
        &scoped("support-review"),
        "partners.push-keys.read",
        &["cfmeet.partner_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let k = &v["result"]["items"][0];
    assert_eq!(
        (k["key_id"].clone(), k["provider"].clone()),
        (json!("ABCDE12345"), json!("APNS"))
    );
    assert_eq!(k["fingerprint"].as_str().unwrap().len(), 64);
    // The state and row version an operator's disable command acts on.
    assert_eq!(
        (k["state"].clone(), k["version"].clone()),
        (json!("DISABLED"), json!(4))
    );
    assert_eq!(k["disabled_at"], "2026-11-01T10:00:00Z");
}

#[tokio::test]
async fn partners_webhooks_expose_the_host_only() {
    let (h, _) = harness().await;
    let c = call(
        &format!("/partners/workspaces/{WS}/webhooks"),
        &scoped("support-review"),
        "partners.webhooks.read",
        &["cfmeet.partner_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let w = &v["result"]["items"][0];
    assert_eq!(w["endpoint_host"], "hooks.acme.example");
    assert!(w.get("url").is_none() && w.get("description").is_none());
}

#[tokio::test]
async fn apps_search_and_detail() {
    let (h, fake) = harness().await;
    let s = call(
        "/apps/apps",
        &format!("{}&q=calendar", scoped("verification-review")),
        "apps.apps.read",
        &["cfmeet.verification.reviewer"],
    );
    let (status, v) = send(&h, &s).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last_call(&fake);
    assert_eq!(
        (path.as_str(), query[0].clone()),
        (
            "/internal/v1/operator/app-search",
            ("q".to_string(), "calendar".to_string())
        )
    );
    assert_eq!(v["result"]["items"][0]["name"], "Calendar Sync");

    let d = call(
        &format!("/apps/apps/{ID}"),
        &scoped("verification-review"),
        "apps.apps.read",
        &["cfmeet.verification.reviewer"],
    );
    let (status, v) = send(&h, &d).await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"];
    assert_eq!(
        (r["name"].clone(), r["developer_name"].clone()),
        (json!("Calendar Sync"), json!("Acme Partner"))
    );
    assert_eq!(r["history"][0]["to_status"], "SUBMITTED", "newest first");
    let text = v.to_string();
    for leak in [
        "redirect",
        "privacy",
        "app.acme.example",
        "test_account",
        "secret",
    ] {
        assert!(
            !text.to_lowercase().contains(leak),
            "{leak} must not reach HQ"
        );
    }
}

#[tokio::test]
async fn partners_usage_and_incidents_map_the_window() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/partners/workspaces/{WS}/usage"),
        &window_query("support-review"),
        "partners.usage.read",
        &["cfmeet.partner_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, operator) = last_call(&fake);
    assert_eq!(
        path,
        format!("/internal/v1/operator/organizations/{WS}/usage")
    );
    assert_eq!(operator, "hq:staff-0001");
    let q: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        q,
        [
            ("since", "2026-11-01T00:00:00Z"),
            ("until", "2026-11-10T00:00:00Z"),
            ("limit", "10")
        ]
    );
    let r = &v["result"];
    assert_eq!(
        (r["from"].clone(), r["to"].clone()),
        (json!("2026-11-01"), json!("2026-11-09"))
    );
    assert_eq!(r["next_cursor"], "c2VxOjUw");
    assert_eq!(r["items"][0]["quantity"], 120);
    assert!(r["items"][0].get("organizationName").is_none());

    let c = call(
        "/partners/incidents",
        &window_query("ops-incident"),
        "partners.incidents.read",
        &["cfmeet.partner_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last_call(&fake);
    assert_eq!(path, "/internal/v1/operator/incidents");
    assert!(query.contains(&("until".into(), "2026-11-10T00:00:00Z".into())));
    let i = &v["result"]["items"][0];
    assert_eq!(i["incident_ref"], "INC-2026-114");
    assert_eq!(i["workspace_id"], WS);
    assert!(!v.to_string().contains("help@acme.example"));
}
