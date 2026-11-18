//! Notification Service roadmap N7a — routing fence protocol v1 in the Meeting database
//! (blueprint Document 4 §9; acceptance matrix Document 7 rows M-10, M-11, M-15, M-17):
//! sticky decisions, legacy-table guard triggers (inert in LEGACY_ONLY), the decision-row
//! guard (F3), canary determinism, unilateral reroute of unacknowledged decisions, the
//! CRITICAL lease rules, and `routingProtocol` in the health/readiness JSON.

mod common;

use axum::http::StatusCode;
use backend::services::{
    email_delivery::{self, category, NewEmail},
    notification_fence::{
        self as fence, canary, CriticalReroute, DecisionReason, LeaseGrant, MeetingCategory, Route,
        UnackedReroute,
    },
};
use chrono::{Duration, Utc};
use common::*;
use migration::MigratorTrait;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement, TransactionTrait};
use uuid::Uuid;

macro_rules! db {
    () => {
        match TestDb::new().await {
            Some(d) => d,
            None => return,
        }
    };
}

fn later() -> chrono::DateTime<Utc> {
    Utc::now() + Duration::days(1)
}

async fn exec(conn: &impl ConnectionTrait, sql: &str) -> Result<u64, DbErr> {
    Ok(conn
        .execute(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await?
        .rows_affected())
}

async fn scalar_i64(conn: &impl ConnectionTrait, sql: &str) -> i64 {
    conn.query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

async fn text(conn: &impl ConnectionTrait, sql: &str) -> String {
    conn.query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<String>(0)
        .unwrap()
}

async fn set_mode(
    conn: &DatabaseConnection,
    category: MeetingCategory,
    mode: &str,
    canary_percent: i16,
    fallback: &str,
    version: i32,
) {
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE notification_routing_state
            SET mode = $2, canary_percent = $3, critical_fallback = $4, routing_version = $5,
                changed_by_run = 'test', changed_at = now()
          WHERE category = $1",
        [
            category.as_str().into(),
            mode.into(),
            canary_percent.into(),
            fallback.into(),
            version.into(),
        ],
    ))
    .await
    .unwrap();
}

async fn insert_email(conn: &impl ConnectionTrait, cat: &str, key: &str) -> Result<u64, DbErr> {
    Ok(conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO email_deliveries (id, category, idempotency_key, recipient_hash, subject)
             VALUES ($1, $2, $3, repeat('a', 64), 'Subject')
             ON CONFLICT (idempotency_key) DO NOTHING",
            [Uuid::new_v4().into(), cat.into(), key.into()],
        ))
        .await?
        .rows_affected())
}

async fn insert_job(
    conn: &impl ConnectionTrait,
    id: Uuid,
    kind: &str,
    dedupe: &str,
) -> Result<u64, DbErr> {
    Ok(conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO meeting_notifications (id, kind, meeting_space_id, conference_id, dedupe_key)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (dedupe_key) DO NOTHING",
            [
                id.into(),
                kind.into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                dedupe.into(),
            ],
        ))
        .await?
        .rows_affected())
}

async fn count(conn: &impl ConnectionTrait, table: &str, column: &str, key: &str) -> i64 {
    conn.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!("SELECT count(*) FROM {table} WHERE {column} = $1"),
        [key.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get_by_index::<i64>(0)
    .unwrap()
}

fn refused(r: Result<u64, DbErr>) {
    let err = r.expect_err("the guard trigger must refuse");
    assert!(
        fence::is_routing_decision_required(&err),
        "expected ROUTING_DECISION_REQUIRED (CFN01), got {err:?}"
    );
}

fn immutable(r: Result<u64, DbErr>) {
    let err = r.expect_err("the decision guard must refuse");
    assert!(
        fence::is_routing_decision_immutable(&err),
        "expected ROUTING_DECISION_IMMUTABLE (CFN02), got {err:?}"
    );
}

fn verify_email(key: &str) -> NewEmail {
    NewEmail {
        category: category::AUTH_VERIFICATION,
        idempotency_key: Some(key.into()),
        to: "ada@example.com".into(),
        to_name: Some("Ada".into()),
        subject: "Your code".into(),
        html: "<p>code</p>".into(),
        text: "code".into(),
        expires_at: Some(later()),
    }
}

#[tokio::test]
async fn migration_seeds_every_meeting_category_legacy_only() {
    let db = db!();
    let rows = db
        .conn
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT category, mode, routing_version, changed_by_run, canary_percent, critical_fallback, protocol
               FROM notification_routing_state ORDER BY category"
                .to_string(),
        ))
        .await
        .unwrap();
    let mut seeded: Vec<String> = Vec::new();
    for r in &rows {
        assert_eq!(r.try_get::<String>("", "mode").unwrap(), "LEGACY_ONLY");
        assert_eq!(r.try_get::<i32>("", "routing_version").unwrap(), 1);
        assert_eq!(
            r.try_get::<String>("", "changed_by_run").unwrap(),
            "migration"
        );
        assert_eq!(r.try_get::<i16>("", "canary_percent").unwrap(), 0);
        assert_eq!(
            r.try_get::<String>("", "critical_fallback").unwrap(),
            "AUTO"
        );
        assert_eq!(r.try_get::<i16>("", "protocol").unwrap(), 1);
        seeded.push(r.try_get("", "category").unwrap());
    }
    let mut expected: Vec<String> = MeetingCategory::ALL
        .iter()
        .map(|c| c.as_str().to_string())
        .collect();
    expected.sort();
    assert_eq!(seeded, expected);
    // CHECK constraints from §9.2.
    assert!(exec(
        &db.conn,
        "UPDATE notification_routing_state SET mode = 'ACTIVE' WHERE category = 'AUTH_VERIFICATION'"
    )
    .await
    .is_err());
    assert!(exec(
        &db.conn,
        "UPDATE notification_routing_state SET canary_percent = 101 WHERE category = 'AUTH_VERIFICATION'"
    )
    .await
    .is_err());
}

