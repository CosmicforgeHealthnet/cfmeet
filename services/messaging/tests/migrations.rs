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
            "m20261211_000039_member_notifications".to_string(),
            "m20261212_000040_inbox_and_chat_controls".to_string(),
            "m20261213_000041_admin_operations".to_string(),
            "m20261214_000042_owner_delivered_exports".to_string(),
            "m20261215_000043_admin_bridge_events".to_string(),
            "m20261216_000044_chat_organization_polls".to_string()
        ]
    );
    messaging_migration::MlsJoinMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(
        messaging_migration::pending_migrations(&db.conn)
            .await
            .unwrap(),
        vec![
            "m20261211_000039_member_notifications".to_string(),
            "m20261212_000040_inbox_and_chat_controls".to_string(),
            "m20261213_000041_admin_operations".to_string(),
            "m20261214_000042_owner_delivered_exports".to_string(),
            "m20261215_000043_admin_bridge_events".to_string(),
            "m20261216_000044_chat_organization_polls".to_string()
        ]
    );
    messaging_migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatControlsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::AdminOperationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatOrganizationMigrator::up(&db.conn, None)
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
        vec![
            "m20261211_000039_member_notifications".to_string(),
            "m20261212_000040_inbox_and_chat_controls".to_string(),
            "m20261213_000041_admin_operations".to_string(),
            "m20261214_000042_owner_delivered_exports".to_string(),
            "m20261215_000043_admin_bridge_events".to_string(),
            "m20261216_000044_chat_organization_polls".to_string()
        ]
    );
    messaging_migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::member_notifications_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
    // The chat-controls history is independent of this one.
    assert_eq!(
        messaging_migration::pending_migrations(&db.conn)
            .await
            .unwrap(),
        vec![
            "m20261212_000040_inbox_and_chat_controls".to_string(),
            "m20261213_000041_admin_operations".to_string(),
            "m20261214_000042_owner_delivered_exports".to_string(),
            "m20261215_000043_admin_bridge_events".to_string(),
            "m20261216_000044_chat_organization_polls".to_string()
        ]
    );
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
    assert!(messaging_migration::member_notifications_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
}

/// Chat controls live in their OWN history: the previous image still finds exactly its
/// histories (it never reads this one), the history rolls back cleanly (the retention
/// "only tightens" trigger comes back) and re-applies.
#[tokio::test]
async fn the_chat_controls_history_is_separate_and_rolls_back() {
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
    assert!(messaging_migration::mls_join_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
    messaging_migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(
        messaging_migration::chat_controls_pending(&db.conn)
            .await
            .unwrap(),
        vec!["m20261212_000040_inbox_and_chat_controls".to_string()]
    );
    let trigger = "SELECT count(*) FROM pg_trigger WHERE tgname = 'trg_messaging_conversation_retention_tightens'";
    assert_eq!(count(&db, trigger).await, 1);
    messaging_migration::ChatControlsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::AdminOperationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatOrganizationMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(count(&db, trigger).await, 0);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM messaging_chat_controls_seaql_migrations"
        )
        .await,
        1
    );
    messaging_migration::ChatControlsMigrator::reset(&db.conn)
        .await
        .unwrap();
    assert_eq!(count(&db, trigger).await, 1);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM pg_tables WHERE tablename IN ('messaging_conversation_inbox_preferences', 'messaging_policy_invalidations')"
        )
        .await,
        0
    );
    messaging_migration::ChatControlsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::AdminOperationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatOrganizationMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
}

/// The admin-bridge operations ledger lives in its OWN history: the previous image never
/// reads it, this image requires it before it reports ready, and it rolls back on its own
/// (the ledger and the report / legal-hold versions go, nothing else).
#[tokio::test]
async fn the_admin_operations_history_is_separate_and_rolls_back() {
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
    messaging_migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatControlsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(
        messaging_migration::pending_migrations(&db.conn)
            .await
            .unwrap(),
        vec![
            "m20261213_000041_admin_operations".to_string(),
            "m20261214_000042_owner_delivered_exports".to_string(),
            "m20261215_000043_admin_bridge_events".to_string(),
            "m20261216_000044_chat_organization_polls".to_string()
        ]
    );
    messaging_migration::AdminOperationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatOrganizationMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
    let versions = "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public'          AND column_name = 'version' AND table_name IN ('messaging_reports', 'messaging_legal_holds')";
    assert_eq!(count(&db, versions).await, 2);
    // Append-only.
    db.conn
        .execute_unprepared(
            "INSERT INTO messaging_admin_operations (idempotency_key, command, subject_kind, subject_id, operator_ref,              resulting_state, resource, recorded_at) VALUES ('7d1f0c2a-0000-4000-8000-000000000001', 'messaging.report.dismiss', 'REPORT',              'r-1', 'hq:staff-1', 'DISMISSED', '{}'::jsonb, now())",
        )
        .await
        .unwrap();
    assert!(db
        .conn
        .execute_unprepared("UPDATE messaging_admin_operations SET resulting_state = 'OPEN'")
        .await
        .is_err());
    assert!(db
        .conn
        .execute_unprepared("DELETE FROM messaging_admin_operations")
        .await
        .is_err());
    messaging_migration::AdminOperationsMigrator::reset(&db.conn)
        .await
        .unwrap();
    assert_eq!(count(&db, versions).await, 0);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM pg_tables WHERE tablename = 'messaging_admin_operations'"
        )
        .await,
        0
    );
    messaging_migration::AdminOperationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatOrganizationMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::admin_operations_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
}

/// Chat organization (favourites, chat lists), MANAGED polls and group descriptions live in
/// their OWN history: the previous image never reads it, this image requires it before it
/// reports ready, and it rolls back on its own (its tables and columns go, nothing else).
#[tokio::test]
async fn the_chat_organization_history_is_separate_and_rolls_back() {
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
    messaging_migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatControlsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::AdminOperationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(
        messaging_migration::pending_migrations(&db.conn)
            .await
            .unwrap(),
        vec!["m20261216_000044_chat_organization_polls".to_string()]
    );
    messaging_migration::ChatOrganizationMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
    let tables = "SELECT count(*) FROM pg_tables WHERE tablename IN ('messaging_chat_lists',          'messaging_chat_list_items', 'messaging_polls', 'messaging_poll_votes')";
    let columns = "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public'          AND ((table_name = 'messaging_conversation_inbox_preferences' AND column_name = 'favourited_at')            OR (table_name = 'messaging_conversations' AND column_name = 'description'))";
    assert_eq!(count(&db, tables).await, 4);
    assert_eq!(count(&db, columns).await, 2);
    messaging_migration::ChatOrganizationMigrator::reset(&db.conn)
        .await
        .unwrap();
    assert_eq!(count(&db, tables).await, 0);
    assert_eq!(count(&db, columns).await, 0);
    // The earlier histories are untouched by the rollback.
    assert!(messaging_migration::admin_operations_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
    assert!(messaging_migration::chat_controls_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
    messaging_migration::ChatOrganizationMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(messaging_migration::chat_organization_pending(&db.conn)
        .await
        .unwrap()
        .is_empty());
}
