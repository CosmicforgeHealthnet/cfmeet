//! Upgrading a database that already holds data (fresh-database tests cannot see this).
//! Production 2026-11-18: the S9 history's backfill updated existing `communication_privacy`
//! rows without advancing their version and the version guard aborted the migration.
use communications_migration::{
    DeviceSessionMigrator, GroupInvitationPolicyMigrator, Migrator, MigratorTrait,
    NotificationPublicationsMigrator, PreferenceAuthorityMigrator, TimelineRealtimeMigrator,
    UnifiedMigrator,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use uuid::Uuid;

#[tokio::test]
async fn the_group_invitation_history_upgrades_existing_privacy_rows_without_touching_versions() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("PostgreSQL acceptance tests require TEST_DATABASE_URL; there is no implicit skip");
    let admin = Database::connect(&url)
        .await
        .expect("test database connection");
    let name = format!("hub_u_{}", Uuid::new_v4().simple());
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("CREATE DATABASE {name}"),
        ))
        .await
        .unwrap();
    let db = Database::connect(format!("{}/{}", url.rsplit_once('/').unwrap().0, name))
        .await
        .unwrap();
    // Every history the previous production image had.
    Migrator::up(&db, None).await.unwrap();
    UnifiedMigrator::up(&db, None).await.unwrap();
    DeviceSessionMigrator::up(&db, None).await.unwrap();
    TimelineRealtimeMigrator::up(&db, None).await.unwrap();
    NotificationPublicationsMigrator::up(&db, None)
        .await
        .unwrap();
    PreferenceAuthorityMigrator::up(&db, None).await.unwrap();
    // Existing data: two profiles, one refusing contact requests, privacy rows at version 3.
    for (n, permission) in [(1, "EVERYONE"), (2, "NOBODY")] {
        let profile = Uuid::new_v4();
        db.execute_unprepared(&format!(
                "INSERT INTO communication_profiles (id, tenant_id, application_id, environment_id, \
                 account_reference, principal_id, display_name) VALUES ('{profile}', '{t}', '{t}', '{t}', \
                 'cfmeet-user:{n}', '{p}', 'Person {n}');
                 INSERT INTO communication_privacy (profile_id, version, contact_request_permissions) \
                 VALUES ('{profile}', 3, '{permission}');",
                t = Uuid::nil(),
                p = Uuid::new_v4(),
        ))
        .await
        .unwrap();
    }
    GroupInvitationPolicyMigrator::up(&db, None).await.unwrap();
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT contact_request_permissions, group_invitation_policy, version \
             FROM communication_privacy ORDER BY contact_request_permissions"
                .to_owned(),
        ))
        .await
        .unwrap();
    let got: Vec<(String, String, i64)> = rows
        .iter()
        .map(|r| {
            (
                r.try_get("", "contact_request_permissions").unwrap(),
                r.try_get("", "group_invitation_policy").unwrap(),
                r.try_get("", "version").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("EVERYONE".into(), "CONNECTIONS".into(), 3),
            ("NOBODY".into(), "NOBODY".into(), 3),
        ]
    );
    // The version guard is back on after the migration.
    let refused = db
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "UPDATE communication_privacy SET group_invitation_policy = 'NOBODY'".to_owned(),
        ))
        .await;
    assert!(refused.is_err(), "the version guard must be enabled again");
    assert!(communications_migration::ready(&db).await.unwrap());
}
