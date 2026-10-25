//! `m20261101_000001_platform_environment_meetings` up / down (Part VI §51).
//!
//! A meeting space has exactly one owner kind (a CF Meet account XOR a complete Platform
//! Access environment scope); the public-event outbox channel exists only after the
//! migration; down refuses while environment-owned meetings exist and restores the
//! previous schema otherwise.

mod common;

use common::TestDb;
use migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;

async fn exec(c: &DatabaseConnection, sql: &str) -> Result<(), sea_orm::DbErr> {
    c.execute(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .map(|_| ())
}

async fn count(c: &DatabaseConnection, sql: &str) -> i64 {
    c.query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

async fn user(c: &DatabaseConnection) -> Uuid {
    let id = Uuid::new_v4();
    exec(
        c,
        &format!(
            "INSERT INTO users (id, username, email, auth_type, role, status, created_at, updated_at) \
             VALUES ('{id}', 'u{}', 'u{}@example.com', 'local', 'user', 'active', now(), now())",
            id.simple(),
            id.simple()
        ),
    )
    .await
    .unwrap();
    id
}

fn code() -> String {
    format!("cf{}", &Uuid::new_v4().simple().to_string()[..10])
}

#[tokio::test]
async fn platform_scope_columns_migrate_up_and_down() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let c = &db.conn;
    Migrator::up(c, None).await.unwrap();
    let owner = user(c).await;
    let (t, a, e, sa) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    // A user-owned space is unchanged.
    exec(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title) VALUES ('{}', '{owner}', '{}', 'u')", Uuid::new_v4(), code())).await.unwrap();
    // An environment-owned space needs the complete scope and no human owner.
    let platform_space = Uuid::new_v4();
    exec(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, platform_tenant_id, platform_application_id, platform_environment_id, created_by_service_account_id) VALUES ('{platform_space}', NULL, '{}', 'p', '{t}', '{a}', '{e}', '{sa}')", code())).await.unwrap();
    for bad in [
        // Both owner kinds.
        format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, platform_tenant_id, platform_application_id, platform_environment_id, created_by_service_account_id) VALUES ('{}', '{owner}', '{}', 'x', '{t}', '{a}', '{e}', '{sa}')", Uuid::new_v4(), code()),
        // No owner at all.
        format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title) VALUES ('{}', NULL, '{}', 'x')", Uuid::new_v4(), code()),
        // Partial scope.
        format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, platform_environment_id) VALUES ('{}', NULL, '{}', 'x', '{e}')", Uuid::new_v4(), code()),
        format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, platform_tenant_id, platform_application_id, platform_environment_id) VALUES ('{}', NULL, '{}', 'x', '{t}', '{a}', '{e}')", Uuid::new_v4(), code()),
    ] {
        assert!(exec(c, &bad).await.is_err(), "accepted: {bad}");
    }
    // The public-event channel exists; unknown channels are still refused.
    let outbox = |channel: &str| {
        format!(
            "INSERT INTO outbox_events (id, aggregate_type, aggregate_id, event_type, channel) \
             VALUES ('{}', 'x', '{}', 'x.y', '{channel}')",
            Uuid::new_v4(),
            Uuid::new_v4()
        )
    };
    exec(c, &outbox("PUBLIC_EVENT")).await.unwrap();
    assert!(exec(c, &outbox("WEBHOOK")).await.is_err());
    // Invitations may lack a CF Meet inviter (machine callers).
    let inv = Uuid::new_v4();
    exec(c, &format!("INSERT INTO meeting_invitations (id, meeting_space_id, inviter_user_id, requested_role, token_hash, expires_at) VALUES ('{inv}', '{platform_space}', NULL, 'PARTICIPANT', '{}', now() + interval '1 hour')", Uuid::new_v4().simple())).await.unwrap();

    // Down refuses while environment-owned data exists — nothing is silently dropped.
    assert!(Migrator::down(c, Some(1)).await.is_err());
    assert_eq!(
        count(
            c,
            "SELECT count(*) FROM meeting_spaces WHERE platform_environment_id IS NOT NULL"
        )
        .await,
        1
    );
    exec(
        c,
        &format!("DELETE FROM meeting_invitations WHERE id = '{inv}'"),
    )
    .await
    .unwrap();
    exec(
        c,
        &format!("DELETE FROM meeting_spaces WHERE id = '{platform_space}'"),
    )
    .await
    .unwrap();
    Migrator::down(c, Some(1)).await.unwrap();
    assert_eq!(
        count(c, "SELECT count(*) FROM information_schema.columns WHERE table_name = 'meeting_spaces' AND column_name LIKE 'platform_%'").await,
        0
    );
    assert_eq!(
        count(
            c,
            "SELECT count(*) FROM outbox_events WHERE channel = 'PUBLIC_EVENT'"
        )
        .await,
        0,
        "delivery-only rows removed"
    );
    assert!(exec(c, &outbox("PUBLIC_EVENT")).await.is_err());
    assert!(exec(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title) VALUES ('{}', NULL, '{}', 'x')", Uuid::new_v4(), code())).await.is_err(), "owner NOT NULL again");
    // And up again.
    Migrator::up(c, None).await.unwrap();
    exec(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, platform_tenant_id, platform_application_id, platform_environment_id, created_by_service_account_id) VALUES ('{}', NULL, '{}', 'p', '{t}', '{a}', '{e}', '{sa}')", Uuid::new_v4(), code())).await.unwrap();
}
