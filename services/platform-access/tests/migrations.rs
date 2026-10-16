//! Platform Access migrations: fresh apply, full rollback and re-apply, own history
//! table, invariants enforced by the database.

mod common;

use common::*;
use platform_access_migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, DbBackend, Statement};

async fn tables(conn: &sea_orm::DatabaseConnection) -> Vec<String> {
    conn.query_all(Statement::from_string(
        DbBackend::Postgres,
        "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY 1",
    ))
    .await
    .unwrap()
    .iter()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
    .collect()
}

#[tokio::test]
async fn migrations_apply_roll_back_and_reapply_cleanly() {
    let Some(db) = TestDb::empty().await else { return };
    Migrator::up(&db.conn, None).await.unwrap();
    let up = tables(&db.conn).await;
    for t in [
        "platform_tenants",
        "platform_applications",
        "platform_environments",
        "platform_service_accounts",
        "platform_application_scopes",
        "platform_api_credentials",
        "platform_api_credential_scopes",
        "platform_principals",
        "platform_identity_changes",
        "platform_access_audit",
        "platform_auth_failure_windows",
        "platform_identity_imports",
        "platform_credential_principal_restrictions",
        "platform_access_seaql_migrations",
    ] {
        assert!(up.contains(&t.to_string()), "{t} missing: {up:?}");
    }
    assert!(Migrator::get_pending_migrations(&db.conn).await.unwrap().is_empty());
    Migrator::down(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db.conn).await, vec!["platform_access_seaql_migrations".to_string()]);
    Migrator::up(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db.conn).await, up);
}

#[tokio::test]
async fn the_database_enforces_identity_invariants() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", &["calls.read"]).await;
    let (view, _) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    let cred = view["id"].as_str().unwrap();
    let refused = [
        // Live telecom outside PRODUCTION.
        format!("UPDATE platform_environments SET live_telecom_allowed = TRUE WHERE id = '{}'", c.env("SANDBOX")),
        // Re-parenting.
        format!("UPDATE platform_applications SET tenant_id = gen_random_uuid() WHERE id = '{}'", c.application_id),
        // Verifier rewrite.
        format!("UPDATE platform_api_credentials SET verifier = decode(repeat('00', 32), 'hex') WHERE id = '{cred}'"),
        // Plaintext telephone numbers as principal references.
        format!(
            "INSERT INTO platform_principals (tenant_id, application_id, environment_id, kind, external_reference) \
             VALUES ('{}', '{}', '{}', 'NATIVE_CF_USER', '+2348012345678')",
            c.tenant_id, c.application_id, c.env("SANDBOX")
        ),
        // Cross-scope ownership chain.
        format!(
            "INSERT INTO platform_service_accounts (tenant_id, application_id, environment_id, name) \
             VALUES (gen_random_uuid(), '{}', '{}', 'evil')",
            c.application_id, c.env("SANDBOX")
        ),
        // Audit is append-only.
        "DELETE FROM platform_access_audit".to_string(),
        "UPDATE platform_access_audit SET actor = 'someone-else'".to_string(),
    ];
    for sql in refused {
        assert!(h.conn().execute_unprepared(&sql).await.is_err(), "accepted: {sql}");
    }
    // Revocation is terminal.
    h.conn()
        .execute_unprepared(&format!("UPDATE platform_api_credentials SET status = 'REVOKED', revoked_at = now() WHERE id = '{cred}'"))
        .await
        .unwrap();
    assert!(h
        .conn()
        .execute_unprepared(&format!("UPDATE platform_api_credentials SET status = 'ACTIVE', revoked_at = NULL WHERE id = '{cred}'"))
        .await
        .is_err());
    // Revision is server-maintained.
    async fn rev(h: &Harness, sql: String) -> i64 {
        h.conn()
            .query_one(Statement::from_string(DbBackend::Postgres, sql))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index::<i64>(0)
            .unwrap()
    }
    let before = rev(&h, format!("SELECT revision FROM platform_tenants WHERE id = '{}'", c.tenant_id)).await;
    h.conn()
        .execute_unprepared(&format!("UPDATE platform_tenants SET revision = 999, status = 'SUSPENDED' WHERE id = '{}'", c.tenant_id))
        .await
        .unwrap();
    let after = rev(&h, format!("SELECT revision FROM platform_tenants WHERE id = '{}'", c.tenant_id)).await;
    assert_eq!(after, before + 1);
}
