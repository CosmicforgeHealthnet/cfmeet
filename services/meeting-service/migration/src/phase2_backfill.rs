//! Phase 2 legacy → v2 backfill (spec v1.1 §B15/§B16).
//!
//! Shared by the `m20261003_000002_phase2_legacy_backfill` migration, the
//! meeting-service compatibility resolver (single-meeting mode) and the
//! `phase2_backfill` evidence binary.
//!
//! Every statement is idempotent: stable mappings (`legacy_meeting_id`,
//! `legacy_participant_id`, `(conference_id, user_id)`, `(conference_id, media_identity)`)
//! plus `ON CONFLICT DO NOTHING` mean a rerun inserts nothing new. Runs are
//! serialized with a transaction-scoped advisory lock, so callers must invoke
//! [`run`] inside a transaction.

use sea_orm_migration::sea_orm::{
    prelude::Uuid, ConnectionTrait, DbBackend, DbErr, Statement, Value,
};

/// Advisory lock key shared by every backfill caller.
const LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtext('cfmeet.phase2_backfill'))";

/// Migration evidence produced by a backfill run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    pub legacy_meeting_rows: i64,
    pub meeting_space_rows: i64,
    pub mapped_spaces: i64,
    pub mapping_failures: i64,
    pub unmapped_meeting_ids: Vec<Uuid>,
    pub legacy_conference_sessions: i64,
    pub legacy_participant_rows: i64,
    pub mapped_legacy_participant_rows: i64,
    pub conference_participant_rows_from_legacy: i64,
    pub participant_session_rows_from_legacy: i64,
    pub unmapped_participant_ids: Vec<Uuid>,
    /// Rows inserted/updated by this run (0 on an idempotent rerun).
    pub inserted_spaces: u64,
    pub inserted_conferences: u64,
    pub inserted_participants: u64,
    pub inserted_sessions: u64,
    pub refreshed_rows: u64,
}

impl BackfillReport {
    pub fn is_clean(&self) -> bool {
        self.mapping_failures == 0 && self.unmapped_participant_ids.is_empty()
    }
}

fn stmt(sql: &str, filter: Option<Uuid>) -> Statement {
    Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        [Value::Uuid(filter.map(Box::new))],
    )
}

const INSERT_SPACES: &str = r#"
INSERT INTO meeting_spaces (
    id, legacy_meeting_id, owner_user_id, meeting_code, title, description, meeting_type, status,
    timezone, access_policy, waiting_room_policy, security_profile, default_role,
    interactive_capacity, viewer_capacity, recording_policy, transcription_policy, origin,
    created_at, updated_at, version)
SELECT gen_random_uuid(), m.id, m.host_id, m.meeting_identifier, m.title, NULL, 'STANDARD', 'ACTIVE',
       'UTC',
       CASE WHEN m.is_private THEN 'AUTHENTICATED' ELSE 'PUBLIC' END,
       CASE WHEN m.is_private THEN 'EVERYONE' ELSE 'OFF' END,
       'STANDARD', 'PARTICIPANT', 100, 0, 'HOST_OPTIONAL', 'DISABLED', 'LEGACY',
       m.created_at AT TIME ZONE 'UTC', m.updated_at AT TIME ZONE 'UTC', 1
  FROM meetings m
 WHERE ($1::uuid IS NULL OR m.id = $1::uuid)
ON CONFLICT DO NOTHING
"#;

const LEGACY_STATUS: &str =
    "CASE m.status::text WHEN 'scheduled' THEN 'CREATED' WHEN 'ongoing' THEN 'LIVE' \
     WHEN 'ended' THEN 'ENDED' ELSE 'CANCELLED' END";

