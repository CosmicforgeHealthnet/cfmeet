//! Upgrade path of the newest Telephony migration (Part I §94): a database at the
//! PREVIOUS head (every migration but the newest, computed from the migrator) holding
//! live data is migrated to head; the data survives, the new schema works on the old
//! rows (an outbox row written before correlations existed is still delivered), the
//! migrator has nothing pending; one step down keeps the data (and the append-only
//! operator audit), and up again restores head.

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use common::{all_on, FakeCalling, TestDb};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use telephony::{clock::ManualClock, service::TelephonyService};
use telephony_migration::{Migrator, MigratorTrait};
use uuid::Uuid;

async fn scalar(conn: &DatabaseConnection, sql: &str) -> i64 {
    conn.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get_by_index::<i64>(0)
    .unwrap()
}

async fn exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_unprepared(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn has_column(conn: &DatabaseConnection, table: &str, column: &str) -> bool {
    scalar(
        conn,
        &format!(
            "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' \
             AND table_name = '{table}' AND column_name = '{column}'"
        ),
    )
    .await
        == 1
}

#[tokio::test]
async fn previous_head_with_data_upgrades_rolls_back_one_step_and_upgrades_again() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let conn = &db.conn;
    let total = Migrator::migrations().len();
    assert!(total >= 2);
    let previous_head = (total - 1) as u32;
    Migrator::up(conn, Some(previous_head)).await.unwrap();
    assert_eq!(
        Migrator::get_pending_migrations(conn).await.unwrap().len(),
        1
    );
    assert!(!has_column(conn, "telephony_outbox", "correlation_id").await);

    // Live data at the previous head: a leg, its command, an undelivered event, an
    // incident and an operator audit row.
    let now = Utc::now();
    let ts = now.to_rfc3339();
    let leg = Uuid::new_v4();
    let event = Uuid::new_v4();
    exec(
        conn,
        &format!(
            "INSERT INTO telephony_legs (id, calling_call_id, calling_call_leg_id, tenant_id, application_id, \
               environment_id, direction, route_class, status, generation, inbound_resolution_id, \
               max_duration_seconds, media_attachment, usage_status, event_sequence, created_at, updated_at) \
             VALUES ('{leg}', gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), \
               gen_random_uuid(), 'INBOUND', 'PSTN_TO_APP', 'RINGING', 1, gen_random_uuid(), 3600, 'NONE', \
               'PENDING', 1, '{ts}', '{ts}')"
        ),
    )
    .await;
    exec(
        conn,
        &format!(
            "INSERT INTO telephony_commands (id, kind, telephony_leg_id, requester_service, idempotency_key, \
               request_digest, status, max_attempts, next_attempt_at, payload, request_id, correlation_id, \
               created_at, updated_at) \
             VALUES (gen_random_uuid(), 'TERMINATE', '{leg}', 'cfmeet.calling', 'term-upgrade-1', \
               repeat('a', 64), 'PENDING', 10, '{ts}', '{{}}', 'req-upgrade-1', 'corr-upgrade-1', '{ts}', '{ts}')"
        ),
    )
    .await;
    exec(
        conn,
        &format!(
            "INSERT INTO telephony_outbox (id, telephony_leg_id, calling_call_id, event_type, leg_sequence, \
               payload, created_at, next_attempt_at) \
             VALUES ('{event}', '{leg}', gen_random_uuid(), 'LEG_RINGING', 1, \
               jsonb_build_object('eventId', '{event}', 'eventType', 'LEG_RINGING'), '{ts}', '{ts}')"
        ),
    )
    .await;
    exec(
        conn,
        &format!(
            "INSERT INTO telephony_incidents (id, dedupe_key, severity, category, status, restricted_evidence, \
               first_seen_at, last_seen_at) \
             VALUES (gen_random_uuid(), 'circuit:upgrade', 'HIGH', 'PROVIDER_CIRCUIT_OPENED', 'OPEN', '{{}}', '{ts}', '{ts}')"
        ),
    )
    .await;
    exec(
        conn,
        &format!(
            "INSERT INTO telephony_operator_audit (id, actor, correlation_id, action, target_kind, target_id, detail, occurred_at) \
             VALUES (gen_random_uuid(), 'ops.upgrade', 'op-upgrade-1', 'ROUTE_DISABLE', 'ROUTE', 'ng-a', '{{}}', '{ts}')"
        ),
    )
    .await;

    // --- Up to head ---------------------------------------------------------------------
    Migrator::up(conn, None).await.unwrap();
    assert!(Migrator::get_pending_migrations(conn)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        Migrator::get_applied_migrations(conn).await.unwrap().len(),
        total
    );
    assert!(has_column(conn, "telephony_outbox", "correlation_id").await);
    assert!(has_column(conn, "telephony_outbox", "dead_lettered_at").await);
    // The old rows are intact; the event has no stored correlation and is not dead.
    assert_eq!(
        scalar(
            conn,
            &format!(
                "SELECT count(*) FROM telephony_outbox WHERE id = '{event}' AND correlation_id IS NULL \
                 AND dead_lettered_at IS NULL AND delivered_at IS NULL"
            )
        )
        .await,
        1
    );
    assert_eq!(
        scalar(conn, "SELECT count(*) FROM telephony_commands").await,
        1
    );
    assert_eq!(
        scalar(conn, "SELECT count(*) FROM telephony_incidents").await,
        1
    );
    // The new vocabulary is accepted; the guards hold.
    exec(
        conn,
        &format!(
            "INSERT INTO telephony_incidents (id, dedupe_key, severity, category, status, restricted_evidence, \
               first_seen_at, last_seen_at) \
             VALUES (gen_random_uuid(), 'outbox-dead-letter:CALLING_REFUSED', 'HIGH', 'OUTBOX_DEAD_LETTER', 'OPEN', \
               '{{}}', '{ts}', '{ts}')"
        ),
    )
    .await;
    exec(
        conn,
        &format!(
            "INSERT INTO telephony_operator_audit (id, actor, correlation_id, action, target_kind, target_id, detail, occurred_at) \
             VALUES (gen_random_uuid(), 'ops.upgrade', 'op-upgrade-2', 'INCIDENT_RESOLVE', 'INCIDENT', 'x1', '{{}}', '{ts}')"
        ),
    )
    .await;
    assert!(conn
        .execute_unprepared(&format!(
            "UPDATE telephony_outbox SET delivered_at = now(), dead_lettered_at = now() WHERE id = '{event}'"
        ))
        .await
        .is_err());
    // The service at head delivers the pre-upgrade event (a fresh correlation is minted
    // for a row without one).
    let calling = Arc::new(FakeCalling::new());
    let svc = TelephonyService::new(
        conn.clone(),
        Arc::new(ManualClock::new(now + Duration::seconds(1))),
        all_on(),
    )
    .with_calling(calling.clone());
    assert_eq!(svc.deliver_outbox_once(10).await.unwrap(), (1, 0));
    assert_eq!(
        calling.correlation_of(&serde_json::json!(event.to_string())),
        Some(None)
    );
    assert!(svc
        .metrics_text()
        .await
        .unwrap()
        .contains("telephony_outbox_dead_lettered 0"));

    // --- Down one step --------------------------------------------------------------------
    Migrator::down(conn, Some(1)).await.unwrap();
    assert_eq!(
        Migrator::get_pending_migrations(conn).await.unwrap().len(),
        1
    );
    assert!(!has_column(conn, "telephony_outbox", "correlation_id").await);
    assert!(!has_column(conn, "telephony_outbox", "dead_lettered_at").await);
    assert_eq!(
        scalar(
            conn,
            &format!("SELECT count(*) FROM telephony_outbox WHERE id = '{event}' AND delivered_at IS NOT NULL")
        )
        .await,
        1
    );
    // Incidents of the newest categories go; the others and the audit stay.
    assert_eq!(
        scalar(
            conn,
            "SELECT count(*) FROM telephony_incidents WHERE category = 'PROVIDER_CIRCUIT_OPENED'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            conn,
            "SELECT count(*) FROM telephony_incidents WHERE category = 'OUTBOX_DEAD_LETTER'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(conn, "SELECT count(*) FROM telephony_operator_audit").await,
        2,
        "the operator audit is append-only, also across a rollback"
    );
    // The previous vocabulary is enforced again for new rows.
    assert!(conn
        .execute_unprepared(&format!(
            "INSERT INTO telephony_operator_audit (id, actor, correlation_id, action, target_kind, target_id, detail, occurred_at) \
             VALUES (gen_random_uuid(), 'ops.upgrade', 'op-upgrade-3', 'INCIDENT_RESOLVE', 'INCIDENT', 'x2', '{{}}', '{ts}')"
        ))
        .await
        .is_err());

    // --- Up again -------------------------------------------------------------------------
    Migrator::up(conn, None).await.unwrap();
    assert!(Migrator::get_pending_migrations(conn)
        .await
        .unwrap()
        .is_empty());
    assert!(has_column(conn, "telephony_outbox", "correlation_id").await);
    assert_eq!(
        scalar(
            conn,
            &format!(
                "SELECT count(*) FROM telephony_outbox WHERE id = '{event}' AND correlation_id IS NULL \
                 AND delivered_at IS NOT NULL"
            )
        )
        .await,
        1
    );
    assert_eq!(scalar(conn, "SELECT count(*) FROM telephony_legs").await, 1);
}
