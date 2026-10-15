//! §94: an in-place upgrade from the previous migration head keeps every row and adds
//! the new objects; reverting the newest migration keeps the data; re-applying it is
//! clean. The previous head is COMPUTED (every registered migration but the newest), so
//! this test follows the history as it grows.
//!
//! Representative rows are written by the real service against the previous schema
//! (protected contact, contexts, sessions, a relay assignment, a revocation whose
//! enforcement is still pending, PENDING outbox events), then the schema is upgraded and
//! the service keeps working on the same data.

mod common;

use std::{collections::BTreeMap, sync::Arc};

use cf_service_auth::ApplicationEnvironmentKind;
use chrono::Duration;
use common::*;
use privacy_relay::{clock::ManualClock, enforcement::DeliveryFailure};
use privacy_relay_migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::json;

const RELAY: &str = "+44 20 7946 0500";
const CONTACT: &str = "+44 20 7946 0123";

async fn tables(conn: &DatabaseConnection) -> Vec<String> {
    conn.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' \
         AND tablename <> 'privacy_relay_seaql_migrations' ORDER BY tablename",
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
    .collect()
}

/// The column list of each table of the previous schema (a column added by the upgrade
/// is not part of the digest).
async fn snapshot(conn: &DatabaseConnection, only: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for table in only {
        let columns: Vec<String> = rows(
            conn,
            &format!(
                "SELECT column_name::text FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = '{table}' ORDER BY column_name"
            ),
        )
        .await
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
        out.insert(table.clone(), columns.join(","));
    }
    out
}

/// Per table: row count and an order-independent digest of the rows over `shape`'s
/// columns.
async fn digest(
    conn: &DatabaseConnection,
    shape: &BTreeMap<String, String>,
) -> BTreeMap<String, (i64, String)> {
    let mut out = BTreeMap::new();
    for (table, columns) in shape {
        let cols: Vec<String> = columns.split(',').map(|c| format!("t.{c}")).collect();
        let row = rows(
            conn,
            &format!(
                "SELECT count(*)::text, COALESCE(md5(string_agg(r, '|' ORDER BY r)), '') FROM \
                 (SELECT ROW({})::text AS r FROM {table} t) x",
                cols.join(", ")
            ),
        )
        .await;
        out.insert(
            table.clone(),
            (row[0][0].parse().unwrap(), row[0][1].clone()),
        );
    }
    out
}

