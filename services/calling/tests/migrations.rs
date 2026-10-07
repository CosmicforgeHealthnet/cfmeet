//! A. Migration certification: clean up / down / up, required constraints, no plaintext
//! secret or contact columns, no Part C tables, no Meeting Service tables.

mod common;

use calling_migration::{MigratorTrait, MIGRATION_TABLE};
use common::TestDb;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

async fn strings(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    conn.query_all(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect()
}

async fn tables(conn: &DatabaseConnection) -> Vec<String> {
    strings(
        conn,
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
    )
    .await
}

const PLATFORM_TABLES: &[&str] = &[
    "platform_api_credential_scopes",
    "platform_api_credentials",
    "platform_application_limits",
    "platform_application_scopes",
    "platform_applications",
    "platform_credential_limits",
    "platform_environments",
    "platform_principals",
    "platform_request_rate_windows",
    "platform_service_accounts",
    "platform_tenants",
];

#[tokio::test]
async fn migrations_apply_roll_back_and_reapply_on_a_clean_database() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let conn = &db.conn;
    assert!(tables(conn).await.is_empty());

    calling_migration::Migrator::up(conn, None).await.unwrap();
    let mut expected: Vec<String> = PLATFORM_TABLES.iter().map(|s| s.to_string()).collect();
    expected.push(MIGRATION_TABLE.to_string());
    expected.sort();
    assert_eq!(tables(conn).await, expected);
    assert!(calling_migration::Migrator::get_pending_migrations(conn)
        .await
        .unwrap()
        .is_empty());

    // Full rollback (reset): only the migration bookkeeping table remains, and the shared
    // guard functions are gone.
    calling_migration::Migrator::down(conn, None).await.unwrap();
    assert_eq!(tables(conn).await, vec![MIGRATION_TABLE.to_string()]);
    let functions = strings(
        conn,
        "SELECT proname::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
         WHERE n.nspname = 'public' AND proname LIKE 'platform\\_%'",
    )
    .await;
    assert!(functions.is_empty(), "{functions:?}");
    assert_eq!(
        calling_migration::Migrator::get_pending_migrations(conn)
            .await
            .unwrap()
            .len(),
        4
    );

    // And up again.
    calling_migration::Migrator::up(conn, None).await.unwrap();
    assert_eq!(tables(conn).await, expected);
    let applied = calling_migration::Migrator::get_applied_migrations(conn)
        .await
        .unwrap();
    assert_eq!(applied.len(), 4);
}

#[tokio::test]
async fn migration_history_is_owned_by_calling() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let all = tables(&db.conn).await;
    assert!(all.contains(&MIGRATION_TABLE.to_string()));
    assert!(
        !all.contains(&"seaql_migrations".to_string()),
        "Calling must not share the default migration table"
    );
}