/// M-10: the same key routed twice (retry, duplicate request), across a mode change, yields
/// one decision and one row in one system.
#[tokio::test]
async fn m10_sticky_decision_across_mode_change() {
    let db = db!();
    let cat = MeetingCategory::AuthVerification;
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "AUTO", 2).await;

    let first = fence::route_message(&db.conn, "otp:verify:h:d1", cat, later())
        .await
        .unwrap();
    assert_eq!(
        (first.route, first.sticky, first.routing_version),
        (Route::New, false, 2)
    );

    // Routing rollback: new decisions OLD, the existing one stays NEW (sticky).
    set_mode(&db.conn, cat, "FENCED_OLD", 0, "AUTO", 3).await;
    let again = fence::route_message(&db.conn, "otp:verify:h:d1", cat, later())
        .await
        .unwrap();
    assert_eq!(again.decision_id, first.decision_id);
    assert_eq!(
        (again.route, again.sticky, again.routing_version),
        (Route::New, true, 2)
    );
    // The guard refuses an old-path copy of a NEW message.
    refused(insert_email(&db.conn, "AUTH_VERIFICATION", "otp:verify:h:d1").await);
    assert_eq!(
        count(
            &db.conn,
            "email_deliveries",
            "idempotency_key",
            "otp:verify:h:d1"
        )
        .await,
        0
    );

    // A business retry of an OLD message in one transaction each: one decision, one row.
    for attempt in 0..2 {
        let txn = db.conn.begin().await.unwrap();
        let routed = fence::route_message(&txn, "otp:verify:h:d2", cat, later())
            .await
            .unwrap();
        assert_eq!(routed.route, Route::Old);
        assert_eq!(routed.routing_version, 3);
        assert_eq!(routed.sticky, attempt == 1);
        let enq = email_delivery::enqueue(&txn, verify_email("otp:verify:h:d2"))
            .await
            .unwrap();
        assert_eq!(enq.created, attempt == 0);
        txn.commit().await.unwrap();
    }
    // Back to FENCED_NEW: the OLD decision stays OLD.
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "AUTO", 4).await;
    let still = fence::route_message(&db.conn, "otp:verify:h:d2", cat, later())
        .await
        .unwrap();
    assert_eq!((still.route, still.sticky), (Route::Old, true));
    for key in ["otp:verify:h:d1", "otp:verify:h:d2"] {
        assert_eq!(
            count(
                &db.conn,
                "notification_route_decisions",
                "idempotency_key",
                key
            )
            .await,
            1
        );
    }
    assert_eq!(
        count(
            &db.conn,
            "email_deliveries",
            "idempotency_key",
            "otp:verify:h:d2"
        )
        .await,
        1
    );

    // Concurrent producers of the same key: exactly one decision, both follow it.
    set_mode(&db.conn, cat, "FENCED_NEW", 50, "AUTO", 5).await;
    let mut handles = Vec::new();
    for _ in 0..8 {
        let conn = db.conn.clone();
        handles.push(tokio::spawn(async move {
            let txn = conn.begin().await.unwrap();
            let r = fence::route_message(&txn, "otp:verify:h:race", cat, later())
                .await
                .unwrap();
            txn.commit().await.unwrap();
            r
        }));
    }
    let mut ids = std::collections::BTreeSet::new();
    for h in handles {
        ids.insert(h.await.unwrap().decision_id);
    }
    assert_eq!(ids.len(), 1);
    assert_eq!(
        count(
            &db.conn,
            "notification_route_decisions",
            "idempotency_key",
            "otp:verify:h:race"
        )
        .await,
        1
    );
}

/// SHADOW and FORCED: OLD routes, shadow flag, CRITICAL break-glass.
#[tokio::test]
async fn shadow_and_forced_fallback_route_old() {
    let db = db!();
    set_mode(
        &db.conn,
        MeetingCategory::MeetingNotification,
        "SHADOW",
        100,
        "AUTO",
        2,
    )
    .await;
    let s = fence::route_message(
        &db.conn,
        "reminder:x:1",
        MeetingCategory::MeetingNotification,
        later(),
    )
    .await
    .unwrap();
    assert_eq!((s.route, s.shadow), (Route::Old, true));

    set_mode(
        &db.conn,
        MeetingCategory::AuthPasswordReset,
        "FENCED_NEW",
        100,
        "FORCED",
        2,
    )
    .await;
    let f = fence::route_message(
        &db.conn,
        "otp:reset:h:d",
        MeetingCategory::AuthPasswordReset,
        later(),
    )
    .await
    .unwrap();
    assert_eq!((f.route, f.shadow), (Route::Old, false));
    // FORCED only concerns CRITICAL categories.
    set_mode(
        &db.conn,
        MeetingCategory::MeetingInvitation,
        "FENCED_NEW",
        100,
        "FORCED",
        2,
    )
    .await;
    let n = fence::route_message(
        &db.conn,
        "meeting-invitation:6f1c8a52-3c2a-4a43-9a3e-0d6c1f2b9e10",
        MeetingCategory::MeetingInvitation,
        later(),
    )
    .await
    .unwrap();
    assert_eq!(n.route, Route::New);
    let reason = text(
        &db.conn,
        "SELECT reason FROM notification_route_decisions WHERE idempotency_key = 'otp:reset:h:d'",
    )
    .await;
    assert_eq!(reason, "ROUTED");
}

/// Canary is a pure function of the key: the database decisions equal the pure function,
/// and the same keys yield the same routes in an independent database.
#[tokio::test]
async fn canary_routing_is_deterministic() {
    let a = db!();
    let b = db!();
    let keys: Vec<String> = (0..120).map(|i| format!("otp:verify:canary:{i}")).collect();
    let mut news = 0;
    for db in [&a, &b] {
        set_mode(
            &db.conn,
            MeetingCategory::AuthVerification,
            "FENCED_NEW",
            30,
            "AUTO",
            2,
        )
        .await;
    }
    for key in &keys {
        let ra = fence::route_message(&a.conn, key, MeetingCategory::AuthVerification, later())
            .await
            .unwrap();
        let rb = fence::route_message(&b.conn, key, MeetingCategory::AuthVerification, later())
            .await
            .unwrap();
        let expected = if canary(key, 30) {
            Route::New
        } else {
            Route::Old
        };
        assert_eq!(ra.route, expected, "{key}");
        assert_eq!(rb.route, expected, "{key}");
        news += usize::from(expected == Route::New);
    }
    assert!((10..70).contains(&news), "canary share {news}/120");
}