fn insert_conferences_sql() -> String {
    format!(
        r#"
INSERT INTO conference_sessions (
    id, meeting_space_id, occurrence_key, kind, origin, legacy_meeting_id, status,
    scheduled_start_at, scheduled_end_at, started_at, ended_at, termination_reason,
    rtc_provider, rtc_room_name, created_at, updated_at, version)
SELECT gen_random_uuid(), s.id, 'legacy:' || m.id::text, 'LEGACY', 'LEGACY', m.id, {status},
       m.start_time AT TIME ZONE 'UTC',
       CASE WHEN m.end_time > m.start_time THEN m.end_time AT TIME ZONE 'UTC' END,
       (SELECT min(l.event_time) FROM session_logs l
         WHERE l.meeting_id = m.id AND l.event_type::text = 'meeting_start') AT TIME ZONE 'UTC',
       CASE WHEN m.status::text = 'ended' THEN
           (SELECT max(l.event_time) FROM session_logs l
             WHERE l.meeting_id = m.id AND l.event_type::text = 'meeting_end') AT TIME ZONE 'UTC' END,
       CASE WHEN m.status::text IN ('ended','cancelled') THEN 'LEGACY' END,
       'LIVEKIT', m.meeting_identifier,
       m.created_at AT TIME ZONE 'UTC', m.updated_at AT TIME ZONE 'UTC', 1
  FROM meetings m
  JOIN meeting_spaces s ON s.legacy_meeting_id = m.id
 WHERE ($1::uuid IS NULL OR m.id = $1::uuid)
ON CONFLICT DO NOTHING
"#,
        status = LEGACY_STATUS
    )
}

/// Legacy-origin conferences stay legacy-managed until legacy writes retire:
/// their lifecycle is refreshed deterministically from the legacy row.
fn refresh_conferences_sql() -> String {
    format!(
        r#"
UPDATE conference_sessions c
   SET status = {status},
       termination_reason = CASE WHEN m.status::text IN ('ended','cancelled') THEN 'LEGACY' END,
       updated_at = now(),
       version = c.version + 1
  FROM meetings m
 WHERE c.legacy_meeting_id = m.id AND c.origin = 'LEGACY'
   AND ($1::uuid IS NULL OR m.id = $1::uuid)
   AND c.status IS DISTINCT FROM {status}
"#,
        status = LEGACY_STATUS
    )
}

/// Legacy-origin spaces never edited through v2 (`version = 1`) follow the legacy row.
const REFRESH_SPACES: &str = r#"
UPDATE meeting_spaces s
   SET title = m.title,
       owner_user_id = m.host_id,
       access_policy = CASE WHEN m.is_private THEN 'AUTHENTICATED' ELSE 'PUBLIC' END,
       waiting_room_policy = CASE WHEN m.is_private THEN 'EVERYONE' ELSE 'OFF' END,
       updated_at = now()
  FROM meetings m
 WHERE s.legacy_meeting_id = m.id AND s.origin = 'LEGACY' AND s.version = 1
   AND ($1::uuid IS NULL OR m.id = $1::uuid)
   AND (s.title IS DISTINCT FROM m.title OR s.owner_user_id IS DISTINCT FROM m.host_id
        OR s.access_policy IS DISTINCT FROM CASE WHEN m.is_private THEN 'AUTHENTICATED' ELSE 'PUBLIC' END)
"#;

/// A legacy meeting that was hard-deleted leaves `legacy_meeting_id = NULL`
/// (FK `ON DELETE SET NULL`); its legacy-origin space is soft-deleted.
const MARK_DELETED_SPACES: &str = r#"
UPDATE meeting_spaces SET status = 'DELETED', updated_at = now()
 WHERE origin = 'LEGACY' AND legacy_meeting_id IS NULL AND status <> 'DELETED' AND $1::uuid IS NULL
"#;

const PARTICIPANT_ROLE: &str = "CASE WHEN p.role::text = 'host' AND p.user_id = m.host_id THEN 'OWNER' \
     WHEN p.role::text = 'host' THEN 'HOST' WHEN p.role::text = 'viewer' THEN 'VIEWER' ELSE 'PARTICIPANT' END";
