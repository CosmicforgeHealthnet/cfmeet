//! Migration / backfill / compatibility tests with realistic legacy fixtures
//! (spec §B15, §B16, §11 "MIGRATION", §13 legacy compatibility).

mod common;

use axum::http::StatusCode;
use common::*;
use migration::{phase2_backfill, Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

/// Number of pre-Phase-2 (legacy) migrations.
const LEGACY_MIGRATIONS: u32 = 14;

struct Fixture {
    host: Uuid,
    alice: Uuid,
    bob: Uuid,
    scheduled: Uuid,
    ongoing: Uuid,
    ended: Uuid,
    cancelled: Uuid,
    private: Uuid,
}

async fn exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_unprepared(sql)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
}

async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
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

/// Realistic legacy data: every meeting status, private/public, a registered user who
/// rejoined (3 rows), guests sharing a display name, waiting and kicked participants,
/// and session logs.
async fn seed_legacy(conn: &DatabaseConnection) -> Fixture {
    let f = Fixture {
        host: Uuid::new_v4(),
        alice: Uuid::new_v4(),
        bob: Uuid::new_v4(),
        scheduled: Uuid::new_v4(),
        ongoing: Uuid::new_v4(),
        ended: Uuid::new_v4(),
        cancelled: Uuid::new_v4(),
        private: Uuid::new_v4(),
    };
    for (id, name) in [(f.host, "host"), (f.alice, "alice"), (f.bob, "bob")] {
        exec(conn, &format!("INSERT INTO users (id, username, email, status) VALUES ('{id}', '{name}', '{name}@example.com', 'active')")).await;
    }
    let meetings = [
        (
            f.scheduled,
            "SCHD2345",
            "scheduled",
            false,
            "now() + interval '1 day'",
        ),
        (
            f.ongoing,
            "LIVE2345",
            "ongoing",
            false,
            "now() - interval '30 minutes'",
        ),
        (
            f.ended,
            "ENDD2345",
            "ended",
            false,
            "now() - interval '2 days'",
        ),
        (
            f.cancelled,
            "CNCL2345",
            "cancelled",
            false,
            "now() + interval '2 days'",
        ),
        (
            f.private,
            "PRIV2345",
            "ongoing",
            true,
            "now() - interval '10 minutes'",
        ),
    ];
    for (id, code, status, private, start) in meetings {
        exec(
            conn,
            &format!(
                "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
                 VALUES ('{id}', '{code}', '{h}', '{h}', 'Meeting {code}', {private}, ({start})::timestamp, ({start})::timestamp + interval '1 hour', '{status}')",
                h = f.host
            ),
        )
        .await;
    }
    let ended = f.ended;
    exec(conn, &format!("INSERT INTO session_logs (meeting_id, event_type, event_time) VALUES ('{ended}', 'meeting_start', now() - interval '2 days'), ('{ended}', 'meeting_end', now() - interval '2 days' + interval '50 minutes')")).await;
    // Ended meeting: host + alice rejoined 3 times + two guests named "Guest" + kicked bob.
    let rows = [
        (
            Some(f.host),
            "host",
            "host",
            "joined",
            "2 days",
            Some("2 days' - interval '-10 minutes"),
        ),
        (
            Some(f.alice),
            "participant",
            "Alice",
            "joined",
            "2 days",
            Some("2 days' - interval '-40 minutes"),
        ),
        (
            Some(f.alice),
            "participant",
            "Alice",
            "joined",
            "2 days' - interval '-45 minutes",
            Some("2 days' - interval '-46 minutes"),
        ),
        (
            Some(f.alice),
            "participant",
            "Alice (phone)",
            "joined",
            "2 days' - interval '-47 minutes",
            None,
        ),
        (
            None,
            "participant",
            "Guest",
            "joined",
            "2 days",
            Some("2 days' - interval '-20 minutes"),
        ),
        (None, "participant", "Guest", "waiting", "2 days", None),
        (
            Some(f.bob),
            "participant",
            "Bob",
            "kicked",
            "2 days",
            Some("2 days' - interval '-5 minutes"),
        ),
    ];
    for (user, role, name, status, join_ago, leave_ago) in rows {
        let user_sql = user.map(|u| format!("'{u}'")).unwrap_or("NULL".into());
        let leave_sql = leave_ago
            .map(|l| format!("now() - interval '{l}'"))
            .unwrap_or("NULL".into());
        exec(
            conn,
            &format!(
                "INSERT INTO participants (meeting_id, user_id, role, join_time, leave_time, status, display_name) \
                 VALUES ('{ended}', {user_sql}, '{role}', now() - interval '{join_ago}', {leave_sql}, '{status}', '{name}')"
            ),
        )
        .await;
    }
    // Ongoing private meeting: a waiting registered user.
    exec(conn, &format!("INSERT INTO participants (meeting_id, user_id, role, status, display_name) VALUES ('{}', '{}', 'participant', 'waiting', 'Bob')", f.private, f.bob)).await;
    f
}

