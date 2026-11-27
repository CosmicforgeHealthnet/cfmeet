//! Routing fence protocol v1, Messaging side (Document 4 §9; acceptance matrix rows
//! M-10 / M-11, readiness half of M-12): the decision ledger, the sticky decision
//! algorithm, the canary, the decision update guard (F3), the `messaging_push_jobs`
//! guard trigger (§9.4) on the real push fan-out path, and `routingProtocol` in `/ready`.
//! Real PostgreSQL; the harness template applies the fence history.
mod common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use common::{flows::*, realtime::cid, *};
use messaging::{
    events::outbox::OutboxEvent,
    notification_fence::{
        self as fence, canary, fence_protocol, push_key, route_message, Decision, FenceError,
        Route, MESSAGE_WAKEUPS,
    },
    realtime::push,
};
use migration::MigratorTrait;
use sea_orm::{ConnectionTrait, FromQueryResult, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use messaging_migration as migration;

async fn ready(h: &Harness) -> Response {
    h.raw(Request::get("/ready").body(Body::empty()).unwrap())
        .await
}

async fn set_mode(h: &Harness, mode: &str, canary_percent: i16) {
    h.sql(&format!(
        "UPDATE messaging_notification_routing_state \
            SET mode = '{mode}', canary_percent = {canary_percent}, routing_version = routing_version + 1, \
                changed_by_run = 'test', changed_at = now() \
          WHERE category = 'MESSAGE_WAKEUPS'"
    ))
    .await
    .unwrap();
}

/// One `route_message` call in its own committed transaction.
async fn route(h: &Harness, key: &str, category: &str) -> Result<Decision, FenceError> {
    let txn = h.conn().begin().await.unwrap();
    let decision = route_message(&txn, key, category, Utc::now() + Duration::hours(24), true).await;
    txn.commit().await.unwrap();
    decision
}

async fn decisions(h: &Harness, key: &str) -> i64 {
    h.scalar(&format!(
        "SELECT count(*) FROM messaging_notification_route_decisions WHERE idempotency_key = '{key}'"
    ))
    .await
}

/// A key the canary puts inside (`true`) or outside (`false`) a 50 % canary.
fn key_with_canary(inside: bool) -> String {
    (0..)
        .map(|_| push_key(Uuid::new_v4()))
        .find(|k| canary(k, 50) == inside)
        .unwrap()
}

#[tokio::test]
async fn migration_seeds_every_messaging_category_legacy_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let rows = h
        .conn()
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT category, mode, canary_percent, critical_fallback, routing_version, protocol, changed_by_run \
               FROM messaging_notification_routing_state ORDER BY category"
                .to_owned(),
        ))
        .await
        .unwrap();
    let seeded: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!([
                r.try_get::<String>("", "category").unwrap(),
                r.try_get::<String>("", "mode").unwrap(),
                r.try_get::<i16>("", "canary_percent").unwrap(),
                r.try_get::<String>("", "critical_fallback").unwrap(),
                r.try_get::<i32>("", "routing_version").unwrap(),
                r.try_get::<i16>("", "protocol").unwrap(),
                r.try_get::<String>("", "changed_by_run").unwrap(),
            ])
        })
        .collect();
    assert_eq!(
        seeded,
        vec![json!([
            "MESSAGE_WAKEUPS",
            "LEGACY_ONLY",
            0,
            "AUTO",
            1,
            1,
            "migration"
        ])]
    );
    assert_eq!(fence::CATEGORIES, &[MESSAGE_WAKEUPS]);
    // CHECK constraints of §9.2.
    for bad in [
        "UPDATE messaging_notification_routing_state SET mode = 'ON'",
        "UPDATE messaging_notification_routing_state SET canary_percent = 101",
        "UPDATE messaging_notification_routing_state SET critical_fallback = 'MAYBE'",
        "INSERT INTO messaging_notification_route_decisions (idempotency_key, decision_id, category, route, routing_version, reason, expires_at) \
           VALUES ('k1', gen_random_uuid(), 'MESSAGE_WAKEUPS', 'BOTH', 1, 'ROUTED', now())",
        "INSERT INTO messaging_notification_route_decisions (idempotency_key, decision_id, category, route, routing_version, reason, expires_at) \
           VALUES ('k2', gen_random_uuid(), 'MESSAGE_WAKEUPS', 'OLD', 1, 'BECAUSE', now())",
    ] {
        assert!(h.sql(bad).await.is_err(), "{bad}");
    }
    // The history is separate from the main one and forward-only.
    assert!(migration::Migrator::get_pending_migrations(h.conn())
        .await
        .unwrap()
        .is_empty());
    assert!(migration::NotificationFenceMigrator::down(h.conn(), None)
        .await
        .is_err());
    assert!(migration::notification_fence_ready(h.conn()).await.unwrap());
}