#[tokio::test]
async fn upgrade_from_the_previous_head_keeps_data_adds_objects_and_reverts_cleanly() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let db = Arc::new(db);
    let all = Migrator::migrations().len() as u32;
    assert!(all >= 2);
    let previous_head = all - 1;
    Migrator::up(&db.conn, Some(previous_head)).await.unwrap();
    assert_eq!(
        scalar(
            &db.conn,
            "SELECT count(*) FROM privacy_relay_seaql_migrations"
        )
        .await,
        previous_head as i64
    );
    let before_tables = tables(&db.conn).await;

    // Representative rows, written by the real service on the previous schema.
    let h = Harness::on(db.clone(), ManualClock::new(t0()), ALL_CALLING_PERMISSIONS);
    let c = Consumer::new(ApplicationEnvironmentKind::Production);
    h.add_relay_number(RELAY, "pb-upgrade").await;
    let principal = uuid::Uuid::new_v4();
    let contact = h.create_contact(&c, CONTACT).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &contact, "VERIFIED", None).await;
    let mut req = context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["CALLBACK_RELAY_ALLOWED"]);
    let ctx = h.create_context(&c, req).await;
    let session = h
        .create_session(
            &c,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        h.assign_relay_number(&c, &session).await.status,
        axum::http::StatusCode::CREATED
    );
    let [a, b] = principals(2).try_into().unwrap();
    let revoked = h.bidirectional(&c, &[a, b]).await;
    let revoked_id = revoked["contextId"].as_str().unwrap().to_string();
    let app_session = h.app_session(&c, &revoked_id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    h.calling.fail_with(Some(DeliveryFailure::Unavailable));
    h.lifecycle(&c, &revoked_id, "revoke", 1).await;
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE status = 'PENDING'")
            .await,
        1
    );
    assert!(
        h.count("SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING'")
            .await
            >= 6
    );
    let shape = snapshot(&db.conn, &before_tables).await;
    let before = digest(&db.conn, &shape).await;
    assert!(
        before.values().filter(|(n, _)| *n > 0).count() >= 15,
        "{before:?}"
    );

    // Upgrade to head: nothing pending, every previous row byte-identical (previous
    // columns), the new objects present.
    Migrator::up(&db.conn, None).await.unwrap();
    assert!(Migrator::get_pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(digest(&db.conn, &shape).await, before, "data intact");
    let after_tables = tables(&db.conn).await;
    for new in ["privacy_relay_incidents", "privacy_relay_operator_audit"] {
        assert!(!before_tables.contains(&new.to_string()));
        assert!(after_tables.contains(&new.to_string()), "{new}");
    }
    assert_eq!(
        scalar(&db.conn, "SELECT count(*) FROM information_schema.columns WHERE table_name = 'privacy_relay_enforcement_jobs' AND column_name = 'dead_lettered_at'").await,
        1
    );

    // The service keeps working on the upgraded data: authority as before, the pending
    // job reaches its (new) dead letter, and the incident model is live.
    assert_eq!(h.reason(&c, &app_session, a, &[b]).await, "CONTEXT_REVOKED");
    exec(
        &db.conn,
        &format!(
            "UPDATE privacy_relay_enforcement_jobs SET attempt_count = {}",
            privacy_relay::enforcement::ENFORCEMENT_MAX_ATTEMPTS - 1
        ),
    )
    .await
    .unwrap();
    h.clock.advance(Duration::minutes(10));
    h.relay.run_enforcement_sweep(10).await.unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE status = 'DEAD_LETTER'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_incidents WHERE category = 'ENFORCEMENT_DEAD_LETTER'")
            .await,
        1
    );

    // Down one: the newest objects go, every previous table keeps its rows (the dead
    // letter becomes a due PENDING job again — enforcement is restrictive, retrying is
    // safe).
    let counts_before_down: Vec<(String, i64)> = {
        let d = digest(&db.conn, &shape).await;
        d.into_iter().map(|(t, (n, _))| (t, n)).collect()
    };
    Migrator::down(&db.conn, Some(1)).await.unwrap();
    assert_eq!(
        scalar(
            &db.conn,
            "SELECT count(*) FROM privacy_relay_seaql_migrations"
        )
        .await,
        previous_head as i64
    );
    assert_eq!(tables(&db.conn).await, before_tables);
    let counts_after_down: Vec<(String, i64)> = digest(&db.conn, &shape)
        .await
        .into_iter()
        .map(|(t, (n, _))| (t, n))
        .collect();
    assert_eq!(counts_after_down, counts_before_down);
    assert_eq!(
        scalar(&db.conn, "SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE status = 'PENDING' AND lease_owner IS NULL").await,
        1
    );
    assert_eq!(
        scalar(
            &db.conn,
            "SELECT count(*) FROM pg_proc WHERE proname = 'privacy_relay_guard_incident'"
        )
        .await,
        0
    );

    // Re-up: clean, objects back, data still there.
    Migrator::up(&db.conn, None).await.unwrap();
    assert_eq!(tables(&db.conn).await, after_tables);
    assert_eq!(
        scalar(
            &db.conn,
            "SELECT count(*) FROM privacy_relay_seaql_migrations"
        )
        .await,
        all as i64
    );
    let counts_reup: Vec<(String, i64)> = digest(&db.conn, &shape)
        .await
        .into_iter()
        .map(|(t, (n, _))| (t, n))
        .collect();
    assert_eq!(counts_reup, counts_before_down);
    // And the service delivers the re-pended job once Calling is back.
    h.calling.fail_with(None);
    h.clock.advance(Duration::minutes(10));
    assert_eq!(h.relay.run_enforcement_sweep(10).await.unwrap(), 1);
}