async fn legacy_db() -> Option<(TestDb, Fixture)> {
    let db = TestDb::empty().await?;
    Migrator::up(&db.conn, Some(LEGACY_MIGRATIONS))
        .await
        .unwrap();
    let f = seed_legacy(&db.conn).await;
    Some((db, f))
}

/// Additive schema + backfill maps every legacy row deterministically and reruns are no-ops.
#[tokio::test]
async fn m01_backfill_maps_legacy_and_is_idempotent() {
    let Some((db, f)) = legacy_db().await else {
        return;
    };
    let c = &db.conn;
    let legacy_meetings_before = count(c, "SELECT count(*) FROM meetings").await;
    let legacy_participants_before = count(c, "SELECT count(*) FROM participants").await;

    Migrator::up(c, None).await.unwrap(); // schema + backfill migration

    let txn = c.begin().await.unwrap();
    let report = phase2_backfill::evidence(&txn, None).await.unwrap();
    txn.commit().await.unwrap();
    println!("migration evidence: {report:#?}");
    assert_eq!(report.legacy_meeting_rows, 5);
    assert_eq!(report.mapped_spaces, 5);
    assert_eq!(report.mapping_failures, 0);
    assert_eq!(report.legacy_conference_sessions, 5);
    assert_eq!(report.legacy_participant_rows, 8);
    assert_eq!(report.mapped_legacy_participant_rows, 8);
    assert!(report.unmapped_participant_ids.is_empty());
    // host, alice (3 rows → 1), guest, guest, bob (ended) + bob (private) = 6 logical participants
    assert_eq!(report.conference_participant_rows_from_legacy, 6);
    // Sessions only for rows that ever connected: host, alice×3, guest(joined), bob(kicked) = 6
    assert_eq!(report.participant_session_rows_from_legacy, 6);
    assert!(report.is_clean());

    // Deterministic translation.
    assert_eq!(count(c, &format!("SELECT count(*) FROM meeting_spaces WHERE legacy_meeting_id='{}' AND meeting_code='ENDD2345' AND owner_user_id='{}' AND access_policy='PUBLIC' AND waiting_room_policy='OFF' AND origin='LEGACY'", f.ended, f.host)).await, 1);
    assert_eq!(count(c, &format!("SELECT count(*) FROM meeting_spaces WHERE legacy_meeting_id='{}' AND access_policy='AUTHENTICATED' AND waiting_room_policy='EVERYONE'", f.private)).await, 1);
    for (id, status) in [
        (f.scheduled, "CREATED"),
        (f.ongoing, "LIVE"),
        (f.ended, "ENDED"),
        (f.cancelled, "CANCELLED"),
    ] {
        assert_eq!(count(c, &format!("SELECT count(*) FROM conference_sessions WHERE legacy_meeting_id='{id}' AND status='{status}'")).await, 1, "{status}");
    }
    assert_eq!(count(c, &format!("SELECT count(*) FROM conference_sessions WHERE legacy_meeting_id='{}' AND started_at IS NOT NULL AND ended_at IS NOT NULL", f.ended)).await, 1, "times from session logs");
    assert_eq!(count(c, &format!("SELECT count(*) FROM conference_sessions WHERE legacy_meeting_id='{}' AND started_at IS NULL", f.ongoing)).await, 1, "no fabricated history");
    assert_eq!(count(c, &format!("SELECT count(*) FROM conference_participants cp JOIN conference_sessions cs ON cs.id = cp.conference_id WHERE cs.legacy_meeting_id='{}' AND cp.user_id='{}' AND cp.role='OWNER'", f.ended, f.host)).await, 1);
    assert_eq!(
        count(
            c,
            &format!(
                "SELECT count(*) FROM conference_participants WHERE user_id='{}'",
                f.alice
            )
        )
        .await,
        1,
        "multi-row legacy user collapses to one logical participant"
    );
    assert_eq!(count(c, &format!("SELECT count(*) FROM participant_sessions ps JOIN conference_participants cp ON cp.id = ps.conference_participant_id WHERE cp.user_id='{}'", f.alice)).await, 3);
    assert_eq!(count(c, "SELECT count(DISTINCT guest_identity) FROM conference_participants WHERE identity_type='GUEST' AND display_name='Guest'").await, 2, "guests never merged by name");
    assert_eq!(count(c, &format!("SELECT count(*) FROM conference_participants WHERE user_id='{}' AND admission_state='REMOVED'", f.bob)).await, 1);
    assert_eq!(count(c, &format!("SELECT count(*) FROM conference_participants WHERE user_id='{}' AND admission_state='WAITING'", f.bob)).await, 1);
    assert_eq!(
        count(
            c,
            "SELECT count(*) FROM participant_sessions WHERE state <> 'HISTORICAL'"
        )
        .await,
        0
    );

    // Rerun: nothing new.
    let txn = c.begin().await.unwrap();
    let rerun = phase2_backfill::run(&txn, None).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        (
            rerun.inserted_spaces,
            rerun.inserted_conferences,
            rerun.inserted_participants,
            rerun.inserted_sessions
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(rerun.refreshed_rows, 0);
    assert_eq!(rerun.conference_participant_rows_from_legacy, 6);

    // Legacy tables untouched.
    assert_eq!(
        count(c, "SELECT count(*) FROM meetings").await,
        legacy_meetings_before
    );
    assert_eq!(
        count(c, "SELECT count(*) FROM participants").await,
        legacy_participants_before
    );
}

/// Rollback compatibility: Phase 2 down-migrations remove only v2 objects; legacy data
/// is intact and a re-apply backfills identically.
#[tokio::test]
async fn m02_rollback_and_reapply() {
    let Some((db, _f)) = legacy_db().await else {
        return;
    };
    let c = &db.conn;
    Migrator::up(c, None).await.unwrap();
    let spaces = count(c, "SELECT count(*) FROM meeting_spaces").await;
    // Parts G–I down-migration alone first: Part F and the v2 core stay.
    Migrator::down(c, Some(1)).await.unwrap();
    assert_eq!(count(c, "SELECT count(*) FROM information_schema.tables WHERE table_name IN ('transcription_jobs','transcript_segments','transcript_translations','webinar_registrations')").await, 0);
    assert_eq!(count(c, "SELECT count(*) FROM information_schema.columns WHERE table_name='meeting_artifacts' AND column_name IN ('status','storage_key','retention_at')").await, 0);
    assert_eq!(count(c, "SELECT count(*) FROM information_schema.columns WHERE table_name='rtc_drift_events' AND column_name IN ('status','dedupe_key')").await, 0);
    assert_eq!(count(c, "SELECT count(*) FROM information_schema.tables WHERE table_name IN ('conference_messages','meeting_poll_ballots')").await, 2);
    // Then the Part F down-migration: v2 core stays, collaboration objects go.
    Migrator::down(c, Some(1)).await.unwrap();
    assert_eq!(count(c, "SELECT count(*) FROM information_schema.tables WHERE table_name IN ('conference_messages','meeting_poll_ballots')").await, 0);
    assert_eq!(count(c, "SELECT count(*) FROM information_schema.columns WHERE table_name='conference_participants' AND column_name='hand_raised_at'").await, 0);
    assert_eq!(
        count(c, "SELECT count(*) FROM meeting_spaces").await,
        spaces
    );
    Migrator::up(c, None).await.unwrap();
    // All seven Phase 2 migrations (schema, backfill, organizations, recurrence
    // exceptions, Part E rtc control plane, Part F collaboration, Parts G–I).
    Migrator::down(c, Some(7)).await.unwrap();
    assert_eq!(count(c, "SELECT count(*) FROM information_schema.tables WHERE table_name IN ('meeting_spaces','conference_sessions','participant_sessions','outbox_events','organizations','meeting_recurrence_exceptions','rtc_webhook_events','rtc_drift_events','worker_leases','conference_messages','meeting_poll_ballots','transcription_jobs','webinar_registrations')").await, 0);
    assert_eq!(count(c, "SELECT count(*) FROM meetings").await, 5);
    assert_eq!(count(c, "SELECT count(*) FROM participants").await, 8);
    Migrator::up(c, None).await.unwrap();
    assert_eq!(
        count(c, "SELECT count(*) FROM meeting_spaces").await,
        spaces
    );
}

/// Legacy-origin rows follow the legacy source until certification; v2 never mutates them.
#[tokio::test]
async fn m03_legacy_conferences_are_legacy_managed() {
    let Some((db, f)) = legacy_db().await else {
        return;
    };
    Migrator::up(&db.conn, None).await.unwrap();
    let app = TestApp::on(db, TestOptions::default());
    let c = &app.db.conn;
    exec(
        c,
        &format!(
            "UPDATE meetings SET status='ended', title='Renamed in v1' WHERE id='{}'",
            f.ongoing
        ),
    )
    .await;
    let (_, diffs) = app
        .state
        .meeting
        .compat
        .shadow_compare(f.ongoing)
        .await
        .unwrap();
    assert!(
        diffs.iter().any(|d| d.contains("conference status")),
        "shadow read detects drift: {diffs:?}"
    );
    assert!(diffs.iter().any(|d| d.contains("title")));
    let report = app.state.meeting.compat.backfill(None).await.unwrap();
    assert!(report.refreshed_rows >= 2);
    let (_, diffs) = app
        .state
        .meeting
        .compat
        .shadow_compare(f.ongoing)
        .await
        .unwrap();
    assert!(diffs.is_empty(), "{diffs:?}");
    // v2 must not mutate a legacy-managed conference.
    let host_token = app
        .state
        .jwt_service
        .generate_access_token(f.host, "user")
        .unwrap();
    let row = c
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT id FROM conference_sessions WHERE legacy_meeting_id='{}'",
                f.private
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    let legacy_conf: Uuid = row.try_get_by_index(0).unwrap();
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{legacy_conf}/end"),
            Auth::Bearer(&host_token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT")
    );
    let (s, v) = app
        .join(legacy_conf, Auth::Bearer(&host_token), json!({}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT")
    );
    // Old links keep resolving to canonical objects.
    let (s, v) = app.get("/api/v2/resolve/PRIV2345", Auth::None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(uuid(&v["conference"]["id"]), legacy_conf);
    let (_, v) = app
        .get(&format!("/api/v2/resolve/{}", f.private), Auth::None)
        .await;
    assert_eq!(v["matchedBy"], "LEGACY_ID");
}

/// A deleted legacy meeting stops resolving and its legacy-origin space is soft-deleted.
#[tokio::test]
async fn m04_legacy_delete_is_reflected() {
    let Some((db, f)) = legacy_db().await else {
        return;
    };
    Migrator::up(&db.conn, None).await.unwrap();
    let app = TestApp::on(db, TestOptions::default());
    // Legacy delete only removes scheduled meetings with no children.
    exec(
        &app.db.conn,
        &format!("DELETE FROM meetings WHERE id='{}'", f.scheduled),
    )
    .await;
    let (s, v) = app.get("/api/v2/resolve/SCHD2345", Auth::None).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "MEETING_NOT_FOUND"));
    app.state.meeting.compat.backfill(None).await.unwrap();
    assert_eq!(count(&app.db.conn, "SELECT count(*) FROM meeting_spaces WHERE meeting_code='SCHD2345' AND status='DELETED'").await, 1);
}

/// v2 can run alongside legacy: a new v2 conference in a legacy-mapped space gets its own
/// provider room and never collides with the legacy room name.
#[tokio::test]
async fn m05_v2_conference_in_legacy_space() {
    let Some((db, f)) = legacy_db().await else {
        return;
    };
    Migrator::up(&db.conn, None).await.unwrap();
    let app = TestApp::on(db, TestOptions::default());
    let host_token = app
        .state
        .jwt_service
        .generate_access_token(f.host, "user")
        .unwrap();
    let (_, r) = app.get("/api/v2/resolve/SCHD2345", Auth::None).await;
    let sid = uuid(&r["meetingSpace"]["id"]);
    let (s, c) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&host_token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let cid = uuid(&c["id"]);
    assert_eq!(count(&app.db.conn, &format!("SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND rtc_room_name LIKE 'cf-%' AND rtc_room_name <> 'SCHD2345'")).await, 1);
}

/// Legacy v1 admit/deny are now scoped to the meeting in the path (IDOR fix).
#[tokio::test]
async fn m06_legacy_admit_is_meeting_scoped() {
    let Some(redis) = std::env::var("TEST_REDIS_URL")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
            eprintln!("SKIPPED: legacy v1 auth requires Redis (TEST_REDIS_URL unset)");
            return;
        }
        panic!("TEST_REDIS_URL is required for legacy v1 endpoint tests (legacy auth middleware uses Redis)");
    };
    let _ = redis;
    let Some((db, f)) = legacy_db().await else {
        return;
    };
    Migrator::up(&db.conn, None).await.unwrap();
    let app = TestApp::on(db, TestOptions::default());
    let other_host = app.user("other-host").await;
    let other_meeting = Uuid::new_v4();
    exec(&app.db.conn, &format!("INSERT INTO meetings (id, meeting_identifier, host_id, title, is_private, start_time, status) VALUES ('{other_meeting}', 'OTHR2345', '{}', 'o', true, now(), 'ongoing')", other_host.id)).await;
    let row = app
        .db
        .conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT id FROM participants WHERE meeting_id='{}' AND status='waiting'",
                f.private
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    let victim: Uuid = row.try_get_by_index(0).unwrap();
    let (s, _) = app
        .post(
            &format!("/api/v1/meetings/{other_meeting}/waiting/{victim}/admit"),
            Auth::Bearer(&other_host.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        count(
            &app.db.conn,
            &format!("SELECT count(*) FROM participants WHERE id='{victim}' AND status='waiting'")
        )
        .await,
        1
    );
}

