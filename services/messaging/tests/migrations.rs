//! Part V C.3: Messaging's migrations apply to a fresh database, roll back completely,
//! re-apply, create only `messaging_` tables and no column that could hold an E2EE
//! secret or plaintext.

mod common;

use common::*;
use messaging_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, Statement};

async fn count(db: &TestDb, sql: &str) -> i64 {
    db.conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            sql.to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

#[tokio::test]
async fn migrations_apply_roll_back_and_reapply() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    let tables = count(
        &db,
        "SELECT count(*) FROM pg_tables WHERE schemaname = 'public'",
    )
    .await;
    assert_eq!(
        count(&db, "SELECT count(*) FROM pg_tables WHERE schemaname = 'public' AND tablename !~ '^messaging_'").await,
        0
    );
    assert!(
        messaging_migration::Migrator::get_pending_migrations(&db.conn)
            .await
            .unwrap()
            .is_empty()
    );
    messaging_migration::Migrator::reset(&db.conn)
        .await
        .unwrap();
    assert_eq!(
        count(&db, "SELECT count(*) FROM pg_tables WHERE schemaname = 'public' AND tablename <> 'messaging_seaql_migrations'").await,
        0,
        "down migrations leave nothing behind"
    );
    assert_eq!(count(&db, "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'public'").await, 0);
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'public'"
        )
        .await,
        tables
    );
}

#[tokio::test]
async fn no_column_can_hold_e2ee_secrets_or_plaintext() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let suspicious = count(
        &db,
        "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' AND column_name ~ \
         '(private|secret|plaintext|epoch|body|content|text|recovery|attachment_key|media_key|password|phone|msisdn|e164)'",
    )
    .await;
    assert_eq!(suspicious, 0);
}

#[tokio::test]
async fn other_databases_are_untouched() {
    // Migrations run against Messaging's database only: a sibling database on the same
    // server gains no table.
    let Some(sibling) = TestDb::empty().await else {
        return;
    };
    let Some(db) = TestDb::empty().await else {
        return;
    };
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(
        count(
            &sibling,
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'public'"
        )
        .await,
        0
    );
}