const PARTICIPANT_STATE: &str =
    "CASE p.status::text WHEN 'waiting' THEN 'WAITING' WHEN 'joined' THEN 'ADMITTED' \
     ELSE 'REMOVED' END";

/// Registered users: many legacy rows (one per join) collapse into ONE logical
/// participant; the latest legacy row decides role/state.
fn insert_registered_participants_sql() -> String {
    format!(
        r#"
INSERT INTO conference_participants (
    id, conference_id, identity_type, user_id, display_name, role, role_source, admission_state,
    requested_at, admitted_at, admitted_by, removed_at, removed_by, created_at, updated_at, version)
SELECT gen_random_uuid(), c.id, 'REGISTERED_USER', p.user_id, left(p.display_name, 100),
       {role}, 'LEGACY', {state},
       p.first_join AT TIME ZONE 'UTC',
       CASE WHEN p.status::text = 'joined' THEN p.join_time AT TIME ZONE 'UTC' END,
       CASE WHEN p.status::text = 'joined' THEN 'system:legacy' END,
       CASE WHEN p.status::text = 'kicked' THEN COALESCE(p.leave_time, p.join_time) AT TIME ZONE 'UTC' END,
       CASE WHEN p.status::text = 'kicked' THEN 'system:legacy' END,
       p.first_join AT TIME ZONE 'UTC', now(), 1
  FROM (SELECT DISTINCT ON (lp.meeting_id, lp.user_id) lp.*,
               min(lp.join_time) OVER (PARTITION BY lp.meeting_id, lp.user_id) AS first_join
          FROM participants lp
         WHERE lp.user_id IS NOT NULL AND ($1::uuid IS NULL OR lp.meeting_id = $1::uuid)
         ORDER BY lp.meeting_id, lp.user_id, lp.join_time DESC) p
  JOIN meetings m ON m.id = p.meeting_id
  JOIN conference_sessions c ON c.legacy_meeting_id = p.meeting_id AND c.origin = 'LEGACY'
ON CONFLICT DO NOTHING
"#,
        role = PARTICIPANT_ROLE,
        state = PARTICIPANT_STATE
    )
}

/// Guests: each legacy guest row is its own logical participant. Display name
/// is never used to merge guests.
fn insert_guest_participants_sql() -> String {
    format!(
        r#"
INSERT INTO conference_participants (
    id, conference_id, identity_type, guest_identity, legacy_participant_id, display_name, role,
    role_source, admission_state, requested_at, admitted_at, admitted_by, removed_at, removed_by,
    created_at, updated_at, version)
SELECT gen_random_uuid(), c.id, 'GUEST', 'legacy-' || p.id::text, p.id, left(p.display_name, 100),
       {role}, 'LEGACY', {state},
       p.join_time AT TIME ZONE 'UTC',
       CASE WHEN p.status::text = 'joined' THEN p.join_time AT TIME ZONE 'UTC' END,
       CASE WHEN p.status::text = 'joined' THEN 'system:legacy' END,
       CASE WHEN p.status::text = 'kicked' THEN COALESCE(p.leave_time, p.join_time) AT TIME ZONE 'UTC' END,
       CASE WHEN p.status::text = 'kicked' THEN 'system:legacy' END,
       p.join_time AT TIME ZONE 'UTC', now(), 1
  FROM participants p
  JOIN meetings m ON m.id = p.meeting_id
  JOIN conference_sessions c ON c.legacy_meeting_id = p.meeting_id AND c.origin = 'LEGACY'
 WHERE p.user_id IS NULL AND ($1::uuid IS NULL OR p.meeting_id = $1::uuid)
ON CONFLICT DO NOTHING
"#,
        role = PARTICIPANT_ROLE,
        state = PARTICIPANT_STATE
    )
}

/// Historical sessions only where legacy data proves a connection existed
/// (admitted rows). Waiting-only rows never connected → no session. The legacy
/// participant id was the LiveKit identity, so it is reused as media identity.
const INSERT_SESSIONS: &str = r#"
INSERT INTO participant_sessions (
    id, conference_participant_id, conference_id, media_identity, state, established_at,
    disconnected_at, end_reason, legacy_participant_id, created_at, updated_at)
