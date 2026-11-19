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
    // `mls_epoch` is the one admitted name: the epoch NUMBER is public MLS framing
    // (RFC 9420 §6.3.1, in the clear in every PrivateMessage) that a Delivery Service
    // orders on. Epoch SECRETS, and any other `epoch` column, stay refused.
    const SUSPICIOUS: &str = "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' AND column_name ~ \
         '(private|secret|plaintext|epoch|recovery|attachment_key|media_key|password|phone|msisdn|e164|(^|_)(body|content|text)(_|$))' \
         AND column_name <> 'mls_epoch'";
    assert_eq!(count(&db, SUSPICIOUS).await, 0);
    let epochs = count(
        &db,
        "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' AND column_name = 'mls_epoch'",
    )
    .await;
    assert!(epochs >= 3, "the guard sees the real schema");
    // The exception is exact: a secret-looking epoch column is still caught.
    db.conn
        .execute_unprepared(
            "CREATE TABLE messaging_guard_probe (epoch_secret BYTEA, mls_epoch_key BYTEA)",
        )
        .await
        .unwrap();
    assert_eq!(count(&db, SUSPICIOUS).await, 2);
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

/// MLS self-join lives in its OWN history: the previous image (which fails closed on any
/// gap-closure migration it does not know) still finds exactly its histories after this
/// one is applied, and this image requires the new history before it reports ready.
#[tokio::test]
async fn the_mls_join_history_is_separate_so_a_rollback_still_starts() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::GapClosureMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(
        messaging_migration::pending_migrations(&db.conn)
            .await
            .unwrap(),
        vec![
            "m20261210_000038_mls_group_infos".to_string(),
            "m20261211_000039_member_notifications".to_string()
        ]
    );
    messaging_migration::MlsJoinMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::mls_join_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
    // What the previous image checks is untouched by this history.
    assert!(messaging_migration::gap_closure_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM messaging_gap_closure_seaql_migrations"
        )
        .await,
        messaging_migration::GapClosureMigrator::migrations().len() as i64
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM messaging_mls_join_seaql_migrations"
        )
        .await,
        1
    );
    messaging_migration::MlsJoinMigrator::reset(&db.conn)
        .await
        .unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM pg_tables WHERE tablename = 'messaging_mls_group_infos'"
        )
        .await,
        0
    );
    messaging_migration::MlsJoinMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::mls_join_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
}

/// The member-notification outbox lives in its OWN history too: the previous image finds
/// exactly the histories it knows after this one is applied, this image requires it before
/// it reports ready, and it rolls back on its own.
#[tokio::test]
async fn the_member_notification_history_is_separate_so_a_rollback_still_starts() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::GapClosureMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::MlsJoinMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(
        messaging_migration::pending_migrations(&db.conn)
            .await
            .unwrap(),
        vec!["m20261211_000039_member_notifications".to_string()]
    );
    messaging_migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
    // What the previous image checks is untouched by this history.
    assert!(messaging_migration::gap_closure_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
    assert!(messaging_migration::mls_join_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
    messaging_migration::MemberNotificationsMigrator::reset(&db.conn)
        .await
        .unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM pg_tables WHERE tablename = 'messaging_member_notifications'"
        )
        .await,
        0
    );
    messaging_migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
}
