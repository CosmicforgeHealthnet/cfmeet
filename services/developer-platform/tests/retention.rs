//! P9: the bounded retention purge of webhook, API usage and activity history. Only the
//! retention worker may delete (ordinary deletes stay refused by the guards), each class
//! keeps what is not due (open deliveries, events with deliveries, replays that stay,
//! recent rows), batches are bounded and audited, and a class bound of 0 keeps its rows.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use developer_platform::retention::{self, Class, RetentionSettings};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;

async fn exec(db: &DatabaseConnection, sql: &str) {
    db.execute(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

struct Scope {
    tenant: Uuid,
    app: Uuid,
    env: Uuid,
    subscription: Uuid,
}

async fn scope(db: &DatabaseConnection) -> Scope {
    let s = Scope {
        tenant: Uuid::new_v4(),
        app: Uuid::new_v4(),
        env: Uuid::new_v4(),
        subscription: Uuid::new_v4(),
    };
    exec(
        db,
        &format!(
            "INSERT INTO devplat_webhook_subscriptions (id, tenant_id, application_id, environment_id, \
               environment_kind, url, event_types, status, secret_sealed, secret_key_id, created_by, \
               created_at, updated_at) VALUES ('{}', '{}', '{}', '{}', 'SANDBOX', 'https://hooks.example/x', \
               ARRAY['call.ended'], 'ACTIVE', 'v2.k1.{}', 'k1', 'test', now(), now())",
            s.subscription,
            s.tenant,
            s.app,
            s.env,
            "a".repeat(48)
        ),
    )
    .await;
    s
}

async fn event(db: &DatabaseConnection, s: &Scope, age_days: i64) -> String {
    let id = format!("whe_{}", Uuid::new_v4().simple());
    exec(
        db,
        &format!(
            "INSERT INTO devplat_webhook_events (event_id, producer, source_event_id, event_type, tenant_id, \
               application_id, environment_id, occurred_at, payload, received_at) VALUES ('{id}', 'cfmeet.calling', \
               '{src}', 'call.ended', '{}', '{}', '{}', now() - interval '{age_days} days', \
               '{{\"id\": \"{id}\", \"type\": \"call.ended\", \"data\": {{}}}}', now() - interval '{age_days} days')",
            s.tenant,
            s.app,
            s.env,
            src = Uuid::new_v4().simple()
        ),
    )
    .await;
    id
}

/// A delivery of `event` (`completed_days`: None = still pending).
async fn delivery(
    db: &DatabaseConnection,
    s: &Scope,
    event: &str,
    completed_days: Option<i64>,
    replay_of: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    let (status, completed) = match completed_days {
        Some(d) => ("SUCCEEDED", format!("now() - interval '{d} days'")),
        None => ("PENDING", "NULL".to_string()),
    };
    let replay = replay_of.map_or("NULL".to_string(), |r| format!("'{r}'"));
    exec(
        db,
        &format!(
            "INSERT INTO devplat_webhook_deliveries (id, subscription_id, event_id, tenant_id, application_id, \
               environment_id, replay_of, status, attempt_count, next_attempt_at, created_at, updated_at, completed_at) \
             VALUES ('{id}', '{}', '{event}', '{}', '{}', '{}', {replay}, '{status}', 1, now(), now(), now(), {completed})",
            s.subscription, s.tenant, s.app, s.env
        ),
    )
    .await;
    if completed_days.is_some() {
        exec(
            db,
            &format!(
                "INSERT INTO devplat_webhook_attempts (delivery_id, attempt_number, started_at, finished_at, \
                   http_status, outcome) VALUES ('{id}', 1, now(), now(), 200, 'SUCCEEDED')"
            ),
        )
        .await;
    }
    id
}

#[tokio::test]
async fn only_the_retention_worker_deletes_and_only_what_is_due() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let db = &db.conn;
    let s = scope(db).await;
    // Webhooks: an old delivered event, a recent one, an old one still pending, an old
    // original whose replay is recent (both kept), an old original with an old replay
    // (both purged).
    let old_event = event(db, &s, 40).await;
    let old_delivery = delivery(db, &s, &old_event, Some(40), None).await;
    let recent_event = event(db, &s, 2).await;
    delivery(db, &s, &recent_event, Some(2), None).await;
    let pending_event = event(db, &s, 40).await;
    delivery(db, &s, &pending_event, None, None).await;
    let kept_original_event = event(db, &s, 40).await;
    let kept_original = delivery(db, &s, &kept_original_event, Some(40), None).await;
    delivery(db, &s, &kept_original_event, Some(1), Some(kept_original)).await;
    let pair_event = event(db, &s, 40).await;
    let pair = delivery(db, &s, &pair_event, Some(40), None).await;
    delivery(db, &s, &pair_event, Some(39), Some(pair)).await;
    let orphan_event = event(db, &s, 40).await; // old, never delivered
                                                // Usage, rejections, activity.
    exec(db, "INSERT INTO devplat_api_usage_batches VALUES (gen_random_uuid(), 'cfmeet.api-gateway', 1, now() - interval '40 days'), (gen_random_uuid(), 'cfmeet.calling', 1, now())").await;
    exec(db, &format!("INSERT INTO devplat_api_usage_daily (environment_id, usage_day, request_count, updated_at) VALUES ('{}', current_date - 500, 3, now()), ('{}', current_date, 4, now())", s.env, s.env)).await;
    exec(db, &format!("INSERT INTO devplat_api_operation_usage (environment_id, usage_day, domain, operation_id, status_class, request_count, updated_at) VALUES ('{}', current_date - 500, 'calling', 'listCalls', '2xx', 3, now()), ('{}', current_date, 'calling', 'listCalls', '2xx', 4, now())", s.env, s.env)).await;
    exec(db, "INSERT INTO devplat_usage_rejections VALUES (gen_random_uuid(), 'cfmeet.calling', 'f1', 'UNKNOWN_ENVIRONMENT', now() - interval '100 days'), (gen_random_uuid(), 'cfmeet.calling', 'f2', 'UNKNOWN_ENVIRONMENT', now())").await;
    exec(db, "INSERT INTO devplat_event_rejections VALUES (gen_random_uuid(), 'cfmeet.calling', 'e1', 'UNKNOWN_TYPE', now() - interval '100 days')").await;
    for age in [400, 1] {
        exec(db, &format!("INSERT INTO devplat_activity_events (id, tenant_id, actor_kind, actor_ref, action, target_type, target_id, outcome, request_id, correlation_id, occurred_at) VALUES (gen_random_uuid(), '{}', 'USER', 'u1', 'project.created', 'PROJECT', 'p1', 'SUCCEEDED', 'r', 'c', now() - interval '{age} days')", s.tenant)).await;
    }

    // Ordinary deletes stay refused everywhere.
    for table in [
        "devplat_webhook_attempts",
        "devplat_webhook_deliveries",
        "devplat_webhook_events",
        "devplat_api_usage_batches",
        "devplat_api_usage_daily",
        "devplat_api_operation_usage",
        "devplat_usage_rejections",
        "devplat_event_rejections",
        "devplat_activity_events",
    ] {
        assert!(
            db.execute(Statement::from_string(
                DbBackend::Postgres,
                format!("DELETE FROM {table}")
            ))
            .await
            .is_err(),
            "{table} accepted an ordinary delete"
        );
    }
    // A class bound of 0 keeps its rows; the switch off purges nothing.
    let keep_activity = RetentionSettings::default().with_days(Class::ActivityEvents, None);
    let off = retention::purge_once(db, &RetentionSettings::disabled(), Utc::now(), 1000).await;
    assert_eq!(off, Default::default());

    let r = retention::purge_once(db, &keep_activity, Utc::now(), 1000).await;
    assert!(r.failed.is_empty(), "{r:?}");
    // the old delivery and the old pair (their attempts go with them)
    assert_eq!(r.deleted(Class::WebhookDeliveryHistory), 3, "{r:?}");
    // the old delivered event, the pair's event, the never-delivered old event
    assert_eq!(r.deleted(Class::WebhookEventHistory), 3, "{r:?}");
    assert_eq!(r.deleted(Class::ApiUsageBatches), 1);
    assert_eq!(r.deleted(Class::ApiUsageCounters), 2);
    assert_eq!(r.deleted(Class::IngestionRejections), 2);
    assert_eq!(r.deleted(Class::ActivityEvents), 0);
    let gone =
        |id: Uuid| format!("SELECT count(*) FROM devplat_webhook_deliveries WHERE id = '{id}'");
    assert_eq!(count(db, &gone(old_delivery)).await, 0);
    assert_eq!(count(db, &gone(pair)).await, 0);
    assert_eq!(count(db, &gone(kept_original)).await, 1);
    assert_eq!(
        count(db, "SELECT count(*) FROM devplat_webhook_deliveries").await,
        4
    );
    for (e, present) in [
        (&old_event, 0),
        (&orphan_event, 0),
        (&recent_event, 1),
        (&pending_event, 1),
        (&kept_original_event, 1),
    ] {
        assert_eq!(
            count(
                db,
                &format!("SELECT count(*) FROM devplat_webhook_events WHERE event_id = '{e}'")
            )
            .await,
            present,
            "{e}"
        );
    }
    assert_eq!(
        count(db, "SELECT count(*) FROM devplat_api_usage_daily").await,
        1
    );
    assert_eq!(
        count(db, "SELECT count(*) FROM devplat_api_operation_usage").await,
        1
    );
    assert_eq!(
        count(db, "SELECT count(*) FROM devplat_activity_events").await,
        2
    );
    // Audited per class (durable counters) and idempotent.
    let summary = retention::retention_summary(db, Utc::now()).await.unwrap();
    let classes = summary["classes"].as_array().unwrap();
    assert!(classes
        .iter()
        .any(|c| c["dataClass"] == "WEBHOOK_DELIVERY_HISTORY" && c["deletedRows"] == 3));
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM devplat_retention_audit WHERE outcome = 'DELETED'"
        )
        .await,
        5
    );
    let again = retention::purge_once(db, &keep_activity, Utc::now(), 1000).await;
    assert!(again.deleted.is_empty(), "{again:?}");
    // The activity class with its default bound (365 days).
    let r = retention::purge_once(db, &RetentionSettings::default(), Utc::now(), 1000).await;
    assert_eq!(r.deleted(Class::ActivityEvents), 1);
    // The audit itself is append-only.
    assert!(db
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "DELETE FROM devplat_retention_audit".to_string()
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn batches_are_bounded_and_the_job_runs_once_per_interval() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let db = &db.conn;
    exec(db, "INSERT INTO devplat_api_usage_batches SELECT gen_random_uuid(), 'cfmeet.api-gateway', 1, now() - interval '60 days' FROM generate_series(1, 25)").await;
    let only_batches = Class::ALL
        .iter()
        .fold(RetentionSettings::default(), |s, c| {
            if *c == Class::ApiUsageBatches {
                s
            } else {
                s.with_days(*c, None)
            }
        });
    let r = retention::purge_once(db, &only_batches, Utc::now(), 10).await;
    assert_eq!(r.deleted(Class::ApiUsageBatches), 10);
    assert_eq!(
        count(db, "SELECT count(*) FROM devplat_api_usage_batches").await,
        15
    );
    let now = Utc::now();
    assert!(retention::retention_pass(db, &only_batches, now)
        .await
        .is_some());
    assert!(
        retention::retention_pass(db, &only_batches, now + Duration::minutes(5))
            .await
            .is_none()
    );
    assert_eq!(
        count(db, "SELECT count(*) FROM devplat_api_usage_batches").await,
        0
    );
    assert!(retention::retention_pass(
        db,
        &RetentionSettings::disabled(),
        now + Duration::hours(2)
    )
    .await
    .is_none());
}