SELECT gen_random_uuid(), cp.id, cp.conference_id, p.id::text, 'HISTORICAL',
       p.join_time AT TIME ZONE 'UTC',
       CASE WHEN p.leave_time >= p.join_time THEN p.leave_time AT TIME ZONE 'UTC' END,
       CASE WHEN p.status::text = 'kicked' THEN 'LEGACY_KICKED' ELSE 'LEGACY_IMPORT' END,
       p.id, now(), now()
  FROM participants p
  JOIN conference_sessions c ON c.legacy_meeting_id = p.meeting_id AND c.origin = 'LEGACY'
  JOIN conference_participants cp ON cp.conference_id = c.id
       AND ((p.user_id IS NOT NULL AND cp.user_id = p.user_id)
         OR (p.user_id IS NULL AND cp.legacy_participant_id = p.id))
 WHERE p.status::text <> 'waiting' AND ($1::uuid IS NULL OR p.meeting_id = $1::uuid)
ON CONFLICT DO NOTHING
"#;

async fn scalar<C: ConnectionTrait>(
    conn: &C,
    sql: &str,
    filter: Option<Uuid>,
) -> Result<i64, DbErr> {
    let row = conn
        .query_one(stmt(sql, filter))
        .await?
        .ok_or_else(|| DbErr::Custom("count query returned no row".into()))?;
    row.try_get_by_index::<i64>(0)
}

async fn ids<C: ConnectionTrait>(
    conn: &C,
    sql: &str,
    filter: Option<Uuid>,
) -> Result<Vec<Uuid>, DbErr> {
    let rows = conn.query_all(stmt(sql, filter)).await?;
    rows.iter().map(|r| r.try_get_by_index::<Uuid>(0)).collect()
}

/// Run the backfill. `filter = Some(legacy_meeting_id)` restricts it to one meeting.
/// Must be called inside a transaction (advisory lock is transaction-scoped).
pub async fn run<C: ConnectionTrait>(
    conn: &C,
    filter: Option<Uuid>,
) -> Result<BackfillReport, DbErr> {
    conn.execute_unprepared(LOCK_SQL).await?;

    let inserted_spaces = conn
        .execute(stmt(INSERT_SPACES, filter))
        .await?
        .rows_affected();
    let inserted_conferences = conn
        .execute(stmt(&insert_conferences_sql(), filter))
        .await?
        .rows_affected();
    let mut refreshed_rows = conn
        .execute(stmt(&refresh_conferences_sql(), filter))
        .await?
        .rows_affected();
    refreshed_rows += conn
        .execute(stmt(REFRESH_SPACES, filter))
        .await?
        .rows_affected();
    refreshed_rows += conn
        .execute(stmt(MARK_DELETED_SPACES, filter))
        .await?
        .rows_affected();
    let inserted_participants = conn
        .execute(stmt(&insert_registered_participants_sql(), filter))
        .await?
        .rows_affected()
        + conn
            .execute(stmt(&insert_guest_participants_sql(), filter))
            .await?
            .rows_affected();
    let inserted_sessions = conn
        .execute(stmt(INSERT_SESSIONS, filter))
        .await?
        .rows_affected();

    report(
        conn,
        filter,
        inserted_spaces,
        inserted_conferences,
        inserted_participants,
        inserted_sessions,
        refreshed_rows,
    )
    .await
}

/// Produce migration evidence without writing anything.
pub async fn evidence<C: ConnectionTrait>(
    conn: &C,
    filter: Option<Uuid>,
) -> Result<BackfillReport, DbErr> {
    report(conn, filter, 0, 0, 0, 0, 0).await
}

