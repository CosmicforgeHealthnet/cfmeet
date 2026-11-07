//! Blueprint v1.0 Section 15 — developer organization creation safety (ORG-014), name
//! propagation to Platform Access (ORG-013) and the operator read plane (PLT-009),
//! through the REAL router against an in-memory Platform Access (CI: real PostgreSQL).

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use developer_platform::{
    control::organizations::{creation_slug, reconcile_provisioning, sync_pending_names},
    platform_access::{DelegationActor, DelegationCall, PlatformAccessPort},
};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

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
    Uuid::parse_str(r.json["id"].as_str().unwrap()).unwrap()
}

async fn exec(c: &Control, sql: &str) {
    c.db.conn.execute_unprepared(sql).await.unwrap();
}

async fn provisioning(c: &Control) -> Vec<(Uuid, String, Option<Uuid>, i32)> {
    c.db.conn
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT id, state, tenant_id, attempts FROM devplat_organization_provisioning ORDER BY created_at",
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.try_get("", "id").unwrap(),
                r.try_get("", "state").unwrap(),
                r.try_get("", "tenant_id").unwrap(),
                r.try_get("", "attempts").unwrap(),
            )
        })
        .collect()
}

/// ORG-014: a failure injected AFTER Platform Access created the tenant and BEFORE the
/// local commit (the OWNER membership insert fails) leaves no active orphan: the tenant is
/// suspended at once and the creation is recorded COMPENSATED. Once the fault is gone a
/// creation completes normally.
#[tokio::test]
async fn a_failure_between_the_tenant_and_the_local_commit_is_compensated() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (alice, alice_id) = c.sessions.sign_in("alice@acme.example");
    exec(
        &c,
        "CREATE FUNCTION s15_inject_failure() RETURNS TRIGGER LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'injected failure'; END $$; \
         CREATE TRIGGER s15_inject_failure BEFORE INSERT ON devplat_workspace_members \
             FOR EACH ROW EXECUTE FUNCTION s15_inject_failure();",
    )
    .await;
    let r = c
        .req(
            As::User(&alice),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Doomed" })),
        )
        .await;
    assert!(r.status.is_server_error(), "{} {}", r.status, r.json);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_organizations").await,
        0
    );
    let rows = provisioning(&c).await;
    assert_eq!(rows.len(), 1);
    let (id, state, tenant, _) = rows[0].clone();
    assert_eq!(state, "COMPENSATED");
    let tenant = tenant.expect("the tenant is recorded");
    // The tenant was created under the creation's slug and is now suspended.
    assert_eq!(c.pa.tenant_by_slug(&creation_slug(id)), Some(tenant));
    assert_eq!(c.pa.tenant_status(tenant), "SUSPENDED");
    let audit = c.pa.audit.lock().unwrap().clone();
    assert!(audit.iter().any(|(actor, call)| {
        actor == &format!("u-{}", alice_id.simple())
            && matches!(call, DelegationCall::SetTenantStatus { tenant_id, suspended: true, .. } if *tenant_id == tenant)
    }));
    // No organization appears for the compensated tenant.
    let list = c
        .req(As::User(&alice), Method::GET, "/v1/organizations", None)
        .await;
    assert_eq!(list.json["items"].as_array().unwrap().len(), 0);
    // Fault removed: creation completes and is recorded COMPLETED.
    exec(
        &c,
        "DROP TRIGGER s15_inject_failure ON devplat_workspace_members; DROP FUNCTION s15_inject_failure();",
    )
    .await;
    let o = org(&c, &alice, "Acme").await;
    let rows = provisioning(&c).await;
    assert_eq!(rows[1].1, "COMPLETED");
    assert_eq!(rows[1].2, Some(o));
    assert_eq!(c.pa.tenant_status(o), "ACTIVE");
}

