//! Schema foundation: migrations apply, roll back and re-apply; every table is in the
//! Organizations domain; the runtime refuses pending migrations.

mod common;

use common::TestDb;
use organizations_migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, Statement};

async fn tables(db: &TestDb) -> Vec<String> {
    db.conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect()
}

#[tokio::test]
async fn migrations_apply_roll_back_and_reapply_and_own_only_organization_tables() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let pending = organizations_migration::pending_migrations(&db.conn)
        .await
        .unwrap();
    assert_eq!(pending.len(), Migrator::migrations().len());
    assert!(
        organizations::connect(&db.url, 2).await.is_err(),
        "pending migrations refuse startup"
    );
    Migrator::up(&db.conn, None).await.unwrap();
    assert!(organizations_migration::pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
    let names = tables(&db).await;
    for expected in [
        "organizations",
        "organization_profiles",
        "organization_memberships",
        "organization_membership_history",
        "organization_invitations",
        "organization_audit_events",
        "organization_outbox",
        "organization_idempotency_records",
        "organization_service_nonces",
        "organization_import_runs",
        "organization_platform_scopes",
        "organization_provisioning",
        "organization_scope_service_bindings",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
    for n in &names {
        assert!(
            n == "organizations"
                || n.starts_with("organization_")
                || n == organizations_migration::MIGRATION_TABLE,
            "{n} is outside the Organizations domain"
        );
    }
    organizations::connect(&db.url, 2).await.unwrap();
    Migrator::reset(&db.conn).await.unwrap();
    assert_eq!(
        tables(&db).await,
        vec![organizations_migration::MIGRATION_TABLE.to_string()]
    );
    Migrator::up(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db).await.len(), names.len());
}

#[tokio::test]
async fn partial_uniques_hold_one_current_membership_and_one_pending_invitation() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let run = |sql: &'static str| {
        let conn = db.conn.clone();
        async move { conn.execute_unprepared(sql).await }
    };
    run("INSERT INTO organizations (id, name, slug, creation_source, created_at, updated_at) VALUES ('00000000-0000-4000-8000-000000000001', 'A', 'abc', 'IMPORTED', now(), now())").await.unwrap();
    run("INSERT INTO organization_memberships (id, organization_id, user_id, role, status, joined_via, created_at, updated_at) VALUES (gen_random_uuid(), '00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-0000000000a1', 'OWNER', 'ACTIVE', 'IMPORTED', now(), now())").await.unwrap();
    assert!(run("INSERT INTO organization_memberships (id, organization_id, user_id, role, status, joined_via, created_at, updated_at) VALUES (gen_random_uuid(), '00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-0000000000a1', 'MEMBER', 'SUSPENDED', 'IMPORTED', now(), now())").await.is_err());
    // Removed history rows may repeat.
    for _ in 0..2 {
        run("INSERT INTO organization_memberships (id, organization_id, user_id, role, status, joined_via, created_at, updated_at, removed_at) VALUES (gen_random_uuid(), '00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-0000000000a1', 'MEMBER', 'REMOVED', 'IMPORTED', now(), now(), now())").await.unwrap();
    }
    run("INSERT INTO organization_invitations (id, organization_id, token_hash, email_normalized, email_hint, role, invited_by, created_at) VALUES (gen_random_uuid(), '00000000-0000-4000-8000-000000000001', repeat('a', 64), 'p@example.com', 'p***@example.com', 'MEMBER', gen_random_uuid(), now())").await.unwrap();
    assert!(run("INSERT INTO organization_invitations (id, organization_id, token_hash, email_normalized, email_hint, role, invited_by, created_at) VALUES (gen_random_uuid(), '00000000-0000-4000-8000-000000000001', repeat('b', 64), 'p@example.com', 'p***@example.com', 'MEMBER', gen_random_uuid(), now())").await.is_err());
    // OWNER is not invitable; an imported slug may be legacy-shaped but never empty.
    assert!(run("INSERT INTO organization_invitations (id, organization_id, token_hash, email_normalized, email_hint, role, invited_by, created_at) VALUES (gen_random_uuid(), '00000000-0000-4000-8000-000000000001', repeat('c', 64), 'q@example.com', 'q***@example.com', 'OWNER', gen_random_uuid(), now())").await.is_err());
    assert!(run("INSERT INTO organizations (id, name, slug, creation_source, created_at, updated_at) VALUES (gen_random_uuid(), 'B', '-bad', 'IMPORTED', now(), now())").await.is_err());
    // The default invitation expiry is 7 days.
    let row = db
        .conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT (expires_at - created_at) = interval '7 days' AS ok FROM organization_invitations WHERE token_hash = repeat('a', 64)",
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(row.try_get::<bool>("", "ok").unwrap());
}