#[tokio::test]
async fn the_gap_closure_migration_rolls_back_to_the_original_guards() {
    use developer_platform_migration::{GapClosureMigrator, MigratorTrait};
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let conn = &db.conn;
    developer_platform_migration::up_all(conn).await.unwrap();
    assert!(developer_platform_migration::pending_migrations(conn)
        .await
        .unwrap()
        .is_empty());
    // Rolls back the later gap-closure migrations too (ADR 0062 platform build, ADR 0064
    // apps, ADR 0068 admin operations).
    GapClosureMigrator::down(conn, Some(4)).await.unwrap();
    assert_eq!(
        developer_platform_migration::pending_migrations(conn)
            .await
            .unwrap(),
        [
            "m20261206_000001_retention_purge",
            "m20261207_000002_platform_build",
            "m20261208_000003_apps",
            "m20261209_000004_admin_operations"
        ]
    );
    // Back to the original guards: even the retention flag cannot delete.
    exec(conn, "INSERT INTO devplat_api_usage_batches VALUES (gen_random_uuid(), 'cfmeet.api-gateway', 1, now())").await;
    {
        use sea_orm::TransactionTrait;
        let txn = conn.begin().await.unwrap();
        txn.execute_unprepared("SET LOCAL devplat.retention_worker = 'on'")
            .await
            .unwrap();
        assert!(txn
            .execute_unprepared("DELETE FROM devplat_api_usage_batches")
            .await
            .is_err());
        txn.rollback().await.unwrap();
    }
    GapClosureMigrator::up(conn, None).await.unwrap();
    assert_eq!(
        count(conn, "SELECT count(*) FROM devplat_retention_audit").await,
        0
    );
}