/// ORG-014: a creation interrupted for good (process crash, authority outage) is settled
/// by the worker's reconciliation: an orphaned tenant found by its creation slug is
/// suspended, a creation that never produced a tenant is abandoned, a recorded
/// organization is never suspended, a recent creation is left alone, and an unavailable
/// authority is retried.
#[tokio::test]
async fn interrupted_creations_are_reconciled_by_the_worker() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (alice, _) = c.sessions.sign_in("alice@acme.example");
    let recorded = org(&c, &alice, "Recorded").await;
    let old = "now() - interval '1 hour'";
    let orphan = Uuid::new_v4();
    let never = Uuid::new_v4();
    let fresh = Uuid::new_v4();
    let done = Uuid::new_v4();
    for (id, at, tenant) in [
        (orphan, old, "NULL".to_string()),
        (never, old, "NULL".to_string()),
        (fresh, "now()", "NULL".to_string()),
        (done, old, format!("'{recorded}'")),
    ] {
        exec(
            &c,
            &format!(
                "INSERT INTO devplat_organization_provisioning (id, requested_by, state, tenant_id, created_at, updated_at) \
                 VALUES ('{id}', 'user:{}', 'PENDING', {tenant}, {at}, {at})",
                Uuid::new_v4()
            ),
        )
        .await;
    }
    // Platform Access created the orphan's tenant (and the fresh one's), then the
    // process died before the local commit.
    let correlation = cf_service_auth::CorrelationId::generate();
    for id in [orphan, fresh] {
        c.pa.delegate(
            DelegationCall::CreateTenant {
                actor: DelegationActor::Human(Uuid::new_v4()),
                slug: creation_slug(id),
                display_name: "Interrupted".into(),
            },
            &correlation,
        )
        .await
        .unwrap();
    }
    let orphan_tenant = c.pa.tenant_by_slug(&creation_slug(orphan)).unwrap();
    let fresh_tenant = c.pa.tenant_by_slug(&creation_slug(fresh)).unwrap();
    let settle = chrono::Duration::minutes(10);
    // Authority unavailable: nothing is settled, every attempt is counted.
    c.pa.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = reconcile_provisioning(&c.db.conn, c.pa.as_ref(), settle, 50)
        .await
        .unwrap();
    assert_eq!(r.retried, 2, "{r:?}");
    assert_eq!(r.completed, 1, "{r:?}");
    c.pa.down.store(false, std::sync::atomic::Ordering::SeqCst);
    let r = reconcile_provisioning(&c.db.conn, c.pa.as_ref(), settle, 50)
        .await
        .unwrap();
    assert_eq!((r.compensated, r.abandoned, r.retried), (1, 1, 0), "{r:?}");
    let by_id = |rows: &[(Uuid, String, Option<Uuid>, i32)], id: Uuid| {
        rows.iter().find(|r| r.0 == id).cloned().unwrap()
    };
    let rows = provisioning(&c).await;
    assert_eq!(by_id(&rows, orphan).1, "COMPENSATED");
    assert_eq!(by_id(&rows, orphan).2, Some(orphan_tenant));
    assert_eq!(by_id(&rows, orphan).3, 1, "the failed attempt was counted");
    assert_eq!(by_id(&rows, never).1, "ABANDONED");
    assert_eq!(by_id(&rows, done).1, "COMPLETED");
    assert_eq!(
        by_id(&rows, fresh).1,
        "PENDING",
        "in-flight creations are left alone"
    );
    assert_eq!(c.pa.tenant_status(orphan_tenant), "SUSPENDED");
    assert_eq!(c.pa.tenant_status(fresh_tenant), "ACTIVE");
    assert_eq!(
        c.pa.tenant_status(recorded),
        "ACTIVE",
        "a recorded organization is never suspended"
    );
    // Idempotent: a further pass changes nothing.
    let r = reconcile_provisioning(&c.db.conn, c.pa.as_ref(), settle, 50)
        .await
        .unwrap();
    assert_eq!(r, Default::default());
}