/// Legacy v1 writes keep working after Phase 2 (no legacy write retirement), and new
/// legacy meetings resolve through the v2 compatibility resolver.
#[tokio::test]
async fn m07_legacy_v1_writes_still_work() {
    if std::env::var("TEST_REDIS_URL")
        .map(|v| v.is_empty())
        .unwrap_or(true)
    {
        if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
            eprintln!("SKIPPED: legacy v1 auth requires Redis (TEST_REDIS_URL unset)");
            return;
        }
        panic!("TEST_REDIS_URL is required for legacy v1 endpoint tests (legacy auth middleware uses Redis)");
    }
    let Some(app) = TestApp::new().await else {
        return;
    };
    let host = app.user("v1host").await;
    let (s, v) = app
        .post(
            "/api/v1/meetings/instant",
            Auth::Bearer(&host.token),
            json!({ "title": "v1 instant" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let code_str = v["data"]["meeting_identifier"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        v["data"]["join_token"].as_str().unwrap().len() > 20,
        "legacy LiveKit token unchanged"
    );
    let (s, r) = app
        .get(
            &format!("/api/v2/resolve/{code_str}"),
            Auth::Bearer(&host.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["matchedBy"], "LEGACY_CODE");
    assert_eq!(r["meetingSpace"]["view"], "FULL");
    assert_eq!(r["conference"]["status"], "LIVE");
    assert_eq!(r["conference"]["isLegacy"], true);
}