/// M-11 (email_deliveries): refused without an OLD decision when the category is not
/// LEGACY_ONLY; allowed in LEGACY_ONLY; unrouted ledger categories are never fenced.
#[tokio::test]
async fn m11_guard_trigger_email_deliveries() {
    let db = db!();
    // LEGACY_ONLY: inert (pre-fence binaries write without decisions).
    assert_eq!(
        insert_email(&db.conn, "AUTH_VERIFICATION", "otp:verify:l:1")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        insert_email(&db.conn, "NOTIFICATION", "device-linked:l1")
            .await
            .unwrap(),
        1
    );
    email_delivery::enqueue(&db.conn, verify_email("otp:verify:l:2"))
        .await
        .unwrap();

    for mode in ["SHADOW", "FENCED_OLD", "FENCED_NEW"] {
        set_mode(
            &db.conn,
            MeetingCategory::AuthVerification,
            mode,
            0,
            "AUTO",
            2,
        )
        .await;
        let key = format!("otp:verify:f:{mode}");
        refused(insert_email(&db.conn, "AUTH_VERIFICATION", &key).await);
        assert_eq!(
            count(&db.conn, "email_deliveries", "idempotency_key", &key).await,
            0
        );
        // The producer path fails loudly too (no second copy is ever created).
        assert!(email_delivery::enqueue(&db.conn, verify_email(&key))
            .await
            .is_err());
        // With an OLD decision in the same transaction, the old-path row is allowed.
        let txn = db.conn.begin().await.unwrap();
        let r = fence::route_message(&txn, &key, MeetingCategory::AuthVerification, later())
            .await
            .unwrap();
        assert_eq!(r.route, Route::Old);
        email_delivery::enqueue(&txn, verify_email(&key))
            .await
            .unwrap();
        txn.commit().await.unwrap();
        assert_eq!(
            count(&db.conn, "email_deliveries", "idempotency_key", &key).await,
            1
        );
    }

    // A NEW decision never gets an old-path row.
    set_mode(
        &db.conn,
        MeetingCategory::AuthVerification,
        "FENCED_NEW",
        100,
        "AUTO",
        3,
    )
    .await;
    let r = fence::route_message(
        &db.conn,
        "otp:verify:n:1",
        MeetingCategory::AuthVerification,
        later(),
    )
    .await
    .unwrap();
    assert_eq!(r.route, Route::New);
    refused(insert_email(&db.conn, "AUTH_VERIFICATION", "otp:verify:n:1").await);

    // Device notices are recorded under NOTIFICATION with a device-linked key.
    set_mode(
        &db.conn,
        MeetingCategory::SecurityDeviceLinked,
        "FENCED_OLD",
        0,
        "AUTO",
        2,
    )
    .await;
    refused(insert_email(&db.conn, "NOTIFICATION", "device-linked:l2").await);
    // Other NOTIFICATION mail, welcome mail, acceptance mail: never fenced.
    assert_eq!(
        insert_email(&db.conn, "NOTIFICATION", "register-existing:u:2026111612")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        insert_email(&db.conn, "AUTH_WELCOME", "welcome:u")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        insert_email(&db.conn, "OPERATIONS_ACCEPTANCE", "acceptance:r")
            .await
            .unwrap(),
        1
    );

    // Every mapped ledger category is fenced once its routing category leaves LEGACY_ONLY.
    for (ledger, routing, key) in [
        (
            "AUTH_PASSWORD_RESET",
            MeetingCategory::AuthPasswordReset,
            "otp:reset:x:y",
        ),
        (
            "SECURITY_DEVICE_STEP_UP",
            MeetingCategory::SecurityDeviceStepUp,
            "device-step-up:x:y",
        ),
        (
            "SECURITY_PASSWORD_CHANGED",
            MeetingCategory::SecurityPasswordChanged,
            "password-changed:x:1",
        ),
        (
            "MEETING_INVITATION",
            MeetingCategory::MeetingInvitation,
            "msg:invitation-x",
        ),
        (
            "ORGANIZATION_INVITATION",
            MeetingCategory::OrganizationInvitation,
            "msg:org-x",
        ),
        (
            "MEETING_NOTIFICATION",
            MeetingCategory::MeetingNotification,
            "msg:notice-x",
        ),
    ] {
        assert_eq!(
            insert_email(&db.conn, ledger, &format!("{key}:legacy"))
                .await
                .unwrap(),
            1
        );
        set_mode(&db.conn, routing, "FENCED_OLD", 0, "AUTO", 2).await;
        refused(insert_email(&db.conn, ledger, key).await);
    }
}

