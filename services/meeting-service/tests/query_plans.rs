//! Part M §42 — database query-plan certification at representative volume.
//!
//! Seeds a synthetic volume (scale with `QP_SPACES`, default 20 000 spaces → 40 000
//! conferences, ~400 000 participants and sessions, ~1 000 000 outbox events, plus a
//! single "hot" conference with `QP_HOT_PARTICIPANTS` (default 2 000) participants,
//! 200 000 events and 100 000 chat messages), ANALYZEs, then runs
//! `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` for the critical query shapes the code
//! issues (filters / ordering / limits mirror the SeaORM queries in `src/services`).
//! Fails if a critical query uses a sequential scan on a large table.
//! Emits one `QUERY_PLAN {json}` line per query (expected index, indexes used, seq-scanned
//! tables with their row counts, rows returned), `QUERY_PLAN_FAIL <query> | …` lines with
//! the SQL and the full rendered plan for every failing query, and the complete
//! `QUERY_PLANS {json}` report (also `$CFMEET_CERT_OUT/query-plans.json`).

mod common;

use common::*;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};
use std::time::Instant;
use uuid::Uuid;

async fn exec(c: &DatabaseConnection, sql: &str) {
    c.execute_unprepared(sql)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
}

async fn one<T: sea_orm::TryGetable>(c: &DatabaseConnection, sql: &str) -> T {
    c.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap_or_else(|e| panic!("{e}\n{sql}"))
    .unwrap()
    .try_get_by_index::<T>(0)
    .unwrap()
}

/// Walk a JSON plan: node types, relations, indexes, and seq scans.
fn walk(node: &Value, out: &mut Vec<(String, Option<String>, Option<String>, f64)>) {
    out.push((
        node["Node Type"].as_str().unwrap_or("").to_string(),
        node["Relation Name"].as_str().map(str::to_string),
        node["Index Name"].as_str().map(str::to_string),
        node["Actual Rows"].as_f64().unwrap_or(0.0),
    ));
    if let Some(children) = node["Plans"].as_array() {
        for c in children {
            walk(c, out);
        }
    }
}

/// The index each critical query shape is designed to use (reported next to the plan the
/// planner actually chose; the gate itself is "no sequential scan on a large table").
fn expected_index(query: &str) -> &'static str {
    match query {
        "conference participant lookup (registered)" => "uq_participant_registered_user",
        "conference participant lookup (scoped id)" => "uq_participant_scope",
        "participant list (admitted)" => "idx_participants_conference_state",
        "participant-session lookup (active by participant)" => "idx_sessions_participant_state",
        "participant-session lookup (provider identity)" => "uq_session_media_identity",
        "active sessions of a conference" => "idx_sessions_conference_active",
        "membership lookup" => "uq_meeting_member_active",
        "invitation lookup (token hash)" => "meeting_invitations_token_hash_key",
        "waiting room lookup" => "idx_participants_waiting",
        "conference event replay" | "conference current sequence" => "idx_outbox_conference_replay",
        "message pagination (room, newest page)" | "message pagination (room, deep cursor)" => {
            "idx_messages_main_cursor"
        }
        "poll lookup" => "idx_polls_conference",
        "Q&A lookup" => "idx_questions_conference",
        "artefact lookup" => "idx_artifact_conference_type",
        "scheduler due auto-end" => "uq_conference_one_live_per_space",
        "scheduler due auto-start (CREATED)" => "idx_conference_scheduler_start",
        "recurrence due materialisation" => "idx_recurrence_materialization",
        "outbox relay pending claim" => "idx_outbox_pending",
        "space resolve by meeting code" => "meeting_spaces_meeting_code_key",
        "space occurrences by schedule" => "idx_conference_space_start",
        _ => "?",
    }
}

/// Human-readable plan tree (EXPLAIN ANALYZE text equivalent) for failure diagnostics:
/// node, relation / index, estimated vs actual rows, filter / index condition.
fn render(node: &Value, depth: usize, out: &mut Vec<String>) {
    let mut line = format!(
        "{}{}",
        "  ".repeat(depth),
        node["Node Type"].as_str().unwrap_or("")
    );
    if let Some(r) = node["Relation Name"].as_str() {
        line += &format!(" on {r}");
    }
    if let Some(i) = node["Index Name"].as_str() {
        line += &format!(" using {i}");
    }
    line += &format!(
        " (est rows={}, actual rows={}, loops={}",
        node["Plan Rows"], node["Actual Rows"], node["Actual Loops"]
    );
    if let Some(r) = node["Rows Removed by Filter"].as_f64() {
        line += &format!(", removed by filter={r}");
    }
    line += ")";
    for k in ["Index Cond", "Recheck Cond", "Filter", "Sort Key"] {
        if !node[k].is_null() {
            line += &format!(" {k}: {}", node[k]);
        }
    }
    out.push(line);
    if let Some(children) = node["Plans"].as_array() {
        for c in children {
            render(c, depth + 1, out);
        }
    }
}