/// M-10: the same key routed twice yields one decision, also across a mode change.
#[tokio::test]
async fn m10_decisions_are_sticky_across_retries_and_mode_changes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // LEGACY_ONLY: OLD, recorded once with the state's routing version.
    let key = push_key(Uuid::new_v4());
    let first = route(&h, &key, MESSAGE_WAKEUPS).await.unwrap();
    assert_eq!(
        (
            first.route,
            first.created,
            first.routing_version,
            first.decision_version
        ),
        (Route::Old, true, 1, 1)
    );
    let retry = route(&h, &key, MESSAGE_WAKEUPS).await.unwrap();
    assert_eq!(
        (retry.decision_id, retry.route, retry.created),
        (first.decision_id, Route::Old, false)
    );
    // The category moves to FENCED_NEW with a 100 % canary: new keys go NEW, the earlier
    // decision is followed.
    set_mode(&h, "FENCED_NEW", 100).await;
    let after = route(&h, &key, MESSAGE_WAKEUPS).await.unwrap();
    assert_eq!(
        (
            after.decision_id,
            after.route,
            after.created,
            after.routing_version
        ),
        (first.decision_id, Route::Old, false, 1)
    );
    let fresh = push_key(Uuid::new_v4());
    let new = route(&h, &fresh, MESSAGE_WAKEUPS).await.unwrap();
    assert_eq!(
        (new.route, new.created, new.routing_version),
        (Route::New, true, 2)
    );
    // ... and back to FENCED_OLD: the NEW decision stays NEW.
    set_mode(&h, "FENCED_OLD", 100).await;
    let back = route(&h, &fresh, MESSAGE_WAKEUPS).await.unwrap();
    assert_eq!(
        (back.decision_id, back.route, back.created),
        (new.decision_id, Route::New, false)
    );
    assert_eq!(decisions(&h, &key).await, 1);
    assert_eq!(decisions(&h, &fresh).await, 1);
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM messaging_notification_route_decisions WHERE reason = 'ROUTED' AND decision_version = 1"
        )
        .await,
        2
    );

    // A rolled-back decision is no decision.
    let rolled = push_key(Uuid::new_v4());
    let txn = h.conn().begin().await.unwrap();
    route_message(&txn, &rolled, MESSAGE_WAKEUPS, Utc::now(), true)
        .await
        .unwrap();
    txn.rollback().await.unwrap();
    assert_eq!(decisions(&h, &rolled).await, 0);

    // Concurrent first decisions converge on one row.
    set_mode(&h, "FENCED_NEW", 50).await;
    let racing = push_key(Uuid::new_v4());
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let conn = h.conn().clone();
        let key = racing.clone();
        tasks.push(tokio::spawn(async move {
            let txn = conn.begin().await.unwrap();
            let d = route_message(
                &txn,
                &key,
                MESSAGE_WAKEUPS,
                Utc::now() + Duration::hours(1),
                true,
            )
            .await
            .unwrap();
            txn.commit().await.unwrap();
            d
        }));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut created = 0;
    for t in tasks {
        let d = t.await.unwrap();
        ids.insert(d.decision_id);
        created += usize::from(d.created);
    }
    assert_eq!((ids.len(), created), (1, 1));
    assert_eq!(decisions(&h, &racing).await, 1);

    // Unknown categories, invalid keys and a key reused for another category are refused.
    assert!(matches!(
        route(&h, &push_key(Uuid::new_v4()), "GROUP_INVITATIONS").await,
        Err(FenceError::UnknownCategory(_))
    ));
    for bad in [String::new(), "has space".into(), "x".repeat(161)] {
        assert!(matches!(
            route(&h, &bad, MESSAGE_WAKEUPS).await,
            Err(FenceError::InvalidKey)
        ));
    }
    h.sql(
        "INSERT INTO messaging_notification_routing_state (category, mode, routing_version, changed_by_run) \
         VALUES ('OTHER_TEST', 'LEGACY_ONLY', 1, 'test')",
    )
    .await
    .unwrap();
    assert!(matches!(
        route(&h, &key, "OTHER_TEST").await,
        Err(FenceError::CategoryMismatch { .. })
    ));
}

