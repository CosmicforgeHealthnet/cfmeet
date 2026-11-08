//! Meeting Service → Organizations migration import (contract §5): the shared checksum
//! test vector, idempotent re-import, conflict reporting, refusal of orphan memberships,
//! reconciliation and the `organizations-import` CLI.

mod common;

use common::{s, Harness};
use organizations::import::Export;
use serde_json::{json, Value};
use uuid::Uuid;

/// The shared test vector (also asserted by the Meeting Service export).
const VECTOR_CHECKSUM: &str = "1a212f81add323e1e9bed715699c0ba8c97b2dc22e211c84a05bca42d0908e78";
const ORG_ID: &str = "00000000-0000-4000-8000-000000000001";
const OWNER_ID: &str = "00000000-0000-4000-8000-0000000000a1";
const MEMBERSHIP_ID: &str = "00000000-0000-4000-8000-0000000000b1";

fn vector() -> Value {
    json!({
        "schemaVersion": 1,
        "source": "cfmeet.meeting-service",
        "exportedAt": "2026-11-06T00:00:00Z",
        "organizations": [{
            "id": ORG_ID, "name": "Acme Health", "slug": "acme-health", "status": "ACTIVE",
            "createdBy": null, "createdAt": "2026-10-04T12:00:00Z", "updatedAt": "2026-10-04T12:00:00Z",
            "profilePolicy": {}
        }],
        "memberships": [{
            "id": MEMBERSHIP_ID, "organizationId": ORG_ID, "userId": OWNER_ID, "role": "OWNER", "status": "ACTIVE",
            "createdBy": null, "createdAt": "2026-10-04T12:00:00Z", "updatedAt": "2026-10-04T12:00:00Z", "removedAt": null
        }],
        "counts": { "organizations": 1, "memberships": 1 },
        "checksum": VECTOR_CHECKSUM
    })
}

/// Recomputes counts and checksum after a deliberate edit.
fn reseal(mut doc: Value) -> Value {
    doc["counts"] = json!({
        "organizations": doc["organizations"].as_array().unwrap().len(),
        "memberships": doc["memberships"].as_array().unwrap().len(),
    });
    doc["checksum"] = json!("0".repeat(64));
    let export: Export = serde_json::from_value(doc.clone()).unwrap();
    doc["checksum"] = json!(export.computed_checksum());
    doc
}

async fn import(h: &Harness, doc: &Value) -> common::Response {
    h.by(&h.peers.meeting)
        .post("/internal/v1/migration/import", doc.clone())
        .await
}

#[tokio::test]
async fn the_shared_vector_imports_with_ids_and_timestamps_preserved_and_reimport_is_a_no_op() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let export: Export = serde_json::from_value(vector()).unwrap();
    assert_eq!(export.computed_checksum(), VECTOR_CHECKSUM);

    let r = import(&h, &vector()).await;
    let report = r.expect(200).clone();
    assert_eq!(report["imported"], 2);
    assert_eq!(report["unchanged"], 0);
    assert_eq!(report["conflicts"], json!([]));
    assert_eq!(report["checksum"], VECTOR_CHECKSUM);

    let org = h
        .by(&h.peers.messaging)
        .get(&format!("/internal/v1/organizations/{ORG_ID}"))
        .await;
    let org = org.expect(200);
    assert_eq!(org["id"], ORG_ID);
    assert_eq!(org["slug"], "acme-health");
    assert_eq!(org["creationSource"], "IMPORTED");
    assert_eq!(org["provisioningState"], "PENDING");
    assert_eq!(org["lifecycleStatus"], "ACTIVE");
    assert_eq!(org["createdAt"], "2026-10-04T12:00:00.000000Z");
    assert_eq!(org["createdBy"], json!(null));
    let members = h
        .by(&h.peers.messaging)
        .get(&format!("/internal/v1/organizations/{ORG_ID}/members"))
        .await;
    let m = &members.expect(200)["items"][0];
    assert_eq!(m["id"], MEMBERSHIP_ID);
    assert_eq!(m["role"], "OWNER");
    assert_eq!(m["joinedVia"], "IMPORTED");
    // The imported OWNER can act immediately (same account id).
    let owner: Uuid = OWNER_ID.parse().unwrap();
    let view = h
        .user(owner)
        .get(&format!("/internal/v1/organizations/{ORG_ID}"))
        .await;
    assert_eq!(view.expect(200)["viewer"]["role"], "OWNER");
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND event_type IN ('organization.created', 'membership.activated')").await, 2);
    assert_eq!(
        h.count(
            "SELECT count(*) FROM organization_membership_history WHERE change_kind = 'IMPORTED'"
        )
        .await,
        1
    );

    // Reconciliation agrees with the export.
    let rec = h
        .by(&h.peers.meeting)
        .get("/internal/v1/migration/reconciliation")
        .await;
    let rec = rec.expect(200).clone();
    assert_eq!(rec["checksum"], VECTOR_CHECKSUM);
    assert_eq!(rec["mismatches"], json!([]));
    assert_eq!(rec["organizations"]["imported"], 1);
    assert_eq!(rec["memberships"]["imported"], 1);
    assert_eq!(rec["lastImport"]["checksum"], VECTOR_CHECKSUM);

    // Re-import: a no-op.
    let again = import(&h, &vector()).await;
    assert_eq!(again.expect(200)["imported"], 0);
    assert_eq!(again.json["unchanged"], 2);
    assert_eq!(again.json["conflicts"], json!([]));
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation'").await, 2);
    assert_eq!(
        h.count("SELECT count(*) FROM organization_import_runs")
            .await,
        2
    );
    assert_eq!(h.count("SELECT count(*) FROM organization_audit_events WHERE action = 'migration.imported' AND organization_id IS NULL").await, 2);

    // A native change after import is visible to reconciliation (never silently equal).
    h.user(owner)
        .patch(
            &format!("/internal/v1/organizations/{ORG_ID}"),
            json!({ "expectedVersion": 1, "name": "Acme Renamed" }),
        )
        .await
        .expect(200);
    let rec = h
        .by(&h.peers.meeting)
        .get("/internal/v1/migration/reconciliation")
        .await;
    assert_eq!(
        rec.expect(200)["mismatches"][0]["kind"],
        "CHECKSUM_MISMATCH"
    );
}

