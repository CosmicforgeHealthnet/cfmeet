//! Privacy Relay owns its migration history and exactly its table family.

mod common;

use common::*;
use privacy_relay_migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, Statement};

const TABLES: &[&str] = &[
    "communication_context_channels",
    "communication_context_directions",
    "communication_context_participants",
    "communication_contexts",
    "contact_access_audit",
    "privacy_relay_commands",
    "privacy_relay_enforcement_jobs",
    "privacy_relay_outbox",
    "privacy_relay_seaql_migrations",
    "privacy_relay_session_channels",
    "privacy_relay_session_directions",
    "privacy_relay_session_participants",
    "privacy_relay_sessions",
    "protected_contacts",
    "relay_audit_events",
    "relay_authorization_decisions",
];

async fn tables(conn: &sea_orm::DatabaseConnection) -> Vec<String> {
    conn.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
    .collect()
}

#[tokio::test]
async fn up_down_up_creates_exactly_the_privacy_relay_tables() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    Migrator::up(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db.conn).await, TABLES);
    assert_eq!(
        scalar(
            &db.conn,
            "SELECT count(*) FROM privacy_relay_seaql_migrations"
        )
        .await,
        6
    );
    Migrator::down(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db.conn).await, ["privacy_relay_seaql_migrations"]);
    assert_eq!(
        scalar(&db.conn, "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'public' AND p.proname LIKE 'privacy_relay_%'").await,
        0,
        "down removes every function"
    );
    Migrator::up(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db.conn).await, TABLES);
    for forbidden in [
        "relay_number_inventory",
        "relay_number_assignments",
        "calls",
        "call_sessions",
        "platform_api_credentials",
        "platform_principals",
        "meeting_spaces",
        "users",
    ] {
        assert!(!TABLES.contains(&forbidden), "{forbidden}");
    }
    // Every tenant-owned table carries explicit ownership columns.
    for table in TABLES
        .iter()
        .filter(|t| **t != "privacy_relay_seaql_migrations")
    {
        for column in ["tenant_id", "application_id", "environment_id"] {
            assert_eq!(
                scalar(&db.conn, &format!("SELECT count(*) FROM information_schema.columns WHERE table_name = '{table}' AND column_name = '{column}'")).await,
                1,
                "{table}.{column}"
            );
        }
    }
}

#[tokio::test]
async fn child_rows_cannot_cross_ownership() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (t, a, e, t2) = (
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
    );
    let ctx = uuid::Uuid::new_v4();
    exec(&db.conn, &format!(
        "INSERT INTO communication_contexts (id, tenant_id, application_id, environment_id, environment_kind, \
           external_context_reference, version, status, direction_policy, allowed_relay_strategies, starts_at, expires_at, \
           policy_version, policy_snapshot, created_by_service_account_id, created_at, updated_at) \
         VALUES ('{ctx}', '{t}', '{a}', '{e}', 'TEST', 'case-1', 1, 'ACTIVE', 'BIDIRECTIONAL', ARRAY['APP_ONLY'], now(), now() + interval '1 hour', \
           'CONTEXT_POLICY_V1', '{{\"terminate_active_calls_on_revoke\": true, \"terminate_active_calls_on_participant_removal\": true, \"terminate_active_calls_on_suspend\": false, \"grace_allows_new_calls\": false}}', \
           gen_random_uuid(), now(), now())"
    )).await.unwrap();
    // A participant claiming another tenant's ownership of this context is refused (FK).
    let foreign = exec(&db.conn, &format!(
        "INSERT INTO communication_context_participants (id, context_id, tenant_id, application_id, environment_id, subject_kind, principal_ref, added_in_version, added_at) \
         VALUES (gen_random_uuid(), '{ctx}', '{t2}', '{a}', '{e}', 'PRINCIPAL', gen_random_uuid(), 1, now())"
    )).await;
    assert!(foreign.is_err());
    // Two subject references, or none, are refused.
    let both = exec(&db.conn, &format!(
        "INSERT INTO communication_context_participants (id, context_id, tenant_id, application_id, environment_id, subject_kind, principal_ref, contact_ref, added_in_version, added_at) \
         VALUES (gen_random_uuid(), '{ctx}', '{t}', '{a}', '{e}', 'PRINCIPAL', gen_random_uuid(), gen_random_uuid(), 1, now())"
    )).await;
    assert!(both.is_err());
    // An arbitrary policy document is refused by the database too.
    let blob = exec(&db.conn, &format!(
        "UPDATE communication_contexts SET policy_snapshot = '{{\"anything\": {{\"nested\": true}}}}' WHERE id = '{ctx}'"
    )).await;
    assert!(blob.is_err());
    // Duplicate active membership is refused.
    let p = uuid::Uuid::new_v4();
    let insert = format!(
        "INSERT INTO communication_context_participants (id, context_id, tenant_id, application_id, environment_id, subject_kind, principal_ref, added_in_version, added_at) \
         VALUES (gen_random_uuid(), '{ctx}', '{t}', '{a}', '{e}', 'PRINCIPAL', '{p}', 1, now())"
    );
    exec(&db.conn, &insert).await.unwrap();
    assert!(exec(&db.conn, &insert).await.is_err());
    // A version can only move forward by one.
    assert!(exec(
        &db.conn,
        &format!("UPDATE communication_contexts SET version = 3 WHERE id = '{ctx}'")
    )
    .await
    .is_err());
    assert!(exec(
        &db.conn,
        &format!("UPDATE communication_contexts SET version = 2 WHERE id = '{ctx}'")
    )
    .await
    .is_ok());
}