/// M-11 (meeting_notifications): the job is fenced by its dedupe_key; the per-recipient
/// email fan-out is covered by the job's decision.
#[tokio::test]
async fn m11_guard_trigger_meeting_notifications() {
    let db = db!();
    let cat = MeetingCategory::MeetingNotification;
    let legacy_job = Uuid::new_v4();
    for (kind, key) in [
        ("CONFERENCE_REMINDER", "reminder:c1:1700000000"),
        ("CONFERENCE_RESCHEDULED", "rescheduled:c1:2"),
        ("CONFERENCE_CANCELLED", "cancelled:c1"),
    ] {
        let id = if kind == "CONFERENCE_REMINDER" {
            legacy_job
        } else {
            Uuid::new_v4()
        };
        assert_eq!(insert_job(&db.conn, id, kind, key).await.unwrap(), 1);
    }

    set_mode(&db.conn, cat, "FENCED_OLD", 0, "AUTO", 2).await;
    for (kind, key) in [
        ("CONFERENCE_REMINDER", "reminder:c2:1700000000"),
        ("CONFERENCE_RESCHEDULED", "rescheduled:c2:2"),
        ("CONFERENCE_CANCELLED", "cancelled:c2"),
    ] {
        refused(insert_job(&db.conn, Uuid::new_v4(), kind, key).await);
        assert_eq!(
            count(&db.conn, "meeting_notifications", "dedupe_key", key).await,
            0
        );
    }
    // With an OLD decision in the same transaction.
    let job = Uuid::new_v4();
    let txn = db.conn.begin().await.unwrap();
    let r = fence::route_message(&txn, "reminder:c3:1700000000", cat, later())
        .await
        .unwrap();
    assert_eq!(r.route, Route::Old);
    insert_job(&txn, job, "CONFERENCE_REMINDER", "reminder:c3:1700000000")
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Fan-out of an OLD job, and of a job created before the fence: allowed.
    let digest = "b".repeat(64);
    for parent in [job, legacy_job] {
        let key = format!("meeting-notification:{parent}:{digest}");
        assert_eq!(
            insert_email(&db.conn, "MEETING_NOTIFICATION", &key)
                .await
                .unwrap(),
            1
        );
    }
    // Fan-out of an unknown job: needs its own decision.
    refused(
        insert_email(
            &db.conn,
            "MEETING_NOTIFICATION",
            &format!("meeting-notification:{}:{digest}", Uuid::new_v4()),
        )
        .await,
    );
    // A job whose decision is NEW (pre-fence job, later decided NEW by a fence-capable
    // producer retry) never fans out on the old path.
    let pre = Uuid::new_v4();
    set_mode(&db.conn, cat, "LEGACY_ONLY", 0, "AUTO", 3).await;
    insert_job(&db.conn, pre, "CONFERENCE_CANCELLED", "cancelled:c4")
        .await
        .unwrap();
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "AUTO", 4).await;
    let r = fence::route_message(&db.conn, "cancelled:c4", cat, later())
        .await
        .unwrap();
    assert_eq!(r.route, Route::New);
    refused(
        insert_email(
            &db.conn,
            "MEETING_NOTIFICATION",
            &format!("meeting-notification:{pre}:{digest}"),
        )
        .await,
    );
}

/// F3: a decision changes only through an audited reroute; completed is terminal; a
/// decision is deleted only after it expires.
#[tokio::test]
async fn f3_decision_row_guard() {
    let db = db!();
    let cat = MeetingCategory::AuthVerification;
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "AUTO", 2).await;
    fence::route_message(&db.conn, "k:new", cat, later())
        .await
        .unwrap();
    set_mode(&db.conn, cat, "FENCED_OLD", 0, "AUTO", 3).await;
    fence::route_message(&db.conn, "k:old", cat, later())
        .await
        .unwrap();
    let w = |set: &str, key: &str| {
        format!("UPDATE notification_route_decisions SET {set} WHERE idempotency_key = '{key}'")
    };

    // Inserts are ROUTED, version 1, unleased.
    immutable(
        exec(
            &db.conn,
            "INSERT INTO notification_route_decisions (idempotency_key, decision_id, category, route, routing_version, reason, expires_at)
             VALUES ('k:bad', gen_random_uuid(), 'AUTH_VERIFICATION', 'OLD', 1, 'AUTO_FAILOVER', now() + interval '1 day')",
        )
        .await,
    );
    // Identity is immutable.
    for set in [
        "category = 'MEETING_NOTIFICATION'",
        "decision_id = gen_random_uuid()",
        "routing_version = 9",
        "expires_at = now() + interval '9 days'",
        "decided_at = now() - interval '1 day'",
    ] {
        immutable(exec(&db.conn, &w(set, "k:new")).await);
    }
    // OLD -> NEW never; silent changes never.
    immutable(
        exec(
            &db.conn,
            &w(
                "route = 'NEW', decision_version = 2, reason = 'AUTO_FAILOVER'",
                "k:old",
            ),
        )
        .await,
    );
    immutable(exec(&db.conn, &w("reason = 'AUTO_FAILOVER'", "k:new")).await);
    immutable(exec(&db.conn, &w("decision_version = 2", "k:new")).await);
    immutable(exec(&db.conn, &w("route = 'OLD'", "k:new")).await);
    immutable(
        exec(
            &db.conn,
            &w(
                "route = 'OLD', decision_version = 2, reason = 'ROUTED'",
                "k:new",
            ),
        )
        .await,
    );
    immutable(
        exec(
            &db.conn,
            &w(
                "route = 'OLD', decision_version = 3, reason = 'REROUTED_UNACKED'",
                "k:new",
            ),
        )
        .await,
    );
    // Leases: one grant at a time (+1 with a lease time), only on NEW.
    immutable(
        exec(
            &db.conn,
            &w("send_lease_until = now() + interval '30 seconds'", "k:new"),
        )
        .await,
    );
    immutable(
        exec(
            &db.conn,
            &w("lease_count = 2, send_lease_until = now()", "k:new"),
        )
        .await,
    );
    immutable(
        exec(
            &db.conn,
            &w("lease_count = 1, send_lease_until = now()", "k:old"),
        )
        .await,
    );
    // Acknowledgement only for NEW, once.
    immutable(exec(&db.conn, &w("ns_acked_at = now()", "k:old")).await);
    assert_eq!(
        exec(&db.conn, &w("ns_acked_at = now()", "k:new"))
            .await
            .unwrap(),
        1
    );
    immutable(
        exec(
            &db.conn,
            &w("ns_acked_at = now() + interval '1 second'", "k:new"),
        )
        .await,
    );
    immutable(exec(&db.conn, &w("ns_acked_at = NULL", "k:new")).await);
    // Completion requires a granted lease.
    immutable(exec(&db.conn, &w("ns_completed_at = now()", "k:new")).await);
    let grant =
        fence::grant_critical_lease(&db.conn, "k:new", std::time::Duration::from_millis(20))
            .await
            .unwrap();
    assert!(matches!(grant, LeaseGrant::Granted { lease_count: 1, .. }));
    assert_eq!(
        fence::complete_critical_send(&db.conn, "k:new")
            .await
            .unwrap(),
        fence::Completion::Completed
    );
    // Completed: terminal (no reroute even after the lease lapsed, no new lease, no reset).
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    immutable(
        exec(
            &db.conn,
            &w(
                "route = 'OLD', decision_version = 2, reason = 'REROUTED_POSSIBLE_DUPLICATE'",
                "k:new",
            ),
        )
        .await,
    );
    immutable(exec(&db.conn, &w("ns_completed_at = NULL", "k:new")).await);
    immutable(
        exec(
            &db.conn,
            &w(
                "lease_count = 2, send_lease_until = now() + interval '1 minute'",
                "k:new",
            ),
        )
        .await,
    );
    // Delete only after expiry.
    immutable(
        exec(
            &db.conn,
            "DELETE FROM notification_route_decisions WHERE idempotency_key = 'k:old'",
        )
        .await,
    );
    fence::route_message(
        &db.conn,
        "k:expired",
        cat,
        Utc::now() - Duration::minutes(1),
    )
    .await
    .unwrap();
    assert_eq!(
        exec(
            &db.conn,
            "DELETE FROM notification_route_decisions WHERE idempotency_key = 'k:expired'"
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        scalar_i64(&db.conn, "SELECT count(*) FROM notification_route_decisions WHERE idempotency_key IN ('k:new', 'k:old')").await,
        2
    );
}

