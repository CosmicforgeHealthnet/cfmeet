//! Part M §27 / §28 — migration certification on a production-like SYNTHETIC legacy
//! dataset (no production or clinical data). Generates `MIGRATION_CERT_MEETINGS`
//! legacy meetings (default 3000) with the status / privacy / instant / scheduled
//! mix, registered + guest participants, rejoins, waiting and kicked rows, session
//! logs and deliberately inconsistent records, then certifies:
//! partial run → interrupted → concurrent resume (migration + 2 extra runners),
//! row mappings, code/link preservation, status mapping, idempotent rerun, legacy
//! reads, compatibility resolver and shadow comparison. Emits the formal migration
//! report (`MIGRATION_REPORT {...}`; also `$CFMEET_CERT_OUT/migration-report.json`).
//!
//! The legacy schema has no passcode, recurrence, organization or membership
//! representation: those dataset categories are reported as NOT APPLICABLE rather
//! than synthesised.

mod common;

use std::time::Instant;

use axum::http::StatusCode;
use common::*;
use migration::{phase2_backfill, Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

/// Number of pre-Phase-2 (legacy) migrations.
const LEGACY_MIGRATIONS: u32 = 14;

async fn exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_unprepared(sql)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
}

async fn n(conn: &DatabaseConnection, sql: &str) -> i64 {
    conn.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap_or_else(|e| panic!("{e}\n{sql}"))
    .unwrap()
    .try_get_by_index::<i64>(0)
    .unwrap()
}

async fn rows(conn: &DatabaseConnection, sql: &str) -> Vec<(String, String, i64)> {
    conn.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .iter()
    .map(|r| {
        (
            r.try_get_by_index::<String>(0).unwrap(),
            r.try_get_by_index::<String>(1).unwrap(),
            r.try_get_by_index::<i64>(2).unwrap(),
        )
    })
    .collect()
}

async fn uuids(conn: &DatabaseConnection, sql: &str) -> Vec<Uuid> {
    conn.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .iter()
    .map(|r| r.try_get_by_index::<Uuid>(0).unwrap())
    .collect()
}