async fn report<C: ConnectionTrait>(
    conn: &C,
    filter: Option<Uuid>,
    inserted_spaces: u64,
    inserted_conferences: u64,
    inserted_participants: u64,
    inserted_sessions: u64,
    refreshed_rows: u64,
) -> Result<BackfillReport, DbErr> {
    const F: &str = "($1::uuid IS NULL OR m.id = $1::uuid)";
    const PF: &str = "($1::uuid IS NULL OR p.meeting_id = $1::uuid)";
    let legacy_meeting_rows = scalar(
        conn,
        &format!("SELECT count(*) FROM meetings m WHERE {F}"),
        filter,
    )
    .await?;
    let meeting_space_rows = scalar(
        conn,
        "SELECT count(*) FROM meeting_spaces WHERE $1::uuid IS NULL OR legacy_meeting_id = $1::uuid",
        filter,
    )
    .await?;
    let mapped_spaces = scalar(
        conn,
        &format!("SELECT count(*) FROM meetings m JOIN meeting_spaces s ON s.legacy_meeting_id = m.id WHERE {F}"),
        filter,
    )
    .await?;
    let unmapped_meeting_ids = ids(
        conn,
        &format!(
            "SELECT m.id FROM meetings m LEFT JOIN meeting_spaces s ON s.legacy_meeting_id = m.id \
             WHERE s.id IS NULL AND {F} ORDER BY m.id LIMIT 100"
        ),
        filter,
    )
    .await?;
    let legacy_conference_sessions = scalar(
        conn,
        "SELECT count(*) FROM conference_sessions WHERE origin = 'LEGACY' \
         AND ($1::uuid IS NULL OR legacy_meeting_id = $1::uuid)",
        filter,
    )
    .await?;
    let legacy_participant_rows = scalar(
        conn,
        &format!("SELECT count(*) FROM participants p WHERE {PF}"),
        filter,
    )
    .await?;
    let mapped_sql = format!(
        "FROM participants p \
           LEFT JOIN conference_sessions c ON c.legacy_meeting_id = p.meeting_id AND c.origin = 'LEGACY' \
           LEFT JOIN conference_participants cp ON cp.conference_id = c.id \
                AND ((p.user_id IS NOT NULL AND cp.user_id = p.user_id) \
                  OR (p.user_id IS NULL AND cp.legacy_participant_id = p.id)) \
          WHERE {PF}"
    );
    let mapped_legacy_participant_rows =
        scalar(conn, &format!("SELECT count(cp.id) {mapped_sql}"), filter).await?;
    let unmapped_participant_ids = ids(
        conn,
        &format!("SELECT p.id {mapped_sql} AND cp.id IS NULL ORDER BY p.id LIMIT 100"),
        filter,
    )
    .await?;
    let conference_participant_rows_from_legacy = scalar(
        conn,
        "SELECT count(*) FROM conference_participants cp JOIN conference_sessions c ON c.id = cp.conference_id \
          WHERE c.origin = 'LEGACY' AND ($1::uuid IS NULL OR c.legacy_meeting_id = $1::uuid)",
        filter,
    )
    .await?;
    let participant_session_rows_from_legacy = scalar(
        conn,
        "SELECT count(*) FROM participant_sessions ps JOIN conference_sessions c ON c.id = ps.conference_id \
          WHERE ps.legacy_participant_id IS NOT NULL AND ($1::uuid IS NULL OR c.legacy_meeting_id = $1::uuid)",
        filter,
    )
    .await?;

    Ok(BackfillReport {
        legacy_meeting_rows,
        meeting_space_rows,
        mapped_spaces,
        mapping_failures: legacy_meeting_rows - mapped_spaces,
        unmapped_meeting_ids,
        legacy_conference_sessions,
        legacy_participant_rows,
        mapped_legacy_participant_rows,
        conference_participant_rows_from_legacy,
        participant_session_rows_from_legacy,
        unmapped_participant_ids,
        inserted_spaces,
        inserted_conferences,
        inserted_participants,
        inserted_sessions,
        refreshed_rows,
    })
}