async fn insert_outbox(conn: &impl ConnectionTrait, decision_id: Uuid, state: &str, expires: &str) {
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "INSERT INTO account_notification_outbox (id, decision_id, event, fallback_sealed, state, expires_at)
             VALUES ($1, $2, '{{\"sealed\":true}}'::jsonb, '\\x00'::bytea, $3, {expires})"
        ),
        [Uuid::new_v4().into(), decision_id.into(), state.into()],
    ))
    .await
    .unwrap();
}

async fn outbox_state(conn: &impl ConnectionTrait, decision_id: Uuid) -> String {
    conn.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT state FROM account_notification_outbox WHERE decision_id = $1",
        [decision_id.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get_by_index(0)
    .unwrap()
}

async fn decision(conn: &impl ConnectionTrait, key: &str) -> (String, i32, String) {
    let r = conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT route, decision_version, reason FROM notification_route_decisions WHERE idempotency_key = $1",
            [key.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    (
        r.try_get("", "route").unwrap(),
        r.try_get("", "decision_version").unwrap(),
        r.try_get("", "reason").unwrap(),
    )
}

/// M-15 (database transition): an unacknowledged NEW decision is rerouted unilaterally in
/// one producer transaction (decision OLD, outbox SUPERSEDED, old-path row created); an
/// acknowledged one is not.
#[tokio::test]
async fn m15_unilateral_reroute_of_unacked_decision() {
    let db = db!();
    let cat = MeetingCategory::AuthVerification;
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "AUTO", 2).await;
    let key = "otp:verify:u:1";
    let txn = db.conn.begin().await.unwrap();
    let r = fence::route_message(&txn, key, cat, later()).await.unwrap();
    assert_eq!(r.route, Route::New);
    insert_outbox(&txn, r.decision_id, "PENDING", "now() + interval '1 hour'").await;
    txn.commit().await.unwrap();
    refused(insert_email(&db.conn, "AUTH_VERIFICATION", key).await);

    // The dryRun probe answered UNKNOWN (caller's job): reroute + old-path row, one txn.
    let txn = db.conn.begin().await.unwrap();
    let out = fence::reroute_unacked(&txn, key).await.unwrap();
    assert_eq!(
        out,
        UnackedReroute::Rerouted {
            decision_id: r.decision_id,
            decision_version: 2
        }
    );
    email_delivery::enqueue(&txn, verify_email(key))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        decision(&db.conn, key).await,
        ("OLD".into(), 2, "REROUTED_UNACKED".into())
    );
    assert_eq!(outbox_state(&db.conn, r.decision_id).await, "SUPERSEDED");
    assert_eq!(
        count(&db.conn, "email_deliveries", "idempotency_key", key).await,
        1
    );
    assert_eq!(
        fence::reroute_unacked(&db.conn, key).await.unwrap(),
        UnackedReroute::AlreadyOld
    );
    // The service can never claim a lease on it now.
    assert_eq!(
        fence::grant_critical_lease(&db.conn, key, fence::CRITICAL_LEASE)
            .await
            .unwrap(),
        LeaseGrant::Revoked
    );
    // Sticky after the reroute: a retry follows OLD.
    let again = fence::route_message(&db.conn, key, cat, later())
        .await
        .unwrap();
    assert_eq!(
        (again.route, again.sticky, again.decision_version),
        (Route::Old, true, 2)
    );

    // Acknowledged (I1 ACCEPTED): refused, nothing changes.
    let acked = fence::route_message(&db.conn, "otp:verify:u:2", cat, later())
        .await
        .unwrap();
    insert_outbox(
        &db.conn,
        acked.decision_id,
        "ACKED",
        "now() + interval '1 hour'",
    )
    .await;
    exec(&db.conn, "UPDATE notification_route_decisions SET ns_acked_at = now() WHERE idempotency_key = 'otp:verify:u:2'")
        .await
        .unwrap();
    assert_eq!(
        fence::reroute_unacked(&db.conn, "otp:verify:u:2")
            .await
            .unwrap(),
        UnackedReroute::Acknowledged
    );
    assert_eq!(
        decision(&db.conn, "otp:verify:u:2").await,
        ("NEW".into(), 1, "ROUTED".into())
    );
    assert_eq!(outbox_state(&db.conn, acked.decision_id).await, "ACKED");
    // Outbox ACKED but the decision ack not yet recorded: still refused.
    let relayed = fence::route_message(&db.conn, "otp:verify:u:3", cat, later())
        .await
        .unwrap();
    insert_outbox(
        &db.conn,
        relayed.decision_id,
        "ACKED",
        "now() + interval '1 hour'",
    )
    .await;
    assert_eq!(
        fence::reroute_unacked(&db.conn, "otp:verify:u:3")
            .await
            .unwrap(),
        UnackedReroute::Acknowledged
    );
    // Leased: the service has it.
    let leased = fence::route_message(&db.conn, "otp:verify:u:4", cat, later())
        .await
        .unwrap();
    assert_eq!(leased.route, Route::New);
    fence::grant_critical_lease(
        &db.conn,
        "otp:verify:u:4",
        std::time::Duration::from_millis(1),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(
        fence::reroute_unacked(&db.conn, "otp:verify:u:4")
            .await
            .unwrap(),
        UnackedReroute::Acknowledged
    );
    assert_eq!(
        fence::reroute_unacked(&db.conn, "otp:verify:none")
            .await
            .unwrap(),
        UnackedReroute::NotFound
    );
    // Non-CRITICAL categories reroute the same way (their outbox is the caller's).
    set_mode(
        &db.conn,
        MeetingCategory::MeetingNotification,
        "FENCED_NEW",
        100,
        "AUTO",
        2,
    )
    .await;
    let std = fence::route_message(
        &db.conn,
        "reminder:u:1",
        MeetingCategory::MeetingNotification,
        later(),
    )
    .await
    .unwrap();
    assert_eq!(std.route, Route::New);
    assert!(matches!(
        fence::reroute_unacked(&db.conn, "reminder:u:1")
            .await
            .unwrap(),
        UnackedReroute::Rerouted {
            decision_version: 2,
            ..
        }
    ));
}

/// M-17: never-leased → automatic reroute, exact; live lease → blocked; completed →
/// terminal; leased-uncompleted → FORCED only, recorded REROUTED_POSSIBLE_DUPLICATE.
#[tokio::test]
async fn m17_critical_lease_rules() {
    let db = db!();
    let cat = MeetingCategory::AuthPasswordReset;
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "AUTO", 2).await;
    let route = |key: &'static str| {
        let conn = db.conn.clone();
        async move {
            let r = fence::route_message(&conn, key, cat, later())
                .await
                .unwrap();
            assert_eq!(r.route, Route::New);
            insert_outbox(&conn, r.decision_id, "ACKED", "now() + interval '1 hour'").await;
            exec(
                &conn,
                &format!("UPDATE notification_route_decisions SET ns_acked_at = now() WHERE idempotency_key = '{key}'"),
            )
            .await
            .unwrap();
            r
        }
    };

    // Never leased: AUTO failover, exact (no duplicate possible); later X6 answers REVOKED.
    let a = route("otp:reset:a").await;
    let txn = db.conn.begin().await.unwrap();
    let out = fence::reroute_critical(&txn, "otp:reset:a", true)
        .await
        .unwrap();
    assert_eq!(
        out,
        CriticalReroute::Rerouted {
            reason: DecisionReason::AutoFailover,
            decision_id: a.decision_id
        }
    );
    email_delivery::enqueue(
        &txn,
        NewEmail {
            category: category::AUTH_PASSWORD_RESET,
            ..verify_email("otp:reset:a")
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        decision(&db.conn, "otp:reset:a").await,
        ("OLD".into(), 2, "AUTO_FAILOVER".into())
    );
    assert_eq!(outbox_state(&db.conn, a.decision_id).await, "SUPERSEDED");
    assert_eq!(
        fence::grant_critical_lease(&db.conn, "otp:reset:a", fence::CRITICAL_LEASE)
            .await
            .unwrap(),
        LeaseGrant::Revoked
    );
    // Manual reroute of a never-leased decision.
    route("otp:reset:m").await;
    assert!(matches!(
        fence::reroute_critical(&db.conn, "otp:reset:m", false)
            .await
            .unwrap(),
        CriticalReroute::Rerouted {
            reason: DecisionReason::ReroutedCriticalLease,
            ..
        }
    ));

    // Live lease: blocked (even FORCED), and a second claim is busy.
    route("otp:reset:live").await;
    let g = fence::grant_critical_lease(&db.conn, "otp:reset:live", fence::CRITICAL_LEASE)
        .await
        .unwrap();
    assert!(matches!(g, LeaseGrant::Granted { lease_count: 1, .. }));
    assert_eq!(
        fence::grant_critical_lease(&db.conn, "otp:reset:live", fence::CRITICAL_LEASE)
            .await
            .unwrap(),
        LeaseGrant::Busy
    );
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "FORCED", 3).await;
    assert_eq!(
        fence::reroute_critical(&db.conn, "otp:reset:live", false)
            .await
            .unwrap(),
        CriticalReroute::WaitForLease
    );
    assert_eq!(
        fence::reroute_critical(&db.conn, "otp:reset:live", true)
            .await
            .unwrap(),
        CriticalReroute::WaitForLease
    );
    // The trigger independently refuses a reroute under a live lease.
    immutable(
        exec(
            &db.conn,
            "UPDATE notification_route_decisions SET route = 'OLD', decision_version = 2, reason = 'REROUTED_POSSIBLE_DUPLICATE'
              WHERE idempotency_key = 'otp:reset:live'",
        )
        .await,
    );
    // Completed: terminal.
    assert_eq!(
        fence::complete_critical_send(&db.conn, "otp:reset:live")
            .await
            .unwrap(),
        fence::Completion::Completed
    );
    assert_eq!(
        fence::complete_critical_send(&db.conn, "otp:reset:live")
            .await
            .unwrap(),
        fence::Completion::AlreadyCompleted
    );
    assert_eq!(
        fence::grant_critical_lease(&db.conn, "otp:reset:live", fence::CRITICAL_LEASE)
            .await
            .unwrap(),
        LeaseGrant::Completed
    );
    assert_eq!(
        decision(&db.conn, "otp:reset:live").await,
        ("NEW".into(), 1, "ROUTED".into())
    );

    // Leased, lease expired, not completed (the service crashed mid-send).
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "AUTO", 4).await;
    route("otp:reset:crash").await;
    fence::grant_critical_lease(
        &db.conn,
        "otp:reset:crash",
        std::time::Duration::from_millis(30),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    assert_eq!(
        fence::reroute_critical(&db.conn, "otp:reset:crash", true)
            .await
            .unwrap(),
        CriticalReroute::Refused
    );
    assert_eq!(
        fence::reroute_critical(&db.conn, "otp:reset:crash", false)
            .await
            .unwrap(),
        CriticalReroute::Refused
    );
    // A new lease may still be granted to the service (re-claim after its crash).
    assert!(matches!(
        fence::grant_critical_lease(
            &db.conn,
            "otp:reset:crash",
            std::time::Duration::from_millis(30)
        )
        .await
        .unwrap(),
        LeaseGrant::Granted { lease_count: 2, .. }
    ));
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "FORCED", 5).await;
    assert_eq!(
        fence::reroute_critical(&db.conn, "otp:reset:crash", true)
            .await
            .unwrap(),
        CriticalReroute::Refused
    );
    assert!(matches!(
        fence::reroute_critical(&db.conn, "otp:reset:crash", false)
            .await
            .unwrap(),
        CriticalReroute::Rerouted {
            reason: DecisionReason::ReroutedPossibleDuplicate,
            ..
        }
    ));
    assert_eq!(
        decision(&db.conn, "otp:reset:crash").await,
        ("OLD".into(), 2, "REROUTED_POSSIBLE_DUPLICATE".into())
    );
    // The service's late X7 is refused after the reroute.
    assert_eq!(
        fence::complete_critical_send(&db.conn, "otp:reset:crash")
            .await
            .unwrap(),
        fence::Completion::Revoked
    );

    // OFF disables the automatic failover; an expired code is never rerouted.
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "OFF", 6).await;
    route("otp:reset:off").await;
    assert_eq!(
        fence::reroute_critical(&db.conn, "otp:reset:off", true)
            .await
            .unwrap(),
        CriticalReroute::Refused
    );
    set_mode(&db.conn, cat, "FENCED_NEW", 100, "AUTO", 7).await;
    let exp = fence::route_message(&db.conn, "otp:reset:expired", cat, later())
        .await
        .unwrap();
    insert_outbox(
        &db.conn,
        exp.decision_id,
        "PENDING",
        "now() - interval '1 second'",
    )
    .await;
    assert_eq!(
        fence::reroute_critical(&db.conn, "otp:reset:expired", true)
            .await
            .unwrap(),
        CriticalReroute::Refused
    );

    // Not leased: completion refused; non-CRITICAL categories have no leases.
    route("otp:reset:nolease").await;
    assert_eq!(
        fence::complete_critical_send(&db.conn, "otp:reset:nolease")
            .await
            .unwrap(),
        fence::Completion::NotLeased
    );
    set_mode(
        &db.conn,
        MeetingCategory::MeetingInvitation,
        "FENCED_NEW",
        100,
        "AUTO",
        2,
    )
    .await;
    fence::route_message(
        &db.conn,
        "meeting-invitation:x",
        MeetingCategory::MeetingInvitation,
        later(),
    )
    .await
    .unwrap();
    assert_eq!(
        fence::grant_critical_lease(&db.conn, "meeting-invitation:x", fence::CRITICAL_LEASE)
            .await
            .unwrap(),
        LeaseGrant::NotCritical
    );
    assert_eq!(
        fence::reroute_critical(&db.conn, "meeting-invitation:x", false)
            .await
            .unwrap(),
        CriticalReroute::NotCritical
    );
    assert_eq!(
        fence::grant_critical_lease(&db.conn, "otp:reset:none", fence::CRITICAL_LEASE)
            .await
            .unwrap(),
        LeaseGrant::NotFound
    );
    assert_eq!(
        scalar_i64(&db.conn, "SELECT count(*) FROM notification_route_decisions WHERE reason = 'REROUTED_POSSIBLE_DUPLICATE'").await,
        1
    );
}