/// ORG-013: a rename reaches the Platform Access tenant; if Platform Access is down the
/// rename still succeeds locally, stays pending, and the worker propagates it.
#[tokio::test]
async fn renames_reach_the_platform_access_tenant_and_are_retried() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (alice, _) = c.sessions.sign_in("alice@acme.example");
    let o = org(&c, &alice, "Acme").await;
    async fn pending_of(c: &Control, o: Uuid) -> i64 {
        c.count(&format!(
            "SELECT count(*) FROM devplat_organizations WHERE tenant_id = '{o}' AND platform_name_sync_pending"
        ))
        .await
    }
    assert_eq!(pending_of(&c, o).await, 0);
    let rename =
        |version: i64, name: &str| json!({ "expectedVersion": version, "displayName": name });
    let r = c
        .req(
            As::User(&alice),
            Method::PATCH,
            &format!("/v1/organizations/{o}"),
            Some(rename(1, "Acme Health")),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(c.pa.tenant_name(o), "Acme Health");
    assert_eq!(pending_of(&c, o).await, 0);
    // A support-only change does not touch Platform Access.
    let audit_before = c.pa.audit.lock().unwrap().len();
    let r = c
        .req(
            As::User(&alice),
            Method::PATCH,
            &format!("/v1/organizations/{o}"),
            Some(json!({ "expectedVersion": 2, "supportEmail": "help@acme.example" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(c.pa.audit.lock().unwrap().len(), audit_before);
    // Authority down: the rename commits locally and stays pending.
    c.pa.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = c
        .req(
            As::User(&alice),
            Method::PATCH,
            &format!("/v1/organizations/{o}"),
            Some(rename(3, "Acme Clinics")),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["displayName"], "Acme Clinics");
    assert_eq!(c.pa.tenant_name(o), "Acme Health");
    assert_eq!(pending_of(&c, o).await, 1);
    let synced = sync_pending_names(&c.db.conn, c.pa.as_ref(), chrono::Duration::zero(), 50)
        .await
        .unwrap();
    assert_eq!(synced, 0, "still down");
    assert_eq!(pending_of(&c, o).await, 1);
    c.pa.down.store(false, std::sync::atomic::Ordering::SeqCst);
    let synced = sync_pending_names(&c.db.conn, c.pa.as_ref(), chrono::Duration::zero(), 50)
        .await
        .unwrap();
    assert_eq!(synced, 1);
    assert_eq!(c.pa.tenant_name(o), "Acme Clinics");
    assert_eq!(pending_of(&c, o).await, 0);
    // A first-party organization's name stays operator-managed in Platform Access.
    let fp = c.pa.first_party_tenant();
    let reg = c
        .operator(
            Method::POST,
            "/internal/v1/operator/organizations",
            Some(json!({ "tenantId": fp, "displayName": "Internal", "ownerEmail": "ops@cf.example" })),
        )
        .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.json);
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_organizations WHERE tenant_id = '{fp}' AND platform_name_sync_pending"
        ))
        .await,
        0
    );
}

