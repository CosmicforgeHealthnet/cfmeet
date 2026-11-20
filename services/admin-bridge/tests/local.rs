//! Bridge-local capabilities through the full chain: the audit search and the operations
//! health view.

mod common;

use common::*;

#[tokio::test]
async fn audit_entries_page_the_hash_chained_audit() {
    let h = start(Options {
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    // A few audited requests first (one refused).
    assert_eq!(send(&h, &discovery_call()).await.0, 200);
    let mut refused = discovery_call();
    refused.permissions = vec!["bridge.health.read".into()];
    assert_eq!(code(&send(&h, &refused).await.1), "PERMISSION_DENIED");
    // The window may end up to 60 s ahead: include the rows just written.
    let start = (chrono::Utc::now() - chrono::Duration::hours(1)).format("%Y-%m-%dT%H:%M:%SZ");
    let end = (chrono::Utc::now() + chrono::Duration::seconds(30)).format("%Y-%m-%dT%H:%M:%SZ");
    let audit = |cursor: Option<String>| {
        let cursor = cursor.map(|c| format!("&cursor={c}")).unwrap_or_default();
        let q = format!(
            "{}&limit=1{cursor}&start={start}&end={end}",
            scoped("support-review")
        );
        call(
            "/audit/entries",
            &q,
            "audit.entries.read",
            &["cfmeet.security_admin"],
        )
    };
    let (status, v) = send(&h, &audit(None)).await;
    assert_eq!(status, 200, "{v}");
    assert_envelope(&v, true);
    let first = &v["result"]["items"][0];
    assert_eq!(first["sequence"], 1);
    assert_eq!(first["actor"], "hq:staff-0001");
    assert_eq!(first["capability_id"], "bridge.capabilities.read");
    assert_eq!(first["decision"], "ALLOWED");
    assert!(first.get("request_digest").is_none() && first.get("path").is_none());
    let next = v["result"]["next_cursor"].as_str().unwrap().to_string();
    let (status, v) = send(&h, &audit(Some(next))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["items"][0]["decision"], "PERMISSION_DENIED");
    // A bad cursor is refused.
    let (_, v) = send(&h, &audit(Some("-1".into()))).await;
    assert_eq!(code(&v), "QUERY_INVALID");
}

#[tokio::test]
async fn operations_health_is_the_aggregate_health() {
    let h = start(Options {
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    let (status, v) = send(
        &h,
        &call(
            "/operations/health",
            &scoped("ops-incident"),
            "operations.health.read",
            &["cfmeet.platform_ops"],
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["services"][0]["service"], "admin-bridge");
    assert!(
        ["OK", "DEGRADED", "DOWN", "UNKNOWN"].contains(&v["result"]["status"].as_str().unwrap())
    );
}

fn approval(capability: &str) -> serde_json::Value {
    let now = chrono::Utc::now();
    serde_json::json!({
        "approval_id": uuid::Uuid::new_v4().to_string(),
        "capability": capability,
        "requester_ref": "staff-0001",
        "approver_ref": "staff-0002",
        "intent_digest": admin_bridge::wire::sha256_hex(b"intent"),
        "decided_at": (now - chrono::Duration::minutes(1)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "expires_at": (now + chrono::Duration::hours(11)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

fn privacy_command(path: &str, capability: &str, body: serde_json::Value, sub: &str) -> Call {
    let mut c = call(
        path,
        &scoped("privacy-request"),
        capability,
        &["cfmeet.security_admin"],
    );
    c.method = "POST";
    c.sub = sub.into();
    c.body = serde_json::to_vec(&body).unwrap();
    c.idempotency_key = body["operation_id"].as_str().map(str::to_string);
    let mut a = approval(capability);
    a["requester_ref"] = serde_json::json!(sub);
    c.approval = Some(a);
    c
}

#[tokio::test]
async fn privacy_requests_complete_only_when_every_service_reports() {
    let h = start(Options {
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    let subject = "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a";
    let id = uuid::Uuid::new_v4().to_string();
    let register = |sub: &str| {
        privacy_command(
            "/privacy/requests",
            "privacy.request.register",
            serde_json::json!({"operation_id": id, "expected_version": 0, "kind": "ERASURE", "subject_ref": subject}),
            sub,
        )
    };
    let first = register("staff-0001");
    let (status, v) = send(&h, &first).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["operation"]["status"], "SUCCEEDED");
    assert_eq!(v["result"]["resource"]["request_id"], id);
    assert_eq!(v["result"]["resource"]["state"], "OPEN");
    assert_eq!(
        v["result"]["resource"]["services"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    // A replay (same body, key and approval; a fresh token) returns the stored outcome.
    let mut again = register("staff-0001");
    again.approval = first.approval.clone();
    let (status, v) = send(&h, &again).await;
    assert_eq!(status, 200, "{v}");

    let services = [
        "meeting-service",
        "messaging",
        "calling",
        "communications",
        "organizations",
        "developer-platform",
        "notifications",
    ];
    let mut version = 1;
    for (n, service) in services.iter().enumerate() {
        // Different staff members (5 commands per minute per staff member and permission).
        let sub = format!("staff-01{n:02}");
        let outcome = if *service == "calling" {
            "NOT_HELD"
        } else {
            "DONE"
        };
        let key = uuid::Uuid::new_v4().to_string();
        let (status, v) = send(
            &h,
            &privacy_command(
                &format!("/privacy/requests/{id}/services/{service}/complete"),
                "privacy.request.complete-service",
                serde_json::json!({"operation_id": key, "expected_version": version, "outcome": outcome}),
                &sub,
            ),
        )
        .await;
        assert_eq!(status, 200, "{service}: {v}");
        version += 1;
        assert_eq!(v["result"]["resource"]["version"], version);
        let expected = if n == services.len() - 1 {
            "COMPLETE"
        } else {
            "OPEN"
        };
        assert_eq!(v["result"]["resource"]["state"], expected);
    }
    // A recorded outcome never changes; a stale version is refused (nothing done).
    let key = uuid::Uuid::new_v4().to_string();
    let (status, v) = send(
        &h,
        &privacy_command(
            &format!("/privacy/requests/{id}/services/messaging/complete"),
            "privacy.request.complete-service",
            serde_json::json!({"operation_id": key, "expected_version": version, "outcome": "LEGAL_HOLD"}),
            "staff-0200",
        ),
    )
    .await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    // Unknown service; unknown request.
    let (_, v) = send(
        &h,
        &privacy_command(
            &format!("/privacy/requests/{id}/services/numbering/complete"),
            "privacy.request.complete-service",
            serde_json::json!({"operation_id": uuid::Uuid::new_v4(), "expected_version": 1, "outcome": "DONE"}),
            "staff-0201",
        ),
    )
    .await;
    assert_eq!(code(&v), "RESOURCE_NOT_FOUND");

    // Read and list.
    let (status, v) = send(
        &h,
        &call(
            &format!("/privacy/requests/{id}"),
            &scoped("privacy-request"),
            "privacy.requests.read",
            &["cfmeet.security_admin"],
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["state"], "COMPLETE");
    assert!(v["result"]["completed_at"].is_string());
    let (status, v) = send(
        &h,
        &call(
            "/privacy/requests",
            &scoped("privacy-request"),
            "privacy.requests.read",
            &["cfmeet.security_admin"],
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["items"][0]["request_id"], id);
}