/// The canary is a pure function of the key; modes other than FENCED_NEW never route NEW.
#[tokio::test]
async fn canary_is_deterministic_and_bounds_new_routes() {
    // Pure: same key, same answer; 0 % never, 100 % always.
    for _ in 0..200 {
        let k = push_key(Uuid::new_v4());
        assert_eq!(canary(&k, 37), canary(&k, 37));
        assert!(!canary(&k, 0));
        assert!(canary(&k, 100));
        // Monotone in the percentage.
        assert!(!canary(&k, 20) || canary(&k, 21));
    }
    // A stable function (SHA-256 prefix): pinned values never change between releases.
    let pinned: Vec<bool> = ["evt_00000000000000000000000000000000", "otp:verify:1", "a"]
        .iter()
        .map(|k| canary(k, 50))
        .collect();
    let expected: Vec<bool> = ["evt_00000000000000000000000000000000", "otp:verify:1", "a"]
        .iter()
        .map(|k| {
            use sha2::{Digest, Sha256};
            let d = Sha256::digest(k.as_bytes());
            u64::from_be_bytes(d[..8].try_into().unwrap()) % 100 < 50
        })
        .collect();
    assert_eq!(pinned, expected);
    // Roughly the requested share of a large sample.
    let inside = (0..2000)
        .filter(|_| canary(&push_key(Uuid::new_v4()), 30))
        .count();
    assert!((450..=750).contains(&inside), "{inside}");

    let Some(h) = Harness::new().await else {
        return;
    };
    let (in_key, out_key) = (key_with_canary(true), key_with_canary(false));
    for mode in ["LEGACY_ONLY", "SHADOW", "FENCED_OLD"] {
        set_mode(&h, mode, 100).await;
        let k = push_key(Uuid::new_v4());
        assert_eq!(
            route(&h, &k, MESSAGE_WAKEUPS).await.unwrap().route,
            Route::Old,
            "{mode}"
        );
    }
    set_mode(&h, "FENCED_NEW", 50).await;
    assert_eq!(
        route(&h, &in_key, MESSAGE_WAKEUPS).await.unwrap().route,
        Route::New
    );
    assert_eq!(
        route(&h, &out_key, MESSAGE_WAKEUPS).await.unwrap().route,
        Route::Old
    );
    // A retry that bypassed the ledger would compute the same route.
    assert!(canary(&in_key, 50) && !canary(&out_key, 50));

    // An audience too large for one envelope stays OLD inside the canary, with its reason.
    let large = key_with_canary(true);
    let txn = h.conn().begin().await.unwrap();
    let d = route_message(
        &txn,
        &large,
        MESSAGE_WAKEUPS,
        Utc::now() + Duration::hours(1),
        false,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(d.route, Route::Old);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_notification_route_decisions \
              WHERE idempotency_key = '{large}' AND route = 'OLD' AND reason = 'RECIPIENT_LIMIT'"
        ))
        .await,
        1
    );
}