/// PLT-009: operators can list / search organizations and read every operator command
/// (organization-scoped and platform-level), paginated, with a named operator only.
#[tokio::test]
async fn the_operator_read_plane_covers_organizations_and_every_operator_command() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (alice, _) = c.sessions.sign_in("alice@acme.example");
    let acme = org(&c, &alice, "Acme 50%_Labs").await;
    let globex = org(&c, &alice, "Globex").await;
    let list = c
        .operator(Method::GET, "/internal/v1/operator/organizations", None)
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    let ids: Vec<Value> = list.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].clone())
        .collect();
    assert_eq!(ids, vec![json!(globex), json!(acme)], "newest first");
    assert!(list.json["items"][0]["createdBy"]
        .as_str()
        .unwrap()
        .starts_with("user:"));
    let search = |q: &str| format!("/internal/v1/operator/organizations?query={q}");
    let found = c.operator(Method::GET, &search("acme"), None).await;
    assert_eq!(found.json["items"].as_array().unwrap().len(), 1);
    // Wildcards match literally.
    let lit = c.operator(Method::GET, &search("50%25_l"), None).await;
    assert_eq!(
        lit.json["items"].as_array().unwrap().len(),
        1,
        "{}",
        lit.json
    );
    let none = c.operator(Method::GET, &search("%25"), None).await;
    assert_eq!(
        none.json["items"].as_array().unwrap().len(),
        1,
        "only the literal %"
    );
    let by_id = c
        .operator(Method::GET, &search(&globex.to_string()), None)
        .await;
    assert_eq!(by_id.json["items"][0]["id"], json!(globex));
    let page1 = c
        .operator(
            Method::GET,
            "/internal/v1/operator/organizations?limit=1",
            None,
        )
        .await;
    let cursor = page1.json["nextCursor"].as_str().unwrap().to_string();
    let page2 = c
        .operator(
            Method::GET,
            &format!("/internal/v1/operator/organizations?limit=1&cursor={cursor}"),
            None,
        )
        .await;
    assert_eq!(page2.json["items"][0]["id"], json!(acme));
    assert!(page2.json["nextCursor"].is_null());
    // Operator commands: one organization-scoped, one platform-level.
    let r = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{acme}/state"),
            Some(json!({ "state": "RESTRICTED", "reason": "BILLING_REVIEW" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let plan = c
        .operator(
            Method::POST,
            "/internal/v1/operator/commercial-plans",
            Some(json!({ "code": "starter", "displayName": "Starter", "entitlementProfileRef": "profile.starter",
                "entitlements": { "apiRequestsPerMinute": 300, "apiRequestsPerDay": 50000, "concurrentCalls": 10 } })),
        )
        .await;
    assert_eq!(plan.status, StatusCode::CREATED, "{}", plan.json);
    let restricted = c
        .operator(
            Method::GET,
            "/internal/v1/operator/organizations?state=RESTRICTED",
            None,
        )
        .await;
    assert_eq!(restricted.json["items"][0]["id"], json!(acme));
    let act = c
        .operator(Method::GET, "/internal/v1/operator/activity", None)
        .await;
    assert_eq!(act.status, StatusCode::OK, "{}", act.json);
    let items = act.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{}", act.json);
    assert_eq!(items[0]["action"], "commercial_plan.published");
    assert_eq!(items[0]["scope"], "PLATFORM");
    assert_eq!(items[0]["operator"], "ops.tester");
    assert_eq!(items[1]["action"], "organization.state_changed");
    assert_eq!(items[1]["scope"], "ORGANIZATION");
    assert_eq!(items[1]["organizationId"], json!(acme));
    let scoped = c
        .operator(
            Method::GET,
            &format!("/internal/v1/operator/activity?organizationId={acme}"),
            None,
        )
        .await;
    assert_eq!(scoped.json["items"].as_array().unwrap().len(), 1);
    let family = c
        .operator(
            Method::GET,
            "/internal/v1/operator/activity?action=commercial_plan.",
            None,
        )
        .await;
    assert_eq!(family.json["items"].as_array().unwrap().len(), 1);
    let paged = c
        .operator(Method::GET, "/internal/v1/operator/activity?limit=1", None)
        .await;
    let next = paged.json["nextCursor"].as_str().unwrap().to_string();
    let rest = c
        .operator(
            Method::GET,
            &format!("/internal/v1/operator/activity?limit=1&cursor={next}"),
            None,
        )
        .await;
    assert_eq!(
        rest.json["items"][0]["action"],
        "organization.state_changed"
    );
    // Malformed filters are refused.
    for bad in [
        "/internal/v1/operator/activity?action=Bad%20Action",
        "/internal/v1/operator/activity?operator=has%20space",
        "/internal/v1/operator/activity?cursor=nope",
        "/internal/v1/operator/organizations?state=SLEEPING",
        "/internal/v1/operator/organizations?limit=101",
        "/internal/v1/operator/organizations?unknown=1",
    ] {
        let r = c.operator(Method::GET, bad, None).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.json);
    }
    // A named operator is required, and only the operator console may read.
    let unnamed = c
        .internal(
            &c.operator,
            Method::GET,
            "/internal/v1/operator/activity",
            None,
        )
        .await;
    assert_eq!(unnamed.json["error"]["code"], "OPERATOR_REQUIRED");
    let gateway = c
        .internal(
            &c.peers.gateway,
            Method::GET,
            "/internal/v1/operator/organizations",
            None,
        )
        .await;
    assert_eq!(gateway.status, StatusCode::FORBIDDEN);
}