#[tokio::test]
async fn a_differing_row_is_reported_and_never_overwritten() {
    let Some(h) = Harness::new().await else {
        return;
    };
    import(&h, &vector()).await.expect(200);
    let mut changed = vector();
    changed["organizations"][0]["name"] = json!("Acme Health Group");
    changed["memberships"][0]["role"] = json!("ADMIN");
    let new_member = json!({
        "id": "00000000-0000-4000-8000-0000000000b2", "organizationId": ORG_ID,
        "userId": "00000000-0000-4000-8000-0000000000a2", "role": "MEMBER", "status": "ACTIVE",
        "createdBy": OWNER_ID, "createdAt": "2026-10-05T08:30:00.123456Z", "updatedAt": "2026-10-05T08:30:00.123456Z", "removedAt": null
    });
    changed["memberships"]
        .as_array_mut()
        .unwrap()
        .push(new_member);
    let changed = reseal(changed);
    let r = import(&h, &changed).await;
    let report = r.expect(200).clone();
    assert_eq!(report["imported"], 1, "the new membership is imported");
    assert_eq!(report["unchanged"], 0);
    let conflicts = report["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 2);
    assert_eq!(
        conflicts[0],
        json!({ "kind": "ORGANIZATION_DIFFERS", "entity": "organization", "id": ORG_ID, "fields": ["name"] })
    );
    assert_eq!(
        conflicts[1],
        json!({ "kind": "MEMBERSHIP_DIFFERS", "entity": "membership", "id": MEMBERSHIP_ID, "fields": ["role"] })
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organizations WHERE id = '{ORG_ID}' AND name = 'Acme Health'"
        ))
        .await,
        1
    );
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_memberships WHERE id = '{MEMBERSHIP_ID}' AND role = 'OWNER'")).await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM organization_memberships WHERE id = '00000000-0000-4000-8000-0000000000b2' AND created_at = '2026-10-05T08:30:00.123456Z'").await,
        1
    );
    let rec = h
        .by(&h.peers.meeting)
        .get("/internal/v1/migration/reconciliation")
        .await;
    let kinds: Vec<String> = rec.expect(200)["mismatches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| s(&m["kind"]))
        .collect();
    assert!(
        kinds.contains(&"CHECKSUM_MISMATCH".to_string()),
        "{kinds:?}"
    );
    assert!(kinds.contains(&"IMPORT_CONFLICTS".to_string()), "{kinds:?}");
}