#[tokio::test]
async fn required_constraints_and_guards_exist() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let constraints = strings(
        &db.conn,
        "SELECT conname::text FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace \
         WHERE n.nspname = 'public'",
    )
    .await;
    for required in [
        // Uniqueness
        "uq_platform_tenants_slug",
        "uq_platform_applications_tenant_slug",
        "uq_platform_environments_application_kind",
        "uq_platform_service_accounts_environment_name",
        "uq_platform_api_credentials_public_id",
        "uq_platform_principals_scoped_reference",
        "uq_platform_api_credentials_rotated_from",
        "uq_platform_api_credentials_superseded_by",
        // Ownership-consistent composite foreign keys
        "fk_platform_applications_tenant",
        "fk_platform_environments_application",
        "fk_platform_service_accounts_environment",
        "fk_platform_api_credentials_service_account",
        "fk_platform_api_credentials_rotated_from",
        "fk_platform_api_credential_scopes_credential",
        "fk_platform_api_credential_scopes_application_scope",
        "fk_platform_principals_environment",
        "fk_platform_principals_service_account",
        "fk_platform_application_limits_application",
        "fk_platform_credential_limits_credential",
        // Status / enum checks
        "ck_platform_tenants_classification",
        "ck_platform_tenants_status",
        "ck_platform_applications_status",
        "ck_platform_environments_kind",
        "ck_platform_environments_status",
        "ck_platform_service_accounts_status",
        "ck_platform_api_credentials_status",
        "ck_platform_principals_kind",
        "ck_platform_principals_status",
        // Security invariants
        "ck_platform_environments_live_telecom",
        "ck_platform_api_credentials_verifier_length",
        "ck_platform_api_credentials_verifier_algorithm",
        "ck_platform_api_credentials_supersession_never_extends",
        "ck_platform_api_credentials_revoked",
        "ck_platform_principals_reference_not_telephone",
        "ck_platform_principals_telephone_endpoint_opaque",
        "ck_platform_principals_service_account_link",
        "ck_platform_principals_service_account_reference",
    ] {
        assert!(
            constraints.iter().any(|c| c == required),
            "missing constraint {required}"
        );
    }

    let triggers = strings(
        &db.conn,
        "SELECT tgname::text FROM pg_trigger WHERE NOT tgisinternal ORDER BY tgname",
    )
    .await;
    for required in [
        "trg_platform_tenants_immutable",
        "trg_platform_applications_immutable",
        "trg_platform_environments_immutable",
        "trg_platform_service_accounts_immutable",
        "trg_platform_service_accounts_revoked_terminal",
        "trg_platform_api_credentials_immutable",
        "trg_platform_api_credentials_set_once",
        "trg_platform_api_credentials_revoked_terminal",
        "trg_platform_principals_immutable",
    ] {
        assert!(
            triggers.iter().any(|t| t == required),
            "missing trigger {required}"
        );
    }

    // Every timestamp is TIMESTAMPTZ; every id is a UUID primary key.
    let naive = strings(
        &db.conn,
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name LIKE 'platform\\_%' \
         AND data_type = 'timestamp without time zone'",
    )
    .await;
    assert!(naive.is_empty(), "naive timestamps: {naive:?}");
    let id_types = strings(
        &db.conn,
        "SELECT table_name || ':' || data_type FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name LIKE 'platform\\_%' AND column_name = 'id'",
    )
    .await;
    assert!(!id_types.is_empty());
    assert!(
        id_types.iter().all(|t| t.ends_with(":uuid")),
        "{id_types:?}"
    );

    // Ownership indexes exist.
    let indexes = strings(
        &db.conn,
        "SELECT indexname::text FROM pg_indexes WHERE schemaname = 'public'",
    )
    .await;
    for required in [
        "idx_platform_service_accounts_application",
        "idx_platform_api_credentials_service_account",
        "idx_platform_api_credentials_application_environment",
        "idx_platform_principals_environment",
        "idx_platform_application_scopes_tenant",
    ] {
        assert!(
            indexes.iter().any(|i| i == required),
            "missing index {required}"
        );
    }
}

#[tokio::test]
async fn no_plaintext_secret_or_contact_columns_exist() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let columns = strings(
        &db.conn,
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name LIKE 'platform\\_%'",
    )
    .await;
    for column in &columns {
        let name = column.split('.').nth(1).unwrap();
        for forbidden in [
            "secret",
            "plaintext",
            "raw",
            "password",
            "token",
            "api_key",
            "phone",
            "e164",
            "msisdn",
            "email",
            "contact",
            "number",
        ] {
            assert!(
                !name.contains(forbidden),
                "column {column} looks like plaintext secret/contact storage"
            );
        }
    }
    let credential_columns = strings(
        &db.conn,
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'platform_api_credentials' ORDER BY column_name",
    )
    .await;
    assert_eq!(
        credential_columns,
        vec![
            "application_id",
            "created_at",
            "environment_id",
            "expires_at",
            "id",
            "label",
            "last_four",
            "not_before",
            "public_id",
            "revoked_at",
            "rotated_from_credential_id",
            "service_account_id",
            "status",
            "superseded_at",
            "superseded_by_credential_id",
            "supersession_valid_until",
            "tenant_id",
            "updated_at",
            "verifier",
            "verifier_algorithm",
            "verifier_key_version",
        ]
    );
}

#[tokio::test]
async fn no_part_c_or_meeting_service_tables_exist() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let all = tables(&db.conn).await;
    for forbidden in [
        "calls",
        "call_participants",
        "call_sessions",
        "call_legs",
        "call_routes",
        "users",
        "organizations",
        "organization_memberships",
        "meetings",
        "api_keys",
        "participants",
    ] {
        assert!(
            !all.iter().any(|t| t == forbidden),
            "unexpected table {forbidden}"
        );
    }
    assert!(all
        .iter()
        .all(|t| t.starts_with("platform_") || t == MIGRATION_TABLE));
}