/// `routingProtocol` is reported once the fence history is applied, and absent otherwise.
#[tokio::test]
async fn readiness_reports_routing_protocol() {
    let app = match TestApp::new().await {
        Some(a) => a,
        None => return,
    };
    let (s, v) = app.get("/health", Auth::None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["routingProtocol"], 1);
    assert_eq!(v["status"], "healthy");
    assert_eq!(fence::fence_protocol(&app.db.conn).await, Some(1));

    // A pre-fence database (history not applied): absent, health unaffected.
    exec(&app.db.conn, "DELETE FROM notification_fence_migrations")
        .await
        .unwrap();
    let (s, v) = app.get("/health", Auth::None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v.get("routingProtocol").is_none(), "{v}");
    assert_eq!(fence::fence_protocol(&app.db.conn).await, None);
    // A history version unknown to this binary: fail closed (absent).
    exec(
        &app.db.conn,
        "INSERT INTO notification_fence_migrations (version, applied_at) VALUES ('m20991231_000001_future', 0)",
    )
    .await
    .unwrap();
    assert_eq!(fence::fence_protocol(&app.db.conn).await, None);

    let empty = match TestDb::empty().await {
        Some(d) => d,
        None => return,
    };
    assert_eq!(fence::fence_protocol(&empty.conn).await, None);
}

