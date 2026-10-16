//! Part IV C3 / C6 / D9 / D10.18: Numbering's independent database — fresh migration,
//! down / re-up, upgrade with data, the exact table set (no unused KYC / SMS / portability
//! / fraud / billing tables), no foreign key leaving Numbering's own tables, and a
//! least-privilege runtime role that can run the whole domain but no DDL.
mod common;

use std::collections::BTreeSet;

use common::{scalar, scope, strings, Harness, TestDb};
use numbering::{
    domain::{ManualClock, RequestContext},
    infra::{jobs, outbox},
    numbers::{EnvironmentKind, OwnershipClass},
};
use numbering_migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, Database};
use serde_json::json;
use uuid::Uuid;

const FOUNDATION: &[&str] = &[
    "numbering_idempotency_keys",
    "numbering_jobs",
    "numbering_outbox_events",
];
const DOMAIN: &[&str] = &[
    "dcn_assignments",
    "number_acquisitions",
    "number_assignment_history",
    "number_capabilities",
    "number_catalog_revisions",
    "number_country_catalog",
    "number_eligibility_decisions",
    "number_inventory_refreshes",
    "number_inventory_snapshots",
    "number_kyc_appeals",
    "number_kyc_cases",
    "number_kyc_decisions",
    "number_kyc_evidence_access",
    "number_kyc_evidence_refs",
    "number_kyc_requirements",
    "number_kyc_verifications",
    "number_lifecycle_events",
    "number_provider_bindings",
    "number_provider_command_attempts",
    "number_provider_commands",
    "number_provider_events",
    "number_provider_inventory_refs",
    "number_reservations",
    "number_resources",
    "number_route_configurations",
    "number_route_readiness",
    "number_routing_profiles",
    "number_search_windows",
    "number_type_catalog",
    "numbering_incidents",
];

async fn tables(conn: &sea_orm::DatabaseConnection) -> BTreeSet<String> {
    strings(
        conn,
        "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
    )
    .await
    .into_iter()
    .collect()
}

fn expected(with_domain: bool) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = FOUNDATION.iter().map(|s| s.to_string()).collect();
    set.insert("numbering_seaql_migrations".into());
    if with_domain {
        set.extend(DOMAIN.iter().map(|s| s.to_string()));
    }
    set
}

#[tokio::test]
async fn fresh_migration_creates_exactly_numbering_tables_and_down_removes_them() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    Migrator::up(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db.conn).await, expected(true));
    assert!(Migrator::get_pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
    // Its own migration history table: never shared with another service.
    assert_eq!(
        scalar(&db.conn, "SELECT count(*) FROM numbering_seaql_migrations").await,
        Migrator::migrations().len() as i64
    );
    // Nothing that later workstreams own yet (SMS, portability, fraud, billing), nothing of
    // another service. (KYC is Workstream F's.)
    for t in tables(&db.conn).await {
        for forbidden in [
            "sms",
            "port",
            "fraud",
            "billing",
            "invoice",
            "call",
            "credential",
            "tenant",
            "relay",
        ] {
            assert!(!t.contains(forbidden), "{t} looks like a {forbidden} table");
        }
        assert!(
            t.starts_with("number_") || t.starts_with("numbering_") || t.starts_with("dcn_"),
            "{t} is not a Numbering table"
        );
    }
    // Every foreign key stays inside Numbering's own tables (no cross-service FK).
    let fks = strings(
        &db.conn,
        "SELECT c.conrelid::regclass::text || ' -> ' || c.confrelid::regclass::text FROM pg_constraint c \
          WHERE c.contype = 'f' AND c.connamespace = 'public'::regnamespace",
    )
    .await;
    assert!(!fks.is_empty());
    for fk in &fks {
        let target = fk.split(" -> ").nth(1).unwrap();
        assert!(
            DOMAIN.contains(&target),
            "{fk} leaves Numbering's domain tables"
        );
    }
    // Down removes everything (tables and functions), and up is repeatable.
    Migrator::down(&db.conn, None).await.unwrap();
    assert_eq!(
        tables(&db.conn).await,
        BTreeSet::from(["numbering_seaql_migrations".to_string()])
    );
    assert_eq!(
        scalar(
            &db.conn,
            "SELECT count(*) FROM pg_proc WHERE pronamespace = 'public'::regnamespace \
               AND (proname LIKE 'number%' OR proname LIKE 'dcn%')"
        )
        .await,
        0
    );
    Migrator::up(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db.conn).await, expected(true));
}