/// Deterministic synthetic legacy dataset (pure SQL, so it scales).
async fn seed(conn: &DatabaseConnection, meetings: i64) {
    let users = (meetings / 5).max(50);
    exec(
        conn,
        &format!(
            "INSERT INTO users (id, username, email, status) \
         SELECT md5('user' || i)::uuid, 'user' || i, 'user' || i || '@synthetic.invalid', \
                CASE WHEN i % 50 = 0 THEN 'inactive'::user_status ELSE 'active'::user_status END \
           FROM generate_series(1, {users}) i"
        ),
    )
    .await;
    // Status mix: 20% scheduled, 5% ongoing, 60% ended, 15% cancelled; 30% private;
    // 40% instant (start = creation, no end); 1/97 inconsistent end < start.
    exec(conn, &format!(
        "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, metadata, is_private, start_time, end_time, status, created_at, updated_at) \
         SELECT md5('meeting' || i)::uuid, 'M' || lpad(i::text, 7, '0'), \
                md5('user' || (1 + i % {hosts}))::uuid, md5('user' || (1 + i % {hosts}))::uuid, \
                'Synthetic meeting ' || i, \
                CASE WHEN i % 4 = 0 THEN json_build_object('source', 'synthetic', 'n', i) END, \
                (i % 10 < 3), \
                created + CASE WHEN i % 5 < 2 THEN interval '0' ELSE interval '1 day' END, \
                CASE WHEN i % 97 = 0 THEN created - interval '1 hour' \
                     WHEN i % 5 < 2 THEN NULL \
                     ELSE created + interval '1 day 1 hour' END, \
                (CASE WHEN i % 20 < 4 THEN 'scheduled' WHEN i % 20 = 4 THEN 'ongoing' \
                      WHEN i % 20 < 17 THEN 'ended' ELSE 'cancelled' END)::meeting_status, \
                created, created \
           FROM (SELECT i, (now() - (i % 400) * interval '1 day')::timestamp AS created FROM generate_series(1, {meetings}) i) g",
        hosts = users / 3
    )).await;
    // Participants for ongoing / ended meetings: 2–9 rows each, every third a guest,
    // host row first, rejoins (duplicate user rows), kicked rows, waiting rows on
    // ongoing meetings, guests sharing a display name.
    exec(conn, &format!(
        "INSERT INTO participants (meeting_id, user_id, role, join_time, leave_time, status, display_name) \
         SELECT m.id, \
                CASE WHEN k = 1 THEN m.host_id WHEN k % 3 = 0 THEN NULL \
                     ELSE md5('user' || (1 + (i * 7 + k) % {users}))::uuid END, \
                (CASE WHEN k = 1 THEN 'host' WHEN k % 9 = 0 THEN 'viewer' ELSE 'participant' END)::participant_role, \
                m.start_time + k * interval '1 minute', \
                CASE WHEN m.status::text = 'ended' THEN m.start_time + (k + 30) * interval '1 minute' END, \
                (CASE WHEN m.status::text = 'ongoing' AND k % 4 = 0 THEN 'waiting' WHEN m.status::text = 'ended' AND k % 5 = 4 THEN 'kicked' ELSE 'joined' END)::participant_status, \
                CASE WHEN k % 3 = 0 THEN 'Guest' ELSE 'Person ' || k END \
           FROM (SELECT md5('meeting' || i)::uuid AS mid, i FROM generate_series(1, {meetings}) i) s \
           JOIN meetings m ON m.id = s.mid \
          CROSS JOIN LATERAL generate_series(1, 2 + (s.i % 8)) k \
          WHERE m.status::text IN ('ongoing', 'ended')"
    )).await;
    // Rejoins: a second row for 1 in 7 registered non-host participants.
    exec(conn,
        "INSERT INTO participants (meeting_id, user_id, role, join_time, leave_time, status, display_name) \
         SELECT meeting_id, user_id, role, join_time + interval '5 minutes', leave_time, status, display_name || ' (rejoin)' \
           FROM participants WHERE user_id IS NOT NULL AND role::text <> 'host' AND status::text <> 'waiting' \
            AND ('x' || substr(md5(id::text), 1, 2))::bit(8)::int % 7 = 0",
    ).await;
    // Session logs for ended meetings; 1 in 13 has none (repairable: unknown start).
    exec(conn, &format!(
        "INSERT INTO session_logs (meeting_id, event_type, event_time) \
         SELECT m.id, e.t::event_type, CASE e.t WHEN 'meeting_start' THEN m.start_time ELSE m.start_time + interval '50 minutes' END \
           FROM generate_series(1, {meetings}) i JOIN meetings m ON m.id = md5('meeting' || i)::uuid \
          CROSS JOIN (VALUES ('meeting_start'), ('meeting_end')) e(t) \
          WHERE m.status::text = 'ended' AND i % 13 <> 0"
    )).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_certification_synthetic_dataset() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let meetings: i64 = std::env::var("MIGRATION_CERT_MEETINGS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let c = &db.conn;
    Migrator::up(c, Some(LEGACY_MIGRATIONS)).await.unwrap();
    let t = Instant::now();
    seed(c, meetings).await;
    let seed_ms = t.elapsed().as_millis();
    let input = json!({
        "users": n(c, "SELECT count(*) FROM users").await,
        "meetings": n(c, "SELECT count(*) FROM meetings").await,
        "meetingsByStatus": rows(c, "SELECT status::text, '', count(*) FROM meetings GROUP BY 1 ORDER BY 1").await.into_iter().map(|(s, _, k)| (s, k)).collect::<std::collections::BTreeMap<_, _>>(),
        "privateMeetings": n(c, "SELECT count(*) FROM meetings WHERE is_private").await,
        "publicMeetings": n(c, "SELECT count(*) FROM meetings WHERE NOT is_private").await,
        "instantMeetings": n(c, "SELECT count(*) FROM meetings WHERE end_time IS NULL").await,
        "scheduledWindowMeetings": n(c, "SELECT count(*) FROM meetings WHERE end_time IS NOT NULL").await,
        "participants": n(c, "SELECT count(*) FROM participants").await,
        "registeredParticipantRows": n(c, "SELECT count(*) FROM participants WHERE user_id IS NOT NULL").await,
        "guestParticipantRows": n(c, "SELECT count(*) FROM participants WHERE user_id IS NULL").await,
        "rejoinRows": n(c, "SELECT count(*) FROM participants WHERE display_name LIKE '% (rejoin)'").await,
        "waitingRows": n(c, "SELECT count(*) FROM participants WHERE status::text = 'waiting'").await,
        "kickedRows": n(c, "SELECT count(*) FROM participants WHERE status::text = 'kicked'").await,
        "sessionLogs": n(c, "SELECT count(*) FROM session_logs").await,
    });
    println!("MIGRATION input {input}");

    // 1. Schema only, then a PARTIAL backfill (first quarter of meetings, one by one).
    Migrator::up(c, Some(1)).await.unwrap(); // pending step: Phase 2 schema only
    let all_ids = uuids(c, "SELECT id FROM meetings ORDER BY meeting_identifier").await;
    let partial: Vec<Uuid> = all_ids.iter().take(all_ids.len() / 4).copied().collect();
    let t = Instant::now();
    for id in &partial {
        let txn = c.begin().await.unwrap();
        phase2_backfill::run(&txn, Some(*id)).await.unwrap();
        txn.commit().await.unwrap();
    }
    let partial_ms = t.elapsed().as_millis();
    let partial_spaces = n(c, "SELECT count(*) FROM meeting_spaces").await;
    assert_eq!(
        partial_spaces as usize,
        partial.len(),
        "partial run persisted"
    );
    // 2. Interruption: half of the partially migrated meetings lose their sessions and
    //    participants (a crash mid-way leaves inconsistent progress).
    let interrupted: Vec<String> = partial
        .iter()
        .step_by(2)
        .map(|u| format!("'{u}'"))
        .collect();
    let list = interrupted.join(",");
    exec(c, &format!("DELETE FROM participant_sessions WHERE conference_id IN (SELECT id FROM conference_sessions WHERE legacy_meeting_id IN ({list}))")).await;
    exec(c, &format!("DELETE FROM conference_participants WHERE conference_id IN (SELECT id FROM conference_sessions WHERE legacy_meeting_id IN ({list}))")).await;
    // 3. Concurrent resume: the real migration (full backfill + later schema) races two
    //    extra full runners; the advisory lock serialises them.
    let t = Instant::now();
    let mut handles = vec![];
    for _ in 0..2 {
        let url = db.url.clone();
        handles.push(tokio::spawn(async move {
            let conn = sea_orm::Database::connect(url).await.unwrap();
            let txn = conn.begin().await.unwrap();
            let r = phase2_backfill::run(&txn, None).await.unwrap();
            txn.commit().await.unwrap();
            r
        }));
    }
    Migrator::up(c, None).await.unwrap();
    let mut concurrent = vec![];
    for h in handles {
        concurrent.push(h.await.unwrap());
    }
    let resume_ms = t.elapsed().as_millis();

    // 4. Certify mappings.
    let ev = phase2_backfill::evidence(c, None).await.unwrap();
    assert!(ev.is_clean(), "{ev:#?}");
    assert_eq!(ev.mapping_failures, 0);
    assert_eq!(ev.legacy_meeting_rows, meetings);
    let dup_spaces = n(c, "SELECT count(*) - count(DISTINCT legacy_meeting_id) FROM meeting_spaces WHERE legacy_meeting_id IS NOT NULL").await;
    let dup_conferences = n(c, "SELECT count(*) - count(DISTINCT legacy_meeting_id) FROM conference_sessions WHERE origin = 'LEGACY'").await;
    let dup_sessions = n(c, "SELECT count(*) - count(DISTINCT legacy_participant_id) FROM participant_sessions WHERE legacy_participant_id IS NOT NULL").await;
    assert_eq!(
        (dup_spaces, dup_conferences, dup_sessions),
        (0, 0, 0),
        "duplicate prevention"
    );
    let codes_preserved = n(c, "SELECT count(*) FROM meetings m JOIN meeting_spaces s ON s.legacy_meeting_id = m.id WHERE s.meeting_code = m.meeting_identifier").await;
    assert_eq!(
        codes_preserved, meetings,
        "every legacy code / link preserved"
    );
    let owners_mapped = n(c, "SELECT count(*) FROM meetings m JOIN meeting_spaces s ON s.legacy_meeting_id = m.id WHERE s.owner_user_id = m.host_id").await;
    assert_eq!(owners_mapped, meetings, "ownership mapped");
    let access_mapped = n(c, "SELECT count(*) FROM meetings m JOIN meeting_spaces s ON s.legacy_meeting_id = m.id WHERE s.access_policy = CASE WHEN m.is_private THEN 'AUTHENTICATED' ELSE 'PUBLIC' END").await;
    assert_eq!(access_mapped, meetings, "privacy mapped");
    let status_map = rows(c, "SELECT m.status::text, c.status, count(*) FROM meetings m JOIN conference_sessions c ON c.legacy_meeting_id = m.id AND c.origin = 'LEGACY' GROUP BY 1, 2 ORDER BY 1, 2").await;
    for (legacy, v2, _) in &status_map {
        let expected = match legacy.as_str() {
            "scheduled" => "CREATED",
            "ongoing" => "LIVE",
            "ended" => "ENDED",
            _ => "CANCELLED",
        };
        assert_eq!(v2, expected, "status mapping {legacy}");
    }
    // Role / admission mapping: the latest legacy row of each registered user decides
    // (documented collapse rule); every logical participant is checked.
    let role_sql = "FROM (SELECT DISTINCT ON (lp.meeting_id, lp.user_id) lp.* FROM participants lp WHERE lp.user_id IS NOT NULL \
                     ORDER BY lp.meeting_id, lp.user_id, lp.join_time DESC) p \
                    JOIN meetings m ON m.id = p.meeting_id \
                    JOIN conference_sessions c ON c.legacy_meeting_id = p.meeting_id AND c.origin = 'LEGACY' \
                    JOIN conference_participants cp ON cp.conference_id = c.id AND cp.user_id = p.user_id";
    let role_checked = n(c, &format!("SELECT count(*) {role_sql}")).await;
    let role_mismatches = n(c, &format!(
        "SELECT count(*) {role_sql} WHERE cp.role <> CASE WHEN p.role::text = 'host' AND p.user_id = m.host_id THEN 'OWNER' \
           WHEN p.role::text = 'host' THEN 'HOST' WHEN p.role::text = 'viewer' THEN 'VIEWER' ELSE 'PARTICIPANT' END \
           OR cp.admission_state <> CASE p.status::text WHEN 'waiting' THEN 'WAITING' WHEN 'joined' THEN 'ADMITTED' ELSE 'REMOVED' END"
    )).await;
    assert_eq!(role_mismatches, 0, "role / admission mapping");
    let host_roles = n(
        c,
        &format!("SELECT count(*) {role_sql} WHERE cp.role = 'OWNER'"),
    )
    .await;
    let distinct_logical = n(c, "SELECT count(*) FROM (SELECT DISTINCT meeting_id, coalesce(user_id::text, id::text) FROM participants) x").await;
    assert_eq!(
        ev.conference_participant_rows_from_legacy, distinct_logical,
        "rejoins collapse to one logical participant"
    );
    let joined_rows = n(
        c,
        "SELECT count(*) FROM participants WHERE status::text <> 'waiting'",
    )
    .await;
    assert_eq!(
        ev.participant_session_rows_from_legacy, joined_rows,
        "every admitted legacy row → one historical ParticipantSession"
    );

    // Exceptions (classified, never silently dropped).
    let inconsistent_window = n(
        c,
        "SELECT count(*) FROM meetings WHERE end_time IS NOT NULL AND end_time <= start_time",
    )
    .await;
    let window_repaired = n(c, "SELECT count(*) FROM meetings m JOIN conference_sessions c ON c.legacy_meeting_id = m.id WHERE m.end_time <= m.start_time AND c.scheduled_end_at IS NULL").await;
    assert_eq!(inconsistent_window, window_repaired);
    let ended_no_logs = n(c, "SELECT count(*) FROM meetings m WHERE m.status::text = 'ended' AND NOT EXISTS (SELECT 1 FROM session_logs l WHERE l.meeting_id = m.id)").await;
    let ended_unknown_start = n(c, "SELECT count(*) FROM conference_sessions WHERE origin = 'LEGACY' AND status = 'ENDED' AND started_at IS NULL").await;
    assert_eq!(ended_no_logs, ended_unknown_start);
    let never_joined = n(
        c,
        "SELECT count(*) FROM participants WHERE status::text = 'waiting'",
    )
    .await;
    let inactive_hosts = n(c, "SELECT count(*) FROM meetings m JOIN users u ON u.id = m.host_id WHERE u.status::text = 'inactive'").await;

    // 5. Idempotent rerun.
    let txn = c.begin().await.unwrap();
    let t = Instant::now();
    let rerun = phase2_backfill::run(&txn, None).await.unwrap();
    let rerun_ms = t.elapsed().as_millis();
    txn.commit().await.unwrap();
    assert_eq!(
        (
            rerun.inserted_spaces,
            rerun.inserted_conferences,
            rerun.inserted_participants,
            rerun.inserted_sessions,
            rerun.refreshed_rows
        ),
        (0, 0, 0, 0, 0),
        "{rerun:#?}"
    );

    // 6. Legacy reads, compatibility resolver, shadow comparison.
    let app = TestApp::on(db, TestOptions::default());
    let c = &app.db.conn;
    let sample = uuids(c, "SELECT m.id FROM meetings m JOIN users u ON u.id = m.host_id WHERE u.status::text = 'active' ORDER BY md5(m.id::text) LIMIT 25").await;
    let (mut legacy_reads, mut resolved) = (0, 0);
    let mut read_failures: Vec<String> = vec![];
    for id in &sample {
        let host = uuids(c, &format!("SELECT host_id FROM meetings WHERE id='{id}'")).await[0];
        let token = app
            .state
            .jwt_service
            .generate_access_token(host, "user")
            .unwrap();
        let (s, v) = app
            .get(format!("/api/v1/meetings/{id}"), Auth::Bearer(&token))
            .await;
        if s == StatusCode::OK && v.to_string().contains(&id.to_string()) {
            legacy_reads += 1;
        } else {
            read_failures.push(format!("{id}: {s}"));
        }
        let code = rows(
            c,
            &format!("SELECT meeting_identifier, '', 0::bigint FROM meetings WHERE id='{id}'"),
        )
        .await[0]
            .0
            .clone();
        let space = uuids(
            c,
            &format!("SELECT id FROM meeting_spaces WHERE legacy_meeting_id='{id}'"),
        )
        .await[0];
        let (s, v) = app.get(format!("/api/v2/resolve/{code}"), Auth::None).await;
        let (s2, v2) = app
            .get(
                format!("/api/v2/resolve/{}", code.to_lowercase()),
                Auth::None,
            )
            .await;
        if s == StatusCode::OK
            && s2 == StatusCode::OK
            && v.to_string().contains(&space.to_string())
            && v2.to_string().contains(&space.to_string())
        {
            resolved += 1;
        }
    }
    // An inactive owner account keeps no read access through the legacy API.
    let inactive = uuids(c, "SELECT m.id FROM meetings m JOIN users u ON u.id = m.host_id WHERE u.status::text = 'inactive' ORDER BY m.id LIMIT 5").await;
    let mut inactive_denied = 0;
    for id in &inactive {
        let host = uuids(c, &format!("SELECT host_id FROM meetings WHERE id='{id}'")).await[0];
        let token = app
            .state
            .jwt_service
            .generate_access_token(host, "user")
            .unwrap();
        let (s, _) = app
            .get(format!("/api/v1/meetings/{id}"), Auth::Bearer(&token))
            .await;
        if s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN {
            inactive_denied += 1;
        }
    }
    let shadow_sample = uuids(
        c,
        "SELECT id FROM meetings ORDER BY md5(id::text || 'shadow') LIMIT 200",
    )
    .await;
    let mut shadow_mismatches: Vec<Value> = vec![];
    for id in &shadow_sample {
        let (_, diffs) = app.state.meeting.compat.shadow_compare(*id).await.unwrap();
        if !diffs.is_empty() {
            shadow_mismatches.push(json!({ "legacyMeetingId": id, "diffs": diffs }));
        }
    }

    let report = json!({
        "classification": "DEVELOPMENT CERTIFICATION — PRODUCTION-LIKE SYNTHETIC DATASET (not a production migration)",
        "input": input,
        "timingsMs": { "seed": seed_ms, "partialRun": partial_ms, "concurrentResume": resume_ms, "idempotentRerun": rerun_ms },
        "meetingSpacesCreated": ev.meeting_space_rows,
        "conferenceSessionsCreated": ev.legacy_conference_sessions,
        "membersMapped": { "ownersMapped": owners_mapped, "note": "legacy schema has no membership table; ownership (host_id) maps to MeetingSpace.owner_user_id" },
        "participantsMapped": { "legacyRows": ev.legacy_participant_rows, "mappedRows": ev.mapped_legacy_participant_rows, "logicalConferenceParticipants": ev.conference_participant_rows_from_legacy, "ownersFromLegacyHostRows": host_roles, "roleAndAdmissionChecked": role_checked, "roleAndAdmissionMismatches": role_mismatches },
        "participantSessionsMapped": { "legacyNonWaitingRows": joined_rows, "participantSessions": ev.participant_session_rows_from_legacy },
        "legacyIdsMapped": ev.mapped_spaces,
        "legacyLinksPreserved": codes_preserved,
        "statusMapping": status_map.iter().map(|(l, v, k)| json!({ "legacy": l, "v2": v, "count": k })).collect::<Vec<_>>(),
        "skippedRecords": { "waitingParticipantRows (no ParticipantSession; mapped as WAITING participants)": never_joined },
        "invalidRecords": { "endBeforeStart": inconsistent_window },
        "repairableExceptions": { "endBeforeStart → scheduled_end_at NULL": window_repaired, "endedWithoutSessionLogs → started_at NULL": ended_unknown_start },
        "manualReviewExceptions": { "meetingsOwnedByInactiveUsers (mapped; owner account inactive)": inactive_hosts },
        "duplicatePrevention": { "duplicateSpaces": dup_spaces, "duplicateConferences": dup_conferences, "duplicateSessions": dup_sessions, "concurrentRunners": concurrent.len() + 1 },
        "partialResume": { "partiallyMigratedMeetings": partial.len(), "spacesAfterPartialRun": partial_spaces, "interruptedMeetings": interrupted.len(), "resumedClean": ev.is_clean() },
        "rerun": { "insertedSpaces": rerun.inserted_spaces, "insertedConferences": rerun.inserted_conferences, "insertedParticipants": rerun.inserted_participants, "insertedSessions": rerun.inserted_sessions, "refreshedRows": rerun.refreshed_rows },
        "legacyReadsAfterMigration": { "sampled": sample.len(), "ok": legacy_reads, "failures": read_failures, "inactiveOwnerSampled": inactive.len(), "inactiveOwnerDenied": inactive_denied },
        "compatibilityResolver": { "sampled": sample.len(), "resolvedExactAndCaseInsensitive": resolved },
        "shadowComparison": { "sampled": shadow_sample.len(), "mismatches": shadow_mismatches.len(), "firstMismatches": shadow_mismatches.iter().take(5).collect::<Vec<_>>() },
        "notApplicable": ["recurring meetings", "passcode meetings", "organization meetings", "space memberships"],
        "notApplicableReason": "no representation in the legacy schema (meetings/participants/session_logs); these exist only as Phase 2 v2 features",
    });
    println!("MIGRATION_REPORT {report}");
    if let Ok(dir) = std::env::var("CFMEET_CERT_OUT") {
        std::fs::write(
            format!("{dir}/migration-report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        legacy_reads,
        sample.len(),
        "legacy reads after migration: {report}"
    );
    assert_eq!(
        inactive_denied,
        inactive.len(),
        "inactive owner denied: {report}"
    );
    assert_eq!(resolved, sample.len(), "compatibility resolver: {report}");
    assert!(shadow_mismatches.is_empty(), "shadow comparison: {report}");
}