/// The history is re-runnable and `down` refuses while any category is fenced (the
/// database half of the rollback floor).
#[tokio::test]
async fn fence_history_down_requires_legacy_only() {
    let db = db!();
    set_mode(
        &db.conn,
        MeetingCategory::MeetingInvitation,
        "SHADOW",
        0,
        "AUTO",
        2,
    )
    .await;
    assert!(migration::NotificationFenceMigrator::down(&db.conn, None)
        .await
        .is_err());
    set_mode(
        &db.conn,
        MeetingCategory::MeetingInvitation,
        "LEGACY_ONLY",
        0,
        "AUTO",
        3,
    )
    .await;
    migration::NotificationFenceMigrator::down(&db.conn, None)
        .await
        .unwrap();
    // Pre-fence schema: old-path writes work without any fence object.
    assert_eq!(
        insert_email(&db.conn, "AUTH_VERIFICATION", "otp:verify:down")
            .await
            .unwrap(),
        1
    );
    migration::NotificationFenceMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert_eq!(
        scalar_i64(
            &db.conn,
            "SELECT count(*) FROM notification_routing_state WHERE mode = 'LEGACY_ONLY'"
        )
        .await,
        9
    );
    assert!(migration::notification_fence_ready(&db.conn).await.unwrap());
}