/// F3: decisions change only through the audited reroute.
#[tokio::test]
async fn f3_decision_updates_are_guarded() {
    let Some(h) = Harness::new().await else {
        return;
    };
    set_mode(&h, "FENCED_NEW", 100).await;
    let key = push_key(Uuid::new_v4());
    assert_eq!(
        route(&h, &key, MESSAGE_WAKEUPS).await.unwrap().route,
        Route::New
    );
    let upd = |set: &str| {
        format!("UPDATE messaging_notification_route_decisions SET {set} WHERE idempotency_key = '{key}'")
    };
    for refused in [
        // Silent route change, a version skip, a non-reroute reason.
        "route = 'OLD'",
        "route = 'OLD', decision_version = 3, reason = 'REROUTED_UNACKED'",
        "route = 'OLD', decision_version = 2, reason = 'ROUTED'",
        "reason = 'AUTO_FAILOVER'",
        "decision_version = 2",
        // Identity columns.
        "category = 'OTHER'",
        "decision_id = gen_random_uuid()",
        "routing_version = 7",
        "decided_at = now() - interval '1 day'",
        "expires_at = now() + interval '30 days'",
        "idempotency_key = 'evt_other'",
        // Lease count grows by one.
        "lease_count = 2",
    ] {
        let r = h.sql(&upd(refused)).await;
        assert!(
            r.as_ref()
                .is_err_and(|e| e.contains("ROUTING_DECISION_IMMUTABLE")),
            "{refused}: {r:?}"
        );
    }
    // Allowed: acknowledgement, lease bookkeeping.
    h.sql(&upd("ns_acked_at = now()")).await.unwrap();
    h.sql(&upd(
        "send_lease_until = now() + interval '30 seconds', lease_count = lease_count + 1",
    ))
    .await
    .unwrap();
    // The acknowledgement is recorded once.
    assert!(h
        .sql(&upd("ns_acked_at = now() + interval '1 hour'"))
        .await
        .is_err());
    assert!(h.sql(&upd("ns_acked_at = NULL")).await.is_err());
    // The audited reroute NEW -> OLD.
    h.sql(&upd(
        "route = 'OLD', decision_version = decision_version + 1, reason = 'REROUTED_UNACKED'",
    ))
    .await
    .unwrap();
    // OLD is never rerouted back to NEW.
    assert!(h
        .sql(&upd(
            "route = 'NEW', decision_version = 3, reason = 'AUTO_FAILOVER'"
        ))
        .await
        .is_err());
    // A completed decision is terminal and never rerouted.
    let done = push_key(Uuid::new_v4());
    assert_eq!(
        route(&h, &done, MESSAGE_WAKEUPS).await.unwrap().route,
        Route::New
    );
    let upd_done = |set: &str| {
        format!("UPDATE messaging_notification_route_decisions SET {set} WHERE idempotency_key = '{done}'")
    };
    h.sql(&upd_done("ns_completed_at = now()")).await.unwrap();
    assert!(h
        .sql(&upd_done(
            "route = 'OLD', decision_version = 2, reason = 'REROUTED_POSSIBLE_DUPLICATE'"
        ))
        .await
        .is_err());
    assert!(h.sql(&upd_done("ns_completed_at = NULL")).await.is_err());
    let row = h
        .conn()
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT route, decision_version, reason FROM messaging_notification_route_decisions WHERE idempotency_key = '{key}'"
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            row.try_get::<String>("", "route").unwrap(),
            row.try_get::<i32>("", "decision_version").unwrap(),
            row.try_get::<String>("", "reason").unwrap()
        ),
        ("OLD".into(), 2, "REROUTED_UNACKED".into())
    );
}

// ------------------------------------------------------------------- M-11 (real path)