#[tokio::test]
async fn upgrading_from_the_foundation_keeps_its_data_and_the_domain_works() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    Migrator::up(&db.conn, Some(1)).await.unwrap();
    assert_eq!(tables(&db.conn).await, expected(false));
    // Foundation data written before the upgrade.
    let ctx = RequestContext::generate();
    let now = chrono::Utc::now();
    let event = outbox::enqueue(
        &db.conn,
        outbox::NewEvent {
            event_type: "number.foundation.recorded",
            aggregate_type: "TEST",
            aggregate_id: Uuid::new_v4(),
            scope: None,
            payload: json!({ "n": 1 }),
        },
        &ctx,
        now,
    )
    .await
    .unwrap();
    let (job, _) = jobs::enqueue(
        &db.conn,
        jobs::NewJob {
            kind: "inventory.refresh",
            dedupe_key: Some("before-upgrade".into()),
            payload: json!({}),
            retry_safety: jobs::RetrySafety::Idempotent,
            max_attempts: 3,
            run_at: now,
        },
        &ctx,
        now,
    )
    .await
    .unwrap();
    Migrator::up(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db.conn).await, expected(true));
    assert_eq!(
        scalar(
            &db.conn,
            &format!("SELECT count(*) FROM numbering_outbox_events WHERE id = '{event}'")
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &db.conn,
            &format!("SELECT count(*) FROM numbering_jobs WHERE id = '{job}'")
        )
        .await,
        1
    );
    // The upgraded database runs the whole domain.
    let db = std::sync::Arc::new(db);
    let h = Harness::build(db, None, ManualClock::new(common::t0()));
    let j = h
        .active(&scope(EnvironmentKind::Sandbox), OwnershipClass::Personal)
        .await;
    assert_eq!(j.assignment.status, "ACTIVE");
}

#[tokio::test]
async fn a_least_privilege_runtime_role_runs_the_domain_but_no_ddl() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // The production split (provision-service-database.sh): the runtime role has DML on
    // Numbering's tables and nothing else.
    let role = format!("nb_rt_{}", Uuid::new_v4().simple());
    let password = Uuid::new_v4().simple().to_string();
    let db_name = h.db.url.rsplit('/').next().unwrap().to_string();
    for sql in [
        format!("CREATE ROLE {role} LOGIN PASSWORD '{password}'"),
        format!("GRANT CONNECT ON DATABASE {db_name} TO {role}"),
        format!("GRANT USAGE ON SCHEMA public TO {role}"),
        format!("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {role}"),
        format!("GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO {role}"),
        format!("GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO {role}"),
        "REVOKE CREATE ON SCHEMA public FROM PUBLIC".to_string(),
    ] {
        h.conn().execute_unprepared(&sql).await.unwrap();
    }
    let admin = h.db.admin_url().to_string();
    let (base, _) = admin.rsplit_once('/').unwrap();
    let host = base.rsplit_once('@').unwrap().1;
    let runtime_url = format!("postgres://{role}:{password}@{host}/{db_name}");
    let runtime = Database::connect(&runtime_url).await.unwrap();
    let rh = Harness::build(
        std::sync::Arc::new(TestDb::attach(runtime.clone(), runtime_url.clone())),
        None,
        ManualClock::new(common::t0()),
    );
    let j = rh
        .active(
            &scope(EnvironmentKind::Production),
            OwnershipClass::Organization,
        )
        .await;
    assert_eq!(j.assignment.status, "ACTIVE");
    for ddl in [
        "CREATE TABLE stolen (id int)",
        "ALTER TABLE number_resources ADD COLUMN x int",
        "DROP TABLE number_assignment_history",
        "TRUNCATE numbering_outbox_events",
        "DROP TRIGGER trg_number_resources_guard ON number_resources",
        "ALTER TABLE number_resources DISABLE TRIGGER ALL",
    ] {
        assert!(runtime.execute_unprepared(ddl).await.is_err(), "{ddl}");
    }
    // The guards bind the runtime role exactly like any other session.
    assert!(runtime
        .execute_unprepared(&format!(
            "UPDATE number_resources SET state = 'AVAILABLE' WHERE id = '{}'",
            j.resource_id
        ))
        .await
        .is_err());
    runtime.close().await.ok();
    h.conn()
        .execute_unprepared(&format!("REASSIGN OWNED BY {role} TO CURRENT_USER"))
        .await
        .ok();
    h.conn()
        .execute_unprepared(&format!("DROP OWNED BY {role}"))
        .await
        .ok();
    // The role lives in the cluster: dropped here (the database is dropped by TestDb).
    let admin_conn = Database::connect(&admin).await.unwrap();
    drop(rh);
    admin_conn
        .execute_unprepared(&format!("DROP DATABASE IF EXISTS {db_name} WITH (FORCE)"))
        .await
        .ok();
    admin_conn
        .execute_unprepared(&format!("DROP ROLE IF EXISTS {role}"))
        .await
        .unwrap();
}