#[tokio::test]
#[ignore = "query-plan certification (scripts/certification/run-load.sh)"]
async fn query_plans_at_representative_volume() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let c = &db.conn;
    let spaces: i64 = std::env::var("QP_SPACES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let hot_n: i64 = std::env::var("QP_HOT_PARTICIPANTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000);
    let t = Instant::now();
    exec(c, &format!(
        "INSERT INTO users (id, username, email, auth_type, role, status, created_at, updated_at) \
         SELECT md5('qpu' || i)::uuid, 'qpu' || i, 'qpu' || i || '@synthetic.invalid', 'local', 'user', 'active', now(), now() FROM generate_series(1, {spaces}) i"
    )).await;
    exec(c, &format!(
        "INSERT INTO meeting_spaces (id, meeting_code, owner_user_id, title) \
         SELECT md5('qps' || i)::uuid, 'qp' || lpad(i::text, 8, '0'), md5('qpu' || i)::uuid, 'space ' || i FROM generate_series(1, {spaces}) i"
    )).await;
    exec(c, &format!(
        "INSERT INTO meeting_members (id, meeting_space_id, principal_type, principal_ref, user_id, role) \
         SELECT gen_random_uuid(), md5('qps' || i)::uuid, 'USER', md5('qpu' || (1 + (i * 7 + k) % {spaces}))::uuid::text, md5('qpu' || (1 + (i * 7 + k) % {spaces}))::uuid, 'PARTICIPANT' \
           FROM generate_series(1, {spaces}) i, generate_series(1, 3) k"
    )).await;
    exec(c, &format!(
        "INSERT INTO meeting_invitations (id, meeting_space_id, inviter_user_id, token_hash, expires_at, created_at) \
         SELECT gen_random_uuid(), md5('qps' || i)::uuid, md5('qpu' || i)::uuid, md5('qpi' || i || '-' || k), now() + interval '7 days', now() \
           FROM generate_series(1, {spaces}) i, generate_series(1, 2) k"
    )).await;
    // Two ENDED conferences per space, scheduled windows spread over a year.
    exec(c, &format!(
        "INSERT INTO conference_sessions (id, meeting_space_id, occurrence_key, kind, status, scheduled_start_at, scheduled_end_at, started_at, ended_at, termination_reason) \
         SELECT md5('qpc' || i || '-' || k)::uuid, md5('qps' || i)::uuid, 'occ-' || k, 'SCHEDULED', 'ENDED', \
                now() - (i % 365) * interval '1 day', now() - (i % 365) * interval '1 day' + interval '1 hour', \
                now() - (i % 365) * interval '1 day', now() - (i % 365) * interval '1 day' + interval '1 hour', 'HOST_ENDED' \
           FROM generate_series(1, {spaces}) i, generate_series(1, 2) k"
    )).await;
    // A slice of CREATED future occurrences (scheduler due-query realism).
    exec(c, &format!(
        "INSERT INTO conference_sessions (id, meeting_space_id, occurrence_key, kind, status, scheduled_start_at, scheduled_end_at) \
         SELECT gen_random_uuid(), md5('qps' || i)::uuid, 'future', 'SCHEDULED', 'CREATED', now() + (i % 30) * interval '1 day', now() + (i % 30) * interval '1 day' + interval '1 hour' \
           FROM generate_series(1, {spaces}) i WHERE i % 4 = 0"
    )).await;
    exec(c, &format!(
        "INSERT INTO meeting_recurrence_rules (id, meeting_space_id, rrule, dtstart_local, duration_minutes, timezone, materialized_until) \
         SELECT gen_random_uuid(), md5('qps' || i)::uuid, 'FREQ=WEEKLY;COUNT=20', now()::timestamp, 30, 'UTC', \
                CASE WHEN i % 500 = 0 THEN NULL ELSE now() + interval '60 days' END \
           FROM generate_series(1, {spaces}) i WHERE i % 5 = 0"
    )).await;
    // 10 participants (+ 1 session each) per historical conference.
    exec(c, &format!(
        "INSERT INTO conference_participants (id, conference_id, identity_type, user_id, guest_identity, display_name, role, role_source, admission_state, admitted_at) \
         SELECT gen_random_uuid(), cs.id, CASE WHEN k % 3 = 0 THEN 'GUEST' ELSE 'REGISTERED_USER' END, \
                CASE WHEN k % 3 = 0 THEN NULL ELSE md5('qpu' || (1 + (abs(hashtext(cs.id::text)) + k) % {spaces}))::uuid END, \
                CASE WHEN k % 3 = 0 THEN md5(cs.id::text || k) END, 'p' || k, 'PARTICIPANT', 'DEFAULT', 'ADMITTED', now() \
           FROM conference_sessions cs, generate_series(1, 10) k WHERE cs.status = 'ENDED' \
         ON CONFLICT DO NOTHING"
    )).await;
    exec(c,
        "INSERT INTO participant_sessions (id, conference_participant_id, conference_id, media_identity, state, established_at, disconnected_at) \
         SELECT gen_random_uuid(), p.id, p.conference_id, 'ps_' || replace(gen_random_uuid()::text, '-', ''), 'HISTORICAL', now() - interval '2 hours', now() - interval '1 hour' \
           FROM conference_participants p",
    ).await;
    exec(c,
        "INSERT INTO outbox_events (id, aggregate_type, aggregate_id, event_type, conference_id, conference_seq, channel, status, payload) \
         SELECT gen_random_uuid(), 'conference', cs.id, 'participant.joined', cs.id, k, 'REALTIME', 'PUBLISHED', '{}'::jsonb \
           FROM conference_sessions cs, generate_series(1, 25) k WHERE cs.status = 'ENDED'",
    ).await;
    // The hot LIVE conference.
    let hot_space = Uuid::new_v4();
    let hot = Uuid::new_v4();
    exec(c, &format!("INSERT INTO meeting_spaces (id, meeting_code, owner_user_id, title) VALUES ('{hot_space}', 'qphot0001', md5('qpu1')::uuid, 'hot')")).await;
    exec(c, &format!(
        "INSERT INTO conference_sessions (id, meeting_space_id, occurrence_key, kind, status, started_at, scheduled_start_at, scheduled_end_at) \
         VALUES ('{hot}', '{hot_space}', 'hot', 'SCHEDULED', 'LIVE', now(), now(), now() + interval '1 hour')"
    )).await;
    exec(c, &format!(
        "INSERT INTO conference_participants (id, conference_id, identity_type, user_id, guest_identity, display_name, role, role_source, admission_state, admitted_at, requested_at) \
         SELECT gen_random_uuid(), '{hot}', CASE WHEN k % 2 = 0 THEN 'GUEST' ELSE 'REGISTERED_USER' END, \
                CASE WHEN k % 2 = 0 THEN NULL ELSE md5('qpu' || k)::uuid END, CASE WHEN k % 2 = 0 THEN md5('hot' || k) END, 'hot ' || k, 'PARTICIPANT', 'DEFAULT', \
                CASE WHEN k % 20 = 0 THEN 'WAITING' ELSE 'ADMITTED' END, CASE WHEN k % 20 = 0 THEN NULL ELSE now() END, now() - k * interval '1 second' \
           FROM generate_series(1, {hot_n}) k"
    )).await;
    exec(c, &format!(
        "INSERT INTO participant_sessions (id, conference_participant_id, conference_id, media_identity, state, established_at) \
         SELECT gen_random_uuid(), p.id, p.conference_id, 'ps_' || replace(gen_random_uuid()::text, '-', ''), CASE WHEN abs(hashtext(p.id::text)) % 10 = 0 THEN 'PENDING' ELSE 'CONNECTED' END, now() \
           FROM conference_participants p WHERE p.conference_id = '{hot}' AND p.admission_state = 'ADMITTED'"
    )).await;
    exec(c, &format!(
        "INSERT INTO outbox_events (id, aggregate_type, aggregate_id, event_type, conference_id, conference_seq, channel, status, payload) \
         SELECT gen_random_uuid(), 'conference', '{hot}', 'chat.message_created', '{hot}', k, 'REALTIME', 'PUBLISHED', '{{}}'::jsonb FROM generate_series(1, 200000) k"
    )).await;
    exec(c, &format!(
        "INSERT INTO conference_messages (id, conference_id, sender_participant_id, body, created_at) \
         SELECT gen_random_uuid(), '{hot}', p.id, 'message ' || k, now() - k * interval '10 milliseconds' \
           FROM generate_series(1, 100000) k JOIN LATERAL (SELECT id FROM conference_participants WHERE conference_id = '{hot}' ORDER BY id OFFSET (k % {hot_n}) LIMIT 1) p ON true"
    )).await;
    // Background chat history: 5 messages in every historical conference.
    exec(c,
        "INSERT INTO conference_messages (id, conference_id, sender_participant_id, body, created_at) \
         SELECT gen_random_uuid(), p.conference_id, p.id, 'history', now() - interval '30 days' \
           FROM conference_participants p JOIN conference_sessions cs ON cs.id = p.conference_id \
          WHERE cs.status = 'ENDED' AND p.display_name IN ('p1', 'p2', 'p4', 'p5', 'p7')",
    ).await;
    exec(c, &format!(
        "INSERT INTO meeting_polls (id, conference_id, question, status, created_at) \
         SELECT gen_random_uuid(), cs.id, 'poll?', 'CLOSED', now() FROM conference_sessions cs WHERE cs.status = 'ENDED' AND abs(hashtext(cs.id::text)) % 4 = 0 \
         UNION ALL SELECT gen_random_uuid(), '{hot}', 'hot poll ' || k, 'OPEN', now() FROM generate_series(1, 50) k"
    )).await;
    exec(c, &format!(
        "INSERT INTO meeting_questions (id, conference_id, participant_id, body, status, created_at) \
         SELECT gen_random_uuid(), '{hot}', p.id, 'question', 'OPEN', now() FROM conference_participants p WHERE p.conference_id = '{hot}' AND abs(hashtext(p.id::text)) % 4 = 0"
    )).await;
    // Background Q&A history (3 questions in every second historical conference), like
    // the poll / chat / artefact history: without it the table holds only the hot
    // conference's questions, the lookup returns 100 % of a few-page table and a
    // sequential scan is the planner's correct choice — not a representative check.
    exec(c,
        "INSERT INTO meeting_questions (id, conference_id, participant_id, body, status, created_at) \
         SELECT gen_random_uuid(), p.conference_id, p.id, 'history question', \
                (ARRAY['ANSWERED','OPEN','DISMISSED'])[1 + abs(hashtext(p.id::text)) % 3], now() - interval '30 days' \
           FROM conference_participants p JOIN conference_sessions cs ON cs.id = p.conference_id \
          WHERE cs.status = 'ENDED' AND abs(hashtext(cs.id::text)) % 2 = 0 AND p.display_name IN ('p1', 'p2', 'p4')",
    ).await;
    exec(c,
        "INSERT INTO meeting_artifacts (id, conference_id, meeting_space_id, artifact_type, status, created_at) \
         SELECT gen_random_uuid(), cs.id, cs.meeting_space_id, 'RECORDING', 'FAILED', now() FROM conference_sessions cs WHERE cs.status = 'ENDED' AND abs(hashtext(cs.id::text)) % 3 = 0",
    ).await;
    exec(c, "ANALYZE").await;
    let seed_secs = t.elapsed().as_secs_f64();

    let counts = json!({
        "meetingSpaces": one::<i64>(c, "SELECT count(*) FROM meeting_spaces").await,
        "conferenceSessions": one::<i64>(c, "SELECT count(*) FROM conference_sessions").await,
        "conferenceParticipants": one::<i64>(c, "SELECT count(*) FROM conference_participants").await,
        "participantSessions": one::<i64>(c, "SELECT count(*) FROM participant_sessions").await,
        "meetingMembers": one::<i64>(c, "SELECT count(*) FROM meeting_members").await,
        "meetingInvitations": one::<i64>(c, "SELECT count(*) FROM meeting_invitations").await,
        "outboxEvents": one::<i64>(c, "SELECT count(*) FROM outbox_events").await,
        "conferenceMessages": one::<i64>(c, "SELECT count(*) FROM conference_messages").await,
        "meetingPolls": one::<i64>(c, "SELECT count(*) FROM meeting_polls").await,
        "meetingQuestions": one::<i64>(c, "SELECT count(*) FROM meeting_questions").await,
        "meetingArtifacts": one::<i64>(c, "SELECT count(*) FROM meeting_artifacts").await,
        "recurrenceRules": one::<i64>(c, "SELECT count(*) FROM meeting_recurrence_rules").await,
        "hotConferenceParticipants": hot_n,
    });
    let any_user: Uuid = one(c, &format!("SELECT user_id FROM conference_participants WHERE conference_id='{hot}' AND user_id IS NOT NULL LIMIT 1")).await;
    let any_pid: Uuid = one(
        c,
        &format!("SELECT id FROM conference_participants WHERE conference_id='{hot}' LIMIT 1"),
    )
    .await;
    let any_identity: String = one(
        c,
        &format!(
            "SELECT media_identity FROM participant_sessions WHERE conference_id='{hot}' LIMIT 1"
        ),
    )
    .await;
    let any_token: String = one(c, "SELECT token_hash FROM meeting_invitations LIMIT 1").await;
    let any_space: Uuid = one(c, "SELECT meeting_space_id FROM meeting_members LIMIT 1").await;
    let any_artifact_conf: Uuid =
        one(c, "SELECT conference_id FROM meeting_artifacts LIMIT 1").await;
    let queries: Vec<(&str, String)> = vec![
        ("conference participant lookup (registered)", format!("SELECT * FROM conference_participants WHERE conference_id='{hot}' AND user_id='{any_user}'")),
        ("conference participant lookup (scoped id)", format!("SELECT * FROM conference_participants WHERE id='{any_pid}' AND conference_id='{hot}'")),
        ("participant list (admitted)", format!("SELECT * FROM conference_participants WHERE conference_id='{hot}' AND admission_state='ADMITTED'")),
        ("participant-session lookup (active by participant)", format!("SELECT * FROM participant_sessions WHERE conference_participant_id='{any_pid}' AND state IN ('PENDING','CONNECTED')")),
        ("participant-session lookup (provider identity)", format!("SELECT * FROM participant_sessions WHERE conference_id='{hot}' AND media_identity='{any_identity}'")),
        ("active sessions of a conference", format!("SELECT * FROM participant_sessions WHERE conference_id='{hot}' AND state IN ('PENDING','CONNECTED')")),
        ("membership lookup", format!("SELECT * FROM meeting_members WHERE meeting_space_id='{any_space}' AND status='ACTIVE' AND (user_id='{any_user}' OR (principal_type='EMAIL' AND principal_ref='x@synthetic.invalid'))")),
        ("invitation lookup (token hash)", format!("SELECT * FROM meeting_invitations WHERE token_hash='{any_token}'")),
        ("waiting room lookup", format!("SELECT * FROM conference_participants WHERE conference_id='{hot}' AND admission_state='WAITING' ORDER BY requested_at")),
        ("conference event replay", format!("SELECT * FROM outbox_events WHERE conference_id='{hot}' AND channel='REALTIME' AND conference_seq > 199500 ORDER BY conference_seq LIMIT 200")),
        ("conference current sequence", format!("SELECT max(conference_seq) FROM outbox_events WHERE conference_id='{hot}' AND channel='REALTIME'")),
        ("message pagination (room, newest page)", format!("SELECT * FROM conference_messages WHERE conference_id='{hot}' AND breakout_room_id IS NULL AND recipient_participant_id IS NULL ORDER BY created_at DESC, id DESC LIMIT 50")),
        ("message pagination (room, deep cursor)", format!("SELECT * FROM conference_messages WHERE conference_id='{hot}' AND breakout_room_id IS NULL AND recipient_participant_id IS NULL AND created_at < now() - interval '15 minutes' ORDER BY created_at DESC, id DESC LIMIT 50")),
        ("poll lookup", format!("SELECT * FROM meeting_polls WHERE conference_id='{hot}' AND status='OPEN' ORDER BY created_at")),
        ("Q&A lookup", format!("SELECT * FROM meeting_questions WHERE conference_id='{hot}' AND status='OPEN' ORDER BY created_at")),
        ("artefact lookup", format!("SELECT * FROM meeting_artifacts WHERE conference_id='{any_artifact_conf}' AND artifact_type='RECORDING' ORDER BY created_at")),
        ("scheduler due auto-end", "SELECT * FROM conference_sessions WHERE status='LIVE' AND origin='V2' AND scheduled_end_at < now() LIMIT 100".to_string()),
        ("scheduler due auto-start (CREATED)", "SELECT * FROM conference_sessions WHERE status='CREATED' AND scheduled_start_at <= now() + interval '5 minutes' ORDER BY scheduled_start_at LIMIT 100".to_string()),
        ("recurrence due materialisation", "SELECT * FROM meeting_recurrence_rules WHERE status='ACTIVE' AND (materialized_until IS NULL OR materialized_until < now() + interval '30 days') LIMIT 100".to_string()),
        ("outbox relay pending claim", "SELECT * FROM outbox_events WHERE status='PENDING' AND available_at <= now() ORDER BY available_at LIMIT 100 FOR UPDATE SKIP LOCKED".to_string()),
        ("space resolve by meeting code", "SELECT * FROM meeting_spaces WHERE meeting_code='qp00012345'".to_string()),
        ("space occurrences by schedule", format!("SELECT * FROM conference_sessions WHERE meeting_space_id='{any_space}' ORDER BY scheduled_start_at")),
    ];
    let large = [
        "conference_participants",
        "participant_sessions",
        "outbox_events",
        "conference_messages",
        "conference_sessions",
        "meeting_members",
        "meeting_invitations",
        "meeting_spaces",
        "meeting_artifacts",
        "meeting_polls",
        "meeting_questions",
        "meeting_recurrence_rules",
    ];
    let mut table_rows = std::collections::BTreeMap::new();
    for t in large {
        table_rows.insert(t, one::<i64>(c, &format!("SELECT count(*) FROM {t}")).await);
    }
    let mut results = vec![];
    let mut failures = vec![];
    let mut failure_detail = vec![];
    for (name, sql) in &queries {
        let plan: Value = {
            let raw: String =
                one::<Value>(c, &format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {sql}"))
                    .await
                    .to_string();
            serde_json::from_str(&raw).unwrap()
        };
        let root = &plan[0];
        let mut nodes = vec![];
        walk(&root["Plan"], &mut nodes);
        let seq_scans: Vec<String> = nodes
            .iter()
            .filter(|(t, rel, _, _)| {
                t == "Seq Scan" && rel.as_deref().map(|r| large.contains(&r)).unwrap_or(false)
            })
            .map(|(_, rel, _, _)| rel.clone().unwrap())
            .collect();
        let indexes: Vec<String> = nodes.iter().filter_map(|(_, _, i, _)| i.clone()).collect();
        let exec_ms = root["Execution Time"].as_f64().unwrap_or(0.0);
        let rows_returned = root["Plan"]["Actual Rows"].as_f64().unwrap_or(0.0);
        let verdict = if seq_scans.is_empty() {
            "INDEX"
        } else {
            "SEQ SCAN"
        };
        let seq_detail: Vec<Value> = seq_scans
            .iter()
            .map(|t| json!({ "table": t, "tableRows": table_rows.get(t.as_str()) }))
            .collect();
        let mut tree = vec![];
        render(&root["Plan"], 0, &mut tree);
        let summary = json!({ "query": name, "verdict": verdict, "expectedIndex": expected_index(name),
                              "indexesUsed": indexes, "seqScans": seq_detail, "rowsReturned": rows_returned,
                              "executionMs": exec_ms });
        println!("QUERY_PLAN {summary}");
        if !seq_scans.is_empty() {
            failures.push(name.to_string());
            println!("QUERY_PLAN_FAIL {name} | sql: {sql}");
            for l in &tree {
                println!("QUERY_PLAN_FAIL {name} | {l}");
            }
            failure_detail.push(summary.clone());
        }
        results.push(json!({ "query": name, "verdict": verdict, "expectedIndex": expected_index(name),
                             "indexes": indexes, "seqScans": seq_detail, "rowsReturned": rows_returned,
                             "executionMs": exec_ms, "sql": sql, "plan": tree,
                             "nodes": nodes.iter().map(|(t, r, i, rows)| json!([t, r, i, rows])).collect::<Vec<_>>() }));
    }
    let report = json!({ "seedSecs": seed_secs, "volumes": counts, "tableRows": table_rows,
                         "plans": results, "seqScanFailures": failures });
    println!("QUERY_PLANS {report}");
    if let Ok(dir) = std::env::var("CFMEET_CERT_OUT") {
        std::fs::write(
            format!("{dir}/query-plans.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    assert!(
        failures.is_empty(),
        "unacceptable sequential scans: {}",
        Value::from(failure_detail)
    );
}
