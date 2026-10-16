//! Telephony migrations (Part E §178–§180): up / down / up on a clean database, the
//! expected tables only (none of Calling's or Privacy Relay's), history in Telephony's
//! own bookkeeping table.

mod common;

use common::TestDb;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use telephony_migration::{MigratorTrait, MIGRATION_TABLE};

const TABLES: &[&str] = &[
    "provider_call_bindings",
    "provider_events",
    "sip_peer_config_refs",
    "telecom_destination_classes",
    "telecom_number_bindings",
    "telecom_providers",
    "telecom_routes",
    "telephony_bridge_legs",
    "telephony_bridges",
    "telephony_captures",
    "telephony_commands",
    "telephony_enterprise_devices",
    "telephony_incidents",
    "telephony_ingress_windows",
    "telephony_legs",
    "telephony_operator_audit",
    "telephony_outbox",
    "telephony_reconciliation_runs",
    "telephony_retention_audit",
    "telephony_retention_holds",
    "telephony_usage_records",
];
const MIGRATIONS: usize = 16;

async fn tables(conn: &DatabaseConnection) -> Vec<String> {
    let rows = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' \
             AND table_type = 'BASE TABLE' ORDER BY 1"
                .to_string(),
        ))
        .await
        .unwrap();
    rows.into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect()
}

#[tokio::test]
async fn migrations_apply_roll_back_and_reapply() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let conn = &db.conn;
    telephony_migration::Migrator::up(conn, None).await.unwrap();
    let mut expected: Vec<String> = TABLES.iter().map(|s| s.to_string()).collect();
    expected.push(MIGRATION_TABLE.to_string());
    expected.sort();
    assert_eq!(tables(conn).await, expected);
    telephony_migration::Migrator::down(conn, None)
        .await
        .unwrap();
    assert_eq!(tables(conn).await, vec![MIGRATION_TABLE.to_string()]);
    assert_eq!(
        telephony_migration::Migrator::get_pending_migrations(conn)
            .await
            .unwrap()
            .len(),
        MIGRATIONS
    );
    telephony_migration::Migrator::up(conn, None).await.unwrap();
    assert_eq!(tables(conn).await, expected);
    assert_eq!(
        telephony_migration::Migrator::get_applied_migrations(conn)
            .await
            .unwrap()
            .len(),
        MIGRATIONS
    );
}

#[test]
fn telephony_creates_no_calling_or_privacy_relay_authority() {
    for foreign in [
        "calls",
        "call_participants",
        "call_sessions",
        "call_legs",
        "call_routes",
        "communication_contexts",
        "privacy_relay_sessions",
        "protected_contacts",
        "contact_fingerprints",
        "relay_number_inventory",
        "relay_number_assignments",
        "meeting_spaces",
        "users",
    ] {
        assert!(!TABLES.contains(&foreign), "{foreign}");
    }
    assert_eq!(MIGRATION_TABLE, "telephony_seaql_migrations");
}
