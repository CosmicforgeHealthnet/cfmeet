//! Developer Platform worker foundation: its own loop records per-instance heartbeats,
//! multiple instances coexist, and the loop stops on shutdown.

mod common;

use chrono::{Duration as ChronoDuration, Utc};
use common::*;
use developer_platform::worker;

#[tokio::test]
async fn worker_heartbeats_are_per_instance_and_monotonic() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let t = Utc::now();
    worker::heartbeat_once(&db.conn, "worker-a", "test", t)
        .await
        .unwrap();
    worker::heartbeat_once(&db.conn, "worker-a", "test", t + ChronoDuration::seconds(5))
        .await
        .unwrap();
    // A late (out-of-order) beat never moves last_seen backwards.
    worker::heartbeat_once(
        &db.conn,
        "worker-a",
        "test",
        t - ChronoDuration::seconds(30),
    )
    .await
    .unwrap();
    worker::heartbeat_once(&db.conn, "worker-b", "test", t)
        .await
        .unwrap();
    use sea_orm::{ConnectionTrait, Statement};
    let row = db
        .conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT beats, extract(epoch FROM last_seen_at - started_at)::bigint FROM devplat_worker_heartbeats WHERE instance_id = 'worker-a'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get_by_index::<i64>(0).unwrap(), 3);
    assert_eq!(row.try_get_by_index::<i64>(1).unwrap(), 5);
    // Malformed instance ids and runtime environments are refused by the schema.
    assert!(worker::heartbeat_once(&db.conn, "Bad Id", "test", t)
        .await
        .is_err());
    assert!(worker::heartbeat_once(&db.conn, "worker-c", "prod", t)
        .await
        .is_err());
}

#[tokio::test]
async fn the_worker_loop_runs_until_shutdown() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(worker::run(
        db.conn.clone(),
        "worker-loop".into(),
        "test".into(),
        std::time::Duration::from_millis(100),
        async {
            let _ = rx.await;
        },
    ));
    tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("the loop stops on shutdown")
        .unwrap();
    use sea_orm::{ConnectionTrait, Statement};
    let beats = db
        .conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT beats FROM devplat_worker_heartbeats WHERE instance_id = 'worker-loop'",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap();
    assert!(beats >= 2, "{beats}");
}