async fn register_push(h: &Harness, c: &Customer, m: &Member) {
    let r = h
        .put(
            c,
            Some(m.principal),
            &format!("/v1/messaging/devices/{}/push-registration", m.device_id()),
            json!({ "platform": "LOCAL_TEST", "handle": format!("fence-test-{}", m.principal) }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

/// The committed `messaging.message.created` event of the latest message, as the outbox
/// dispatcher would claim it.
async fn latest_message_event(h: &Harness) -> OutboxEvent {
    OutboxEvent::find_by_statement(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        "SELECT id, sequence, event_type, schema_version, aggregate_type, aggregate_id, tenant_id, \
                application_id, environment_id, correlation_id, causation_id, payload, occurred_at, \
                attempts, conversation_id, conversation_event_sequence \
           FROM messaging_outbox_events WHERE event_type = 'messaging.message.created' \
          ORDER BY sequence DESC LIMIT 1"
            .to_owned(),
    ))
    .one(h.conn())
    .await
    .unwrap()
    .unwrap()
}

async fn jobs_of(h: &Harness, event: &OutboxEvent) -> i64 {
    h.scalar(&format!(
        "SELECT count(*) FROM messaging_push_jobs WHERE event_id = '{}'",
        event.id
    ))
    .await
}

/// M-11: the legacy push path writes jobs without a decision only in LEGACY_ONLY (or
/// without a state row); otherwise the guard trigger requires an OLD decision for the
/// outbox event and refuses with ROUTING_DECISION_REQUIRED, writing nothing.
/// The real fan-out records the decision itself (N7b), in the transaction that writes
/// the jobs.
#[tokio::test]
async fn m11_push_jobs_require_an_old_decision_outside_legacy_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.allow_managed(&c, "GROUP").await;
    let mut people = Vec::new();
    for _ in 0..3 {
        let m = h.member(&c, 0).await;
        register_push(&h, &c, &m).await;
        people.push(m);
    }
    let r = h
        .create_conversation(
            &c,
            people[0].principal,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let conversation = r.json;
    for p in &people[1..] {
        let a = h
            .post(
                &c,
                Some(people[0].principal),
                &format!("/v1/conversations/{}/members", cid(&conversation)),
                json!({ "principalId": p.principal }),
            )
            .await;
        assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    }
    let alice = people[0].principal;
    let send = |text: &'static str| {
        let (h, c, conversation) = (&h, &c, &conversation);
        async move {
            let r = h.send_managed(c, conversation, alice, text).await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
            latest_message_event(h).await
        }
    };

    // LEGACY_ONLY: today's behaviour, no decision needed (two recipients' devices).
    let e1 = send("one").await;
    assert_eq!(push::fanout_jobs(&h.state, &e1).await.unwrap(), 2);
    assert_eq!(decisions(&h, &push_key(e1.id)).await, 0);
    // The trigger computes the same key as the outbox envelope id.
    assert_eq!(push_key(e1.id), e1.event_id());

    // FENCED_OLD: a writer without a decision (a pre-fence binary's raw insert) is
    // refused, and the database error itself names the rule; nothing is written.
    set_mode(&h, "FENCED_OLD", 0).await;
    let e2 = send("two").await;
    let copy = |event: Uuid| {
        format!(
            "INSERT INTO messaging_push_jobs (id, event_id, conversation_id, environment_id, device_id, registration_id, \
               payload, next_attempt_at, expires_at, created_at) \
             SELECT gen_random_uuid(), '{event}', conversation_id, environment_id, device_id, registration_id, \
                    payload, now(), now() + interval '1 hour', now() \
               FROM messaging_push_jobs WHERE event_id = '{}' LIMIT 1",
            e1.id
        )
    };
    for _ in 0..2 {
        let refused = h.sql(&copy(e2.id)).await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("ROUTING_DECISION_REQUIRED")),
            "{refused:?}"
        );
        assert_eq!(jobs_of(&h, &e2).await, 0);
    }
    // The real fan-out routes the event first (OLD, one decision for every device).
    assert_eq!(push::fanout_jobs(&h.state, &e2).await.unwrap(), 2);
    assert_eq!(decisions(&h, &push_key(e2.id)).await, 1);

    // A NEW decision does not license the legacy path: the fan-out writes no jobs and
    // a raw insert is refused.
    set_mode(&h, "FENCED_NEW", 100).await;
    let e3 = send("three").await;
    assert_eq!(push::fanout_jobs(&h.state, &e3).await.unwrap(), 0);
    let new = route(&h, &push_key(e3.id), MESSAGE_WAKEUPS).await.unwrap();
    assert_eq!((new.route, new.created), (Route::New, false));
    assert!(h.sql(&copy(e3.id)).await.is_err());
    assert_eq!(jobs_of(&h, &e3).await, 0);

    // An OLD decision for the event covers every device fan-out of it.
    set_mode(&h, "FENCED_NEW", 0).await;
    let e4 = send("four").await;
    let old = route(&h, &push_key(e4.id), MESSAGE_WAKEUPS).await.unwrap();
    assert_eq!(old.route, Route::Old);
    assert_eq!(push::fanout_jobs(&h.state, &e4).await.unwrap(), 2);
    // Idempotent re-fan-out: nothing twice, the decision is followed.
    assert_eq!(push::fanout_jobs(&h.state, &e4).await.unwrap(), 0);
    assert_eq!(jobs_of(&h, &e4).await, 2);
    assert_eq!(decisions(&h, &push_key(e4.id)).await, 1);

    // SHADOW is not LEGACY_ONLY either: raw inserts are refused.
    set_mode(&h, "SHADOW", 0).await;
    let e5 = send("five").await;
    assert!(h.sql(&copy(e5.id)).await.is_err());
    assert_eq!(jobs_of(&h, &e5).await, 0);

    // Back to LEGACY_ONLY: allowed again without a decision.
    set_mode(&h, "LEGACY_ONLY", 0).await;
    assert_eq!(push::fanout_jobs(&h.state, &e5).await.unwrap(), 2);
    assert_eq!(decisions(&h, &push_key(e5.id)).await, 0);

    // No state row for the category: pre-fence behaviour (allowed).
    h.sql("DELETE FROM messaging_notification_routing_state WHERE category = 'MESSAGE_WAKEUPS'")
        .await
        .unwrap();
    let e6 = send("six").await;
    assert_eq!(push::fanout_jobs(&h.state, &e6).await.unwrap(), 2);
    assert_eq!(decisions(&h, &push_key(e6.id)).await, 0);
}