/// ACCOUNT_NOTICES (N12 follow-up): inert in LEGACY_ONLY; once fenced, the welcome
/// (`AUTH_WELCOME`) and the `NOTIFICATION` notices keyed `new-sign-in:`, `email-in-use:` and
/// `email-changed:` need a decision, while device notices stay with SECURITY_DEVICE_LINKED
/// and every other notice stays unfenced. `down` refuses while the category is fenced or a
/// decision uses it, and restores the previous mapping.
#[tokio::test]
async fn account_notices_are_fenced_only_outside_legacy_only() {
    let db = db!();
    let notices = [
        (
            "AUTH_WELCOME",
            "welcome:5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607",
        ),
        (
            "NOTIFICATION",
            "new-sign-in:5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607",
        ),
        ("NOTIFICATION", "email-in-use:abc:2026111812"),
        (
            "NOTIFICATION",
            "email-changed:old:5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607",
        ),
        (
            "NOTIFICATION",
            "email-changed:new:5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607",
        ),
    ];
    assert_eq!(
        text(
            &db.conn,
            "SELECT mode FROM notification_routing_state WHERE category = 'ACCOUNT_NOTICES'"
        )
        .await,
        "LEGACY_ONLY"
    );
    for (ledger, key) in notices {
        assert_eq!(
            fence_category(&db.conn, ledger, key).await.as_deref(),
            Some("ACCOUNT_NOTICES"),
            "{key}"
        );
        // Inert in LEGACY_ONLY: today's rows are written as today.
        assert_eq!(
            insert_email(&db.conn, ledger, &format!("{key}:legacy"))
                .await
                .unwrap(),
            1
        );
    }
    set_mode(
        &db.conn,
        MeetingCategory::AccountNotices,
        "FENCED_OLD",
        0,
        "AUTO",
        2,
    )
    .await;
    for (ledger, key) in notices {
        refused(insert_email(&db.conn, ledger, key).await);
        // With an OLD decision the row is allowed (the decision already expired, so the
        // test can purge it below).
        let txn = db.conn.begin().await.unwrap();
        let r = fence::route_message(
            &txn,
            key,
            MeetingCategory::AccountNotices,
            Utc::now() - Duration::seconds(1),
        )
        .await
        .unwrap();
        assert_eq!(r.route, Route::Old);
        assert_eq!(insert_email(&txn, ledger, key).await.unwrap(), 1);
        txn.commit().await.unwrap();
    }
    // Device notices keep their own category (LEGACY_ONLY here); other notices are never
    // fenced.
    for (ledger, key) in [
        ("NOTIFICATION", "device-linked:l9"),
        ("NOTIFICATION", "security-notice:mfa-enabled:x"),
        ("NOTIFICATION", "register-existing:u:2026111812"),
        ("OPERATIONS_ACCEPTANCE", "acceptance:r9"),
    ] {
        assert_eq!(
            insert_email(&db.conn, ledger, key).await.unwrap(),
            1,
            "{key}"
        );
    }
    assert_eq!(
        fence_category(&db.conn, "NOTIFICATION", "device-linked:x")
            .await
            .as_deref(),
        Some("SECURITY_DEVICE_LINKED")
    );

    // down: refused while fenced, refused while a decision uses the category.
    assert!(
        migration::NotificationFenceMigrator::down(&db.conn, Some(1))
            .await
            .is_err()
    );
    set_mode(
        &db.conn,
        MeetingCategory::AccountNotices,
        "LEGACY_ONLY",
        0,
        "AUTO",
        3,
    )
    .await;
    assert!(
        migration::NotificationFenceMigrator::down(&db.conn, Some(1))
            .await
            .is_err(),
        "decisions use ACCOUNT_NOTICES"
    );
    exec(
        &db.conn,
        "DELETE FROM notification_route_decisions WHERE category = 'ACCOUNT_NOTICES'",
    )
    .await
    .unwrap();
    migration::NotificationFenceMigrator::down(&db.conn, Some(1))
        .await
        .unwrap();
    assert_eq!(
        scalar_i64(
            &db.conn,
            "SELECT count(*) FROM notification_routing_state WHERE category = 'ACCOUNT_NOTICES'"
        )
        .await,
        0
    );
    for (ledger, key) in notices {
        assert_eq!(fence_category(&db.conn, ledger, key).await, None, "{key}");
    }
    assert_eq!(
        fence_category(&db.conn, "NOTIFICATION", "device-linked:x")
            .await
            .as_deref(),
        Some("SECURITY_DEVICE_LINKED")
    );
    migration::NotificationFenceMigrator::up(&db.conn, None)
        .await
        .unwrap();
    assert!(migration::notification_fence_ready(&db.conn).await.unwrap());
    assert_eq!(
        fence_category(&db.conn, "AUTH_WELCOME", "welcome:x")
            .await
            .as_deref(),
        Some("ACCOUNT_NOTICES")
    );
}

/// The routing category the `email_deliveries` guard trigger maps a ledger row to.
async fn fence_category(conn: &DatabaseConnection, ledger: &str, key: &str) -> Option<String> {
    conn.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT notification_fence_email_category($1, $2) AS c",
        [ledger.into(), key.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get::<Option<String>>("", "c")
    .unwrap()
}