#[tokio::test]
async fn invalid_exports_and_orphan_memberships_are_refused_atomically() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Orphan: a membership whose organization is neither exported nor imported.
    let mut orphan = vector();
    orphan["memberships"].as_array_mut().unwrap().push(json!({
        "id": "00000000-0000-4000-8000-0000000000b9", "organizationId": "00000000-0000-4000-8000-000000000099",
        "userId": OWNER_ID, "role": "MEMBER", "status": "ACTIVE", "createdBy": null,
        "createdAt": "2026-10-04T12:00:00Z", "updatedAt": "2026-10-04T12:00:00Z", "removedAt": null
    }));
    let orphan = reseal(orphan);
    import(&h, &orphan)
        .await
        .expect_error(400, "VALIDATION_ERROR");
    assert_eq!(
        h.count("SELECT count(*) FROM organizations").await,
        0,
        "nothing is imported"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organization_memberships")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organization_import_runs")
            .await,
        0
    );

    let mut bad_checksum = vector();
    bad_checksum["checksum"] = json!("f".repeat(64));
    import(&h, &bad_checksum)
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let mut bad_version = vector();
    bad_version["schemaVersion"] = json!(2);
    import(&h, &bad_version)
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let mut bad_counts = vector();
    bad_counts["counts"]["organizations"] = json!(3);
    import(&h, &bad_counts)
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let mut bad_role = vector();
    bad_role["memberships"][0]["role"] = json!("MANAGER");
    import(&h, &reseal(bad_role))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    import(&h, &json!({ "schemaVersion": 1 }))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // Only organization.migrate callers.
    h.by(&h.peers.messaging)
        .post("/internal/v1/migration/import", vector())
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    assert_eq!(h.count("SELECT count(*) FROM organizations").await, 0);
    // The operator console may import.
    h.by(&h.peers.operator)
        .post("/internal/v1/migration/import", vector())
        .await
        .expect(200);
}

#[tokio::test]
async fn slug_collisions_and_suspended_legacy_organizations() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // A native organization already holds the slug: reported, its memberships too.
    h.create_org(Uuid::new_v4(), "acme-health").await;
    let r = import(&h, &vector()).await;
    let report = r.expect(200).clone();
    assert_eq!(report["imported"], 0);
    let kinds: Vec<String> = report["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| s(&c["kind"]))
        .collect();
    assert_eq!(kinds, vec!["SLUG_TAKEN", "ORGANIZATION_NOT_IMPORTED"]);
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organizations WHERE id = '{ORG_ID}'"
        ))
        .await,
        0
    );

    // A suspended legacy organization stays suspended; its owners cannot lift it.
    let mut legacy = vector();
    legacy["organizations"][0]["id"] = json!("00000000-0000-4000-8000-000000000002");
    legacy["organizations"][0]["slug"] = json!("x");
    legacy["organizations"][0]["status"] = json!("SUSPENDED");
    legacy["memberships"][0]["organizationId"] = json!("00000000-0000-4000-8000-000000000002");
    let legacy = reseal(legacy);
    import(&h, &legacy).await.expect(200);
    let owner: Uuid = OWNER_ID.parse().unwrap();
    let view = h
        .user(owner)
        .get("/internal/v1/organizations/00000000-0000-4000-8000-000000000002")
        .await;
    assert_eq!(view.expect(200)["lifecycleStatus"], "SUSPENDED");
    assert_eq!(view.json["slug"], "x", "legacy slugs are preserved");
    h.user(owner)
        .post(
            "/internal/v1/organizations/00000000-0000-4000-8000-000000000002/reactivate",
            json!({}),
        )
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
}

#[tokio::test]
async fn the_cli_imports_the_same_export_idempotently() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let path = std::env::temp_dir().join(format!("org-export-{}.json", Uuid::new_v4().simple()));
    std::fs::write(&path, vector().to_string()).unwrap();
    let bin = env!("CARGO_BIN_EXE_organizations-import");
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .args(args)
            .env("DATABASE_URL", &h.db.url)
            .output()
            .unwrap()
    };
    let first = run(&[path.to_str().unwrap()]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let report: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(
        (report["imported"].as_u64(), report["checksum"].as_str()),
        (Some(2), Some(VECTOR_CHECKSUM))
    );
    let second = run(&[path.to_str().unwrap()]);
    let report: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(
        (report["imported"].as_u64(), report["unchanged"].as_u64()),
        (Some(0), Some(2))
    );
    let rec = run(&["--reconciliation"]);
    let rec: Value = serde_json::from_slice(&rec.stdout).unwrap();
    assert_eq!(rec["checksum"], VECTOR_CHECKSUM);
    // A refused export exits 1 and imports nothing more.
    let mut bad = vector();
    bad["checksum"] = json!("0".repeat(64));
    std::fs::write(&path, bad.to_string()).unwrap();
    let refused = run(&[path.to_str().unwrap()]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("checksum mismatch"));
    std::fs::remove_file(&path).ok();
    assert_eq!(
        h.count("SELECT count(*) FROM organization_import_runs WHERE actor_kind = 'OPERATOR'")
            .await,
        2
    );
}