/// Readiness reports `routingProtocol: 1` once the fence history is applied, and omits
/// it otherwise (pre-fence databases and binaries look the same to certify).
#[tokio::test]
async fn readiness_reports_the_routing_protocol_only_when_applied() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = ready(&h).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("messaging/common/v1.schema.json#/$defs/Readiness", &r.json);
    assert_eq!(r.json["routingProtocol"], 1);
    assert_eq!(fence_protocol(h.conn()).await, Some(1));

    // Main history only (plus the gap-closure history this image requires): ready as
    // before, no routingProtocol.
    let db = Arc::new(TestDb::empty().await.unwrap());
    migration::Migrator::up(&db.conn, None).await.unwrap();
    migration::GapClosureMigrator::up(&db.conn, None)
        .await
        .unwrap();
    migration::MlsJoinMigrator::up(&db.conn, None)
        .await
        .unwrap();
    migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    migration::ChatControlsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    migration::AdminOperationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    let plain = Harness::on(db.clone(), |_| {});
    let r = ready(&plain).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("messaging/common/v1.schema.json#/$defs/Readiness", &r.json);
    assert!(r.json.get("routingProtocol").is_none(), "{}", r.text);
    assert_eq!(fence_protocol(&db.conn).await, None);
    // The guard trigger is absent there: the legacy path is untouched.
    assert_eq!(
        plain
            .scalar("SELECT count(*) FROM pg_trigger WHERE tgname = 'trg_messaging_push_jobs_routing_guard'")
            .await,
        0
    );
    // Applying the fence history (the separate binary) turns it on.
    migration::NotificationFenceMigrator::up(&db.conn, None)
        .await
        .unwrap();
    let r = ready(&plain).await;
    assert_eq!(r.json["routingProtocol"], 1, "{}", r.text);

    // A fence history this binary does not know (a newer release): not reported.
    db.conn
        .execute_unprepared(&format!(
            "INSERT INTO {} (version, applied_at) VALUES ('m29991231_000001_future', 0)",
            migration::NOTIFICATION_FENCE_MIGRATION_TABLE
        ))
        .await
        .unwrap();
    let r = ready(&plain).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json.get("routingProtocol").is_none(), "{}", r.text);
    // ... and the main migrator is unaffected by the separate history.
    assert!(migration::pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
}
