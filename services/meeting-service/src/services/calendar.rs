//! CalendarService — the first-party calendar on top of Meeting Service v2, the single
//! scheduling authority (Blueprint v1.0 Section 10; MTG-005/007/008/010/011/018,
//! ORG-011). No second scheduling store: everything is read from `meeting_spaces`,
//! `conference_sessions`, `meeting_recurrence_rules` and `meeting_recurrence_exceptions`.
//!
//! Rules (see `docs/operations/meetings-calendar-section-10.md`):
//!
//! * **Agenda scope.** A registered caller sees occurrences of spaces it owns, is an
//!   active member of, holds an active email-bound invitation for, is registered for,
//!   or took part in; a machine caller sees its own environment's spaces. With
//!   `organizationId` (verified membership only, per request — never a client-supplied
//!   role) the agenda is restricted to that organization and also lists its
//!   organization-visible meetings (ORGANIZATION_ONLY / PUBLIC / PUBLIC_PASSCODE /
//!   AUTHENTICATED, never HEALTHCARE or HIGH_SECURITY). An unknown and an unauthorized
//!   organization get the same denial.
//! * **Beyond the horizon.** Recurring occurrences later than the materialization
//!   horizon are expanded virtually (bounded: at most `limit + 1` per series and
//!   [`AGENDA_MAX_SERIES`] series per request) under the same `occurrenceKey` the
//!   materializer will use, so materialization never duplicates an entry.
//! * **Effective window.** A start-only occurrence lasts [`DEFAULT_OCCURRENCE_MINUTES`];
//!   an unstarted occurrence is *stale* once `effective end + lateStartGraceMinutes` has
//!   passed. Stale occurrences are expired (CANCELLED by `system:expired`) by the
//!   scheduler and are never the canonical conference.
//! * **Past starts.** A scheduled start more than [`PAST_START_TOLERANCE_MINUTES`] in the
//!   past is rejected on create and reschedule.

use std::collections::{HashMap, HashSet};

use base64::Engine;
use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use sea_orm::{
    AccessMode, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, FromQueryResult, IdenStatic,
    IsolationLevel, Iterable, QueryFilter, QueryOrder, QueryResult, QuerySelect, Statement,
    TransactionTrait, Value,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domain::{
        AdmissionState, ConferenceKind, ConferenceStatus, RecordOrigin, RecurrenceExceptionKind,
        RecurrenceStatus, Role, SecurityProfile,
    },
    dto::v2::{AgendaQuery, AgendaRelation},
    error::ApiResult,
    models::{
        conference_participants, conference_sessions, meeting_invitations,
        meeting_recurrence_exceptions, meeting_recurrence_rules, meeting_spaces,
        webinar_registrations,
    },
    services::{
        actor::ActorContext,
        ics::{self, IcsCalendar, IcsEvent, IcsStatus},
        meeting_core::{
            conference_authority, find_conference, find_space, not_found_conference,
            permission_denied, platform_owns, space_role, state_conflict, validation, Core,
        },
        meeting_events::emit,
        meeting_permissions::{space_allows, PermissionContext, SpacePermission},
        recurrence::{build_rule_set, occurrence_key},
    },
};

pub const AGENDA_DEFAULT_LIMIT: u32 = 50;
pub const AGENDA_MAX_LIMIT: u32 = 200;
pub const AGENDA_DEFAULT_RANGE_DAYS: i64 = 31;
pub const AGENDA_MAX_RANGE_DAYS: i64 = 366;
/// Hard maximum of recurring series expanded beyond the horizon in one request.
pub const AGENDA_MAX_SERIES: usize = 500;
/// Hard maximum of rule iterations spent skipping to the window, per series.
const MAX_RULE_ITERATIONS: usize = 20_000;
/// Duration assumed for an occurrence scheduled without an end.
pub const DEFAULT_OCCURRENCE_MINUTES: i64 = 60;
/// Clock-skew tolerance for "start in the past".
pub const PAST_START_TOLERANCE_MINUTES: i64 = 5;
pub const MAX_OCCURRENCE_HOURS: i64 = 24;
/// One-off occurrences exported in a space calendar (most recent first window).
const ICS_MAX_SINGLE_EVENTS: u64 = 200;
const ICS_HISTORY_DAYS: i64 = 90;
const EXPIRY_BATCH: u64 = 200;

/// The effective end of an occurrence: its scheduled end, else start + default duration.
pub fn effective_end(start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> DateTime<Utc> {
    end.unwrap_or(start + Duration::minutes(DEFAULT_OCCURRENCE_MINUTES))
}

/// Create/reschedule time-window rules shared by every v2 scheduling write.
pub fn validate_schedule(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> ApiResult<()> {
    if end.is_some() && start.is_none() {
        return Err(validation("scheduledEndAt requires scheduledStartAt"));
    }
    if let Some(s) = start {
        if s < now - Duration::minutes(PAST_START_TOLERANCE_MINUTES) {
            return Err(validation("scheduledStartAt must not be in the past"));
        }
        if let Some(e) = end {
            if e <= s {
                return Err(validation("scheduledEndAt must be after scheduledStartAt"));
            }
            if e - s > Duration::hours(MAX_OCCURRENCE_HOURS) {
                return Err(validation(
                    "A conference cannot be scheduled for more than 24 hours",
                ));
            }
        }
    }
    Ok(())
}

/// Schedule details are withheld from callers without authority for healthcare and
/// high-security meetings (MTG-011).
pub fn schedule_restricted(space: &meeting_spaces::Model) -> bool {
    matches!(
        space.security_profile,
        SecurityProfile::Healthcare | SecurityProfile::HighSecurity
    ) || space.meeting_type == crate::domain::MeetingType::Healthcare
        || space.access_policy == crate::domain::AccessPolicy::HealthcareAppointment
}

/// Titles are withheld from callers without the FULL view for these profiles.
pub fn title_restricted(space: &meeting_spaces::Model) -> bool {
    matches!(
        space.security_profile,
        SecurityProfile::Healthcare | SecurityProfile::HighSecurity
    )
}

// ---------------------------------------------------------------------------
// SQL fragments
// ---------------------------------------------------------------------------

/// `lateStartGraceMinutes` of the joined space `s`, mirroring `SchedulePolicy` defaults.
const GRACE_SQL: &str = "make_interval(mins => CASE WHEN jsonb_typeof(s.schedule_policy -> 'lateStartGraceMinutes') = 'number' \
     THEN (s.schedule_policy ->> 'lateStartGraceMinutes')::int ELSE 60 END)";

/// The start window of conference `c` is still open at `$1`.
fn window_open_sql(now_param: &str) -> String {
    format!(
        "(COALESCE(c.scheduled_end_at, c.scheduled_start_at + interval '{DEFAULT_OCCURRENCE_MINUTES} minutes') + {GRACE_SQL} > {now_param})"
    )
}

fn placeholders(from: usize, count: usize) -> String {
    (from..from + count)
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Canonical conference per space: LIVE (v2 first, latest start) > the next unstarted
/// occurrence whose start window is still open (unscheduled ones last) > the latest one.
/// One query for any number of spaces (MTG-010, MTG-018).
pub async fn canonical_conferences<C: ConnectionTrait>(
    conn: &C,
    space_ids: &[Uuid],
    now: DateTime<Utc>,
) -> ApiResult<HashMap<Uuid, conference_sessions::Model>> {
    if space_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = format!(
        r#"SELECT DISTINCT ON (meeting_space_id) * FROM (
             SELECT c.*,
                    CASE WHEN c.status = 'LIVE' THEN 0
                         WHEN c.status IN ('CREATED','PREPARING','WAITING')
                              AND (c.scheduled_start_at IS NULL OR {open}) THEN 1
                         ELSE 2 END AS canonical_bucket
               FROM conference_sessions c JOIN meeting_spaces s ON s.id = c.meeting_space_id
              WHERE c.meeting_space_id IN ({ids})
           ) x
           ORDER BY meeting_space_id, canonical_bucket,
                    CASE WHEN canonical_bucket = 0 AND origin = 'V2' THEN 0 ELSE 1 END,
                    CASE WHEN canonical_bucket = 0 THEN started_at END DESC NULLS LAST,
                    CASE WHEN canonical_bucket = 1 THEN scheduled_start_at END ASC NULLS LAST,
                    created_at DESC, id"#,
        open = window_open_sql("$1"),
        ids = placeholders(2, space_ids.len()),
    );
    let mut values: Vec<Value> = vec![now.into()];
    values.extend(space_ids.iter().map(|id| Value::from(*id)));
    let rows = conference_sessions::Entity::find()
        .from_raw_sql(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(|c| (c.meeting_space_id, c)).collect())
}

pub async fn canonical_conference<C: ConnectionTrait>(
    conn: &C,
    space_id: Uuid,
    now: DateTime<Utc>,
) -> ApiResult<Option<conference_sessions::Model>> {
    Ok(canonical_conferences(conn, &[space_id], now)
        .await?
        .remove(&space_id))
}

/// Expire stale occurrences: unstarted v2 occurrences whose start window closed become
/// CANCELLED (`cancelled_by = system:expired`), one `conference.cancelled` event each.
/// Multi-worker safe (`FOR UPDATE SKIP LOCKED`), bounded per call.
pub async fn expire_stale<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    now: DateTime<Utc>,
) -> ApiResult<u64> {
    let txn = db.begin().await?;
    let sql = format!(
        r#"WITH due AS (
             SELECT c.id FROM conference_sessions c JOIN meeting_spaces s ON s.id = c.meeting_space_id
              WHERE c.status = 'CREATED' AND c.origin = 'V2'
                AND c.scheduled_start_at IS NOT NULL AND c.scheduled_start_at < $1
                AND NOT {open}
              ORDER BY c.scheduled_start_at
              LIMIT {EXPIRY_BATCH}
              FOR UPDATE OF c SKIP LOCKED)
           UPDATE conference_sessions c
              SET status = 'CANCELLED', termination_reason = 'CANCELLED',
                  cancelled_at = $1, cancelled_by = 'system:expired',
                  version = c.version + 1, updated_at = $1
             FROM due WHERE c.id = due.id
           RETURNING c.*"#,
        open = window_open_sql("$1"),
    );
    let expired = conference_sessions::Entity::find()
        .from_raw_sql(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [now.into()],
        ))
        .all(&txn)
        .await?;
    for c in &expired {
        emit(
            &txn,
            crate::services::conference::conference_event(
                c,
                crate::services::meeting_events::event::CONFERENCE_CANCELLED,
            ),
        )
        .await?;
    }
    txn.commit().await?;
    Ok(expired.len() as u64)
}

// ---------------------------------------------------------------------------
// Cursors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AgendaKey {
    #[serde(rename = "s")]
    pub start_micros: i64,
    #[serde(rename = "p")]
    pub space_id: Uuid,
    #[serde(rename = "k")]
    pub occurrence_key: String,
}

impl AgendaKey {
    fn start(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp_micros(self.start_micros).unwrap_or_default()
    }
}

fn encode_cursor<T: Serialize>(value: &T) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(value).unwrap_or_default())
}

fn decode_cursor<T: for<'de> Deserialize<'de>>(cursor: &str) -> ApiResult<T> {
    if cursor.len() > 512 {
        return Err(validation("Invalid cursor"));
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| validation("Invalid cursor"))
}

/// Space-list cursor: newest first by `(created_at, id)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceListCursor {
    #[serde(rename = "c")]
    pub created_micros: i64,
    #[serde(rename = "i")]
    pub id: Uuid,
}

impl SpaceListCursor {
    pub fn of(space: &meeting_spaces::Model) -> String {
        encode_cursor(&SpaceListCursor {
            created_micros: space.created_at.timestamp_micros(),
            id: space.id,
        })
    }
    pub fn parse(cursor: &str) -> ApiResult<Self> {
        decode_cursor(cursor)
    }
    pub fn created_at(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp_micros(self.created_micros).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Agenda
// ---------------------------------------------------------------------------

/// One agenda occurrence, materialized or computed from the series.
#[derive(Debug, Clone)]
pub struct AgendaOccurrence {
    pub conference_id: Option<Uuid>,
    pub occurrence_key: String,
    pub status: ConferenceStatus,
    pub kind: ConferenceKind,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub is_exception: bool,
    pub original_start_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct AgendaEntry {
    pub occurrence: AgendaOccurrence,
    pub space: meeting_spaces::Model,
    pub relations: Vec<AgendaRelation>,
    pub viewer_role: Option<Role>,
    /// The caller manages the space (FULL view).
    pub full: bool,
}

impl AgendaEntry {
    fn key(&self) -> AgendaKey {
        AgendaKey {
            start_micros: self.occurrence.start.timestamp_micros(),
            space_id: self.space.id,
            occurrence_key: self.occurrence.occurrence_key.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgendaPage {
    pub entries: Vec<AgendaEntry>,
    pub next_cursor: Option<String>,
}

/// The caller's relation flags for one space (and conference) row.
struct Relations {
    owner: bool,
    member_rank: i32,
    invitee: bool,
    registrant: bool,
    participant: bool,
    organization: bool,
}

impl Relations {
    fn from_row(row: &QueryResult) -> ApiResult<Self> {
        Ok(Self {
            owner: row.try_get("", "r_owner")?,
            member_rank: row.try_get("", "r_member_rank")?,
            invitee: row.try_get("", "r_invitee")?,
            registrant: row.try_get("", "r_registrant")?,
            participant: row.try_get("", "r_participant")?,
            organization: row.try_get("", "r_org")?,
        })
    }

    fn list(&self) -> Vec<AgendaRelation> {
        let mut out = vec![];
        if self.owner {
            out.push(AgendaRelation::Owner);
        }
        if self.member_rank > 0 {
            out.push(AgendaRelation::Member);
        }
        if self.invitee {
            out.push(AgendaRelation::Invitee);
        }
        if self.registrant {
            out.push(AgendaRelation::Registrant);
        }
        if self.participant {
            out.push(AgendaRelation::Participant);
        }
        if self.organization {
            out.push(AgendaRelation::Organization);
        }
        out
    }

    fn viewer_role(&self) -> Option<Role> {
        if self.owner {
            return Some(Role::Owner);
        }
        Some(match self.member_rank {
            6 => Role::Host,
            5 => Role::CoHost,
            4 => Role::Moderator,
            3 => Role::Presenter,
            2 => Role::Participant,
            1 => Role::Viewer,
            _ => return None,
        })
    }
}

fn member_rank_sql() -> String {
    "CASE mm.role WHEN 'HOST' THEN 6 WHEN 'CO_HOST' THEN 5 WHEN 'MODERATOR' THEN 4 \
     WHEN 'PRESENTER' THEN 3 WHEN 'PARTICIPANT' THEN 2 WHEN 'VIEWER' THEN 1 ELSE 0 END"
        .to_string()
}

/// Shared parameters: $1 user, $2 verified email (lower), $3 organization, $4 revocation
/// verified, $5 now, $6/$7/$8 platform tenant/application/environment.
const CANDIDATES_SQL: &str = r#"cand AS (
    SELECT s.id FROM meeting_spaces s WHERE $1::uuid IS NOT NULL AND s.owner_user_id = $1
    UNION SELECT s.id FROM meeting_spaces s
           WHERE $8::uuid IS NOT NULL AND s.owner_user_id IS NULL AND s.platform_environment_id = $8
             AND s.platform_application_id = $7 AND s.platform_tenant_id = $6
    UNION SELECT mm.meeting_space_id FROM meeting_members mm
           WHERE mm.status = 'ACTIVE' AND (mm.user_id = $1 OR (mm.principal_type = 'EMAIL' AND mm.principal_ref = $2))
    UNION SELECT i.meeting_space_id FROM meeting_invitations i
           WHERE $2::text IS NOT NULL AND i.status = 'ACTIVE' AND i.invitee_email IS NOT NULL
             AND lower(i.invitee_email) = $2 AND i.expires_at > $5
    UNION SELECT r.meeting_space_id FROM webinar_registrations r WHERE r.user_id = $1 AND r.status = 'REGISTERED'
    UNION SELECT cs.meeting_space_id FROM conference_participants p
           JOIN conference_sessions cs ON cs.id = p.conference_id WHERE p.user_id = $1
    UNION SELECT s.id FROM meeting_spaces s WHERE $3::uuid IS NOT NULL AND s.organization_id = $3
)"#;

/// Visible space, strict spaces only with a verified revocation status, organization filter.
const SPACE_OK_SQL: &str = r#"s.status <> 'DELETED' AND NOT (s.origin = 'LEGACY' AND s.legacy_meeting_id IS NULL)
    AND ($4 OR NOT (s.security_profile IN ('HEALTHCARE','HIGH_SECURITY')
                    OR COALESCE(s.access_config ->> 'requireVerifiedRevocation', 'false') = 'true'))
    AND ($3::uuid IS NULL OR s.organization_id = $3)"#;

/// Relation columns for space `s` (and conference `conf`, `NULL::uuid` for a series).
fn relation_columns(conf: &str, participant: &str) -> String {
    format!(
        r#"((($1::uuid IS NOT NULL AND s.owner_user_id = $1)
             OR ($8::uuid IS NOT NULL AND s.owner_user_id IS NULL AND s.platform_environment_id = $8
                 AND s.platform_application_id = $7 AND s.platform_tenant_id = $6))) AS r_owner,
           COALESCE((SELECT max({rank}) FROM meeting_members mm
                      WHERE mm.meeting_space_id = s.id AND mm.status = 'ACTIVE'
                        AND (mm.user_id = $1 OR (mm.principal_type = 'EMAIL' AND mm.principal_ref = $2))), 0)::int4 AS r_member_rank,
           EXISTS (SELECT 1 FROM meeting_invitations i
                    WHERE i.meeting_space_id = s.id AND $2::text IS NOT NULL AND i.status = 'ACTIVE'
                      AND i.invitee_email IS NOT NULL AND lower(i.invitee_email) = $2 AND i.expires_at > $5
                      AND (i.use_limit IS NULL OR i.usage_count < i.use_limit)
                      AND (i.conference_id IS NULL OR i.conference_id = {conf})) AS r_invitee,
           EXISTS (SELECT 1 FROM webinar_registrations r
                    WHERE r.meeting_space_id = s.id AND r.user_id = $1 AND r.status = 'REGISTERED'
                      AND (r.conference_id IS NULL OR r.conference_id = {conf})) AS r_registrant,
           {participant} AS r_participant,
           ($3::uuid IS NOT NULL AND s.organization_id = $3
             AND s.access_policy IN ('ORGANIZATION_ONLY','PUBLIC','PUBLIC_PASSCODE','AUTHENTICATED')
             AND s.security_profile NOT IN ('HEALTHCARE','HIGH_SECURITY')
             AND s.meeting_type <> 'HEALTHCARE') AS r_org"#,
        rank = member_rank_sql(),
    )
}

fn space_columns() -> String {
    meeting_spaces::Column::iter()
        .map(|c| format!("s.{0} AS s_{0}", c.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn relation_filter(relations: &Option<Vec<AgendaRelation>>) -> String {
    let all = "q.r_owner OR q.r_member_rank > 0 OR q.r_invitee OR q.r_registrant OR q.r_participant OR q.r_org";
    let selected = match relations {
        None => return format!("({all})"),
        Some(list) => list
            .iter()
            .map(|r| match r {
                AgendaRelation::Owner => "q.r_owner",
                AgendaRelation::Member => "q.r_member_rank > 0",
                AgendaRelation::Invitee => "q.r_invitee",
                AgendaRelation::Registrant => "q.r_registrant",
                AgendaRelation::Participant => "q.r_participant",
                AgendaRelation::Organization => "q.r_org",
            })
            .collect::<Vec<_>>()
            .join(" OR "),
    };
    format!("({selected})")
}

struct AgendaFilter {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    statuses: Option<Vec<ConferenceStatus>>,
    relations: Option<Vec<AgendaRelation>>,
    organization_id: Option<Uuid>,
    cursor: Option<AgendaKey>,
    limit: usize,
}

fn parse_list<T>(
    raw: &Option<String>,
    parse: impl Fn(&str) -> Option<T>,
    name: &str,
) -> ApiResult<Option<Vec<T>>>
where
    T: PartialEq,
{
    let Some(raw) = raw.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let mut out = vec![];
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let v = parse(part).ok_or_else(|| validation(format!("Unknown {name} value {part}")))?;
        if !out.contains(&v) {
            out.push(v);
        }
    }
    if out.is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

impl AgendaFilter {
    fn from_query(q: &AgendaQuery, now: DateTime<Utc>) -> ApiResult<Self> {
        let from = q.from.unwrap_or(now);
        let to =
            q.to.unwrap_or(from + Duration::days(AGENDA_DEFAULT_RANGE_DAYS));
        if to <= from {
            return Err(validation("to must be after from"));
        }
        if to - from > Duration::days(AGENDA_MAX_RANGE_DAYS) {
            return Err(validation("The agenda range is limited to 366 days"));
        }
        let limit = q.limit.unwrap_or(AGENDA_DEFAULT_LIMIT);
        if !(1..=AGENDA_MAX_LIMIT).contains(&limit) {
            return Err(validation("limit must be between 1 and 200"));
        }
        Ok(Self {
            from,
            to,
            statuses: parse_list(&q.status, ConferenceStatus::parse, "status")?,
            relations: parse_list(&q.role, AgendaRelation::parse, "role")?,
            organization_id: q.organization_id,
            cursor: q.cursor.as_deref().map(decode_cursor).transpose()?,
            limit: limit as usize,
        })
    }

    fn status_allowed(&self, s: ConferenceStatus) -> bool {
        self.statuses
            .as_ref()
            .map(|l| l.contains(&s))
            .unwrap_or(true)
    }

    fn after_cursor(&self, key: &AgendaKey) -> bool {
        self.cursor.as_ref().map(|c| key > c).unwrap_or(true)
    }
}

pub struct CalendarService {
    core: Core,
}

/// What a caller may see in an exported calendar.
struct CalendarAccess {
    full: bool,
}

impl CalendarService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    fn base_values(&self, ctx: &ActorContext, org: Option<Uuid>, now: DateTime<Utc>) -> Vec<Value> {
        let registered = ctx.registered();
        let platform = ctx.platform();
        vec![
            registered.map(|r| r.user_id).into(),
            registered
                .and_then(|r| r.verified_email.as_ref())
                .map(|e| e.to_ascii_lowercase())
                .into(),
            org.into(),
            registered
                .map(|r| r.revocation_checked)
                .unwrap_or(true)
                .into(),
            now.into(),
            platform.map(|p| p.tenant_id).into(),
            platform.map(|p| p.application_id).into(),
            platform.map(|p| p.environment_id).into(),
        ]
    }

    /// `GET /api/v2/me/conferences` — one consistent snapshot, constant query count.
    pub async fn agenda(&self, ctx: &ActorContext, query: AgendaQuery) -> ApiResult<AgendaPage> {
        if ctx.platform().is_none() {
            ctx.require_registered()?;
        }
        let now = Utc::now();
        let filter = AgendaFilter::from_query(&query, now)?;
        if let Some(org) = filter.organization_id {
            // Per-request, server-verified membership; unknown and unauthorized ids get
            // the same answer (ADR 0056 §5).
            if !ctx.verified_organizations.contains(&org) {
                return Err(permission_denied(
                    "You are not a verified member of that organization",
                ));
            }
        }
        let txn = self
            .core
            .db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await?;
        let mut entries = self.materialized(&txn, ctx, &filter, now).await?;
        entries.extend(self.beyond_horizon(&txn, ctx, &filter, now).await?);
        txn.commit().await?;

        entries.sort_by_key(AgendaEntry::key);
        let mut seen = HashSet::new();
        entries.retain(|e| seen.insert((e.space.id, e.occurrence.occurrence_key.clone())));
        let has_more = entries.len() > filter.limit;
        entries.truncate(filter.limit);
        let next_cursor = has_more
            .then(|| entries.last().map(|e| encode_cursor(&e.key())))
            .flatten();
        Ok(AgendaPage {
            entries,
            next_cursor,
        })
    }

    fn entry(
        space: meeting_spaces::Model,
        rel: &Relations,
        occurrence: AgendaOccurrence,
    ) -> AgendaEntry {
        let viewer_role = rel.viewer_role();
        let pctx = PermissionContext::for_space(&space);
        let full = viewer_role
            .map(|r| space_allows(r, SpacePermission::ViewMembers, &pctx))
            .unwrap_or(false);
        AgendaEntry {
            occurrence,
            relations: rel.list(),
            viewer_role,
            full,
            space,
        }
    }

    async fn materialized<C: ConnectionTrait>(
        &self,
        conn: &C,
        ctx: &ActorContext,
        f: &AgendaFilter,
        now: DateTime<Utc>,
    ) -> ApiResult<Vec<AgendaEntry>> {
        let statuses = f
            .statuses
            .as_ref()
            .map(|l| {
                format!(
                    "AND c.status IN ({})",
                    l.iter()
                        .map(|s| format!("'{}'", s.as_str()))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .unwrap_or_default();
        let sql = format!(
            r#"WITH {CANDIDATES_SQL}
               SELECT * FROM (
                 SELECT c.*, {space_cols}, {relations}
                   FROM conference_sessions c JOIN meeting_spaces s ON s.id = c.meeting_space_id
                  WHERE c.meeting_space_id IN (SELECT id FROM cand) AND {SPACE_OK_SQL}
                    AND c.scheduled_start_at IS NOT NULL
                    AND c.scheduled_start_at >= $9 AND c.scheduled_start_at < $10
                    {statuses}
                    AND ($11::timestamptz IS NULL
                         OR (c.scheduled_start_at, c.meeting_space_id, c.occurrence_key COLLATE "C")
                            > ($11::timestamptz, $12::uuid, $13::text COLLATE "C"))
               ) q
               WHERE {access}
               ORDER BY q.scheduled_start_at, q.meeting_space_id, q.occurrence_key COLLATE "C"
               LIMIT {limit}"#,
            space_cols = space_columns(),
            relations = relation_columns(
                "c.id",
                "EXISTS (SELECT 1 FROM conference_participants p WHERE p.conference_id = c.id \
                 AND p.user_id = $1 AND p.admission_state NOT IN ('REMOVED','BANNED','DENIED'))"
            ),
            access = relation_filter(&f.relations),
            limit = f.limit + 1,
        );
        let mut values = self.base_values(ctx, f.organization_id, now);
        values.push(f.from.into());
        values.push(f.to.into());
        values.push(f.cursor.as_ref().map(|c| c.start()).into());
        values.push(f.cursor.as_ref().map(|c| c.space_id).into());
        values.push(f.cursor.as_ref().map(|c| c.occurrence_key.clone()).into());
        let rows = conn
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let c = conference_sessions::Model::from_query_result(&row, "")?;
            let space = meeting_spaces::Model::from_query_result(&row, "s_")?;
            let rel = Relations::from_row(&row)?;
            let Some(start) = c.scheduled_start_at else {
                continue;
            };
            out.push(Self::entry(
                space,
                &rel,
                AgendaOccurrence {
                    conference_id: Some(c.id),
                    occurrence_key: c.occurrence_key.clone(),
                    status: c.status,
                    kind: c.kind,
                    start,
                    end: c.scheduled_end_at,
                    started_at: c.started_at,
                    ended_at: c.ended_at,
                    is_exception: c.is_exception,
                    original_start_at: c.original_start_at,
                },
            ));
        }
        Ok(out)
    }

    /// Recurring occurrences later than each series' materialization horizon (MTG-008).
    async fn beyond_horizon<C: ConnectionTrait>(
        &self,
        conn: &C,
        ctx: &ActorContext,
        f: &AgendaFilter,
        now: DateTime<Utc>,
    ) -> ApiResult<Vec<AgendaEntry>> {
        if !f.status_allowed(ConferenceStatus::Created)
            && !f.status_allowed(ConferenceStatus::Cancelled)
        {
            return Ok(vec![]);
        }
        if f.relations
            .as_ref()
            .map(|l| l == &[AgendaRelation::Participant])
            .unwrap_or(false)
        {
            return Ok(vec![]);
        }
        let sql = format!(
            r#"WITH {CANDIDATES_SQL}
               SELECT * FROM (
                 SELECT rr.*, {space_cols}, {relations}
                   FROM meeting_recurrence_rules rr JOIN meeting_spaces s ON s.id = rr.meeting_space_id
                  WHERE rr.status = 'ACTIVE' AND s.status = 'ACTIVE'
                    AND rr.meeting_space_id IN (SELECT id FROM cand) AND {SPACE_OK_SQL}
               ) q
               WHERE {access}
               ORDER BY q.meeting_space_id
               LIMIT {limit}"#,
            space_cols = space_columns(),
            relations = relation_columns("NULL::uuid", "false"),
            access = relation_filter(&f.relations),
            limit = AGENDA_MAX_SERIES + 1,
        );
        let rows = conn
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                self.base_values(ctx, f.organization_id, now),
            ))
            .await?;
        if rows.len() > AGENDA_MAX_SERIES {
            return Err(validation(
                "Too many recurring series for one agenda request; filter by role or organizationId",
            ));
        }
        let mut series = Vec::with_capacity(rows.len());
        for row in rows {
            series.push((
                meeting_recurrence_rules::Model::from_query_result(&row, "")?,
                meeting_spaces::Model::from_query_result(&row, "s_")?,
                Relations::from_row(&row)?,
            ));
        }
        if series.is_empty() {
            return Ok(vec![]);
        }
        let rule_ids: Vec<Uuid> = series.iter().map(|(r, _, _)| r.id).collect();
        let mut exceptions: HashMap<Uuid, Vec<meeting_recurrence_exceptions::Model>> =
            HashMap::new();
        for e in meeting_recurrence_exceptions::Entity::find()
            .filter(meeting_recurrence_exceptions::Column::RecurrenceRuleId.is_in(rule_ids))
            .all(conn)
            .await?
        {
            exceptions.entry(e.recurrence_rule_id).or_default().push(e);
        }
        let mut out = vec![];
        for (rule, space, rel) in series {
            let exc = exceptions.remove(&rule.id).unwrap_or_default();
            for occurrence in virtual_occurrences(&rule, &exc, f, now) {
                out.push(Self::entry(space.clone(), &rel, occurrence));
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // ICS
    // -----------------------------------------------------------------------

    /// Calendar access to a space (and optionally one conference): management or
    /// membership, an admitted participation, an email-bound invitation or a webinar
    /// registration. `None` = no access.
    async fn calendar_access(
        &self,
        ctx: &ActorContext,
        space: &meeting_spaces::Model,
        conference: Option<&conference_sessions::Model>,
    ) -> ApiResult<Option<CalendarAccess>> {
        let db = &self.core.db;
        if let Some(p) = ctx.platform() {
            return Ok(platform_owns(space, p).then_some(CalendarAccess { full: true }));
        }
        let pctx = PermissionContext::for_space(space);
        if let Some(c) = conference {
            if let Some(a) = conference_authority(db, space, c.id, ctx).await? {
                return Ok(Some(CalendarAccess {
                    full: space_allows(a.role, SpacePermission::ViewMembers, &pctx),
                }));
            }
        }
        let Some(actor) = ctx.registered() else {
            return Ok(None);
        };
        if let Some((role, _)) = space_role(db, space, actor).await? {
            return Ok(Some(CalendarAccess {
                full: space_allows(role, SpacePermission::ViewMembers, &pctx),
            }));
        }
        let conference_id = conference.map(|c| c.id);
        if let Some(email) = actor
            .verified_email
            .as_ref()
            .map(|e| e.to_ascii_lowercase())
        {
            let invitations = meeting_invitations::Entity::find()
                .filter(meeting_invitations::Column::MeetingSpaceId.eq(space.id))
                .filter(
                    meeting_invitations::Column::Status.eq(crate::domain::InvitationStatus::Active),
                )
                .filter(meeting_invitations::Column::ExpiresAt.gt(Utc::now()))
                .filter(meeting_invitations::Column::InviteeEmail.is_not_null())
                .all(db)
                .await?;
            if invitations.iter().any(|i| {
                i.invitee_email
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                    == Some(email.as_str())
                    && i.use_limit.map(|l| i.usage_count < l).unwrap_or(true)
                    && (i.conference_id.is_none() || i.conference_id == conference_id)
            }) {
                return Ok(Some(CalendarAccess { full: false }));
            }
        }
        let registered = webinar_registrations::Entity::find()
            .filter(webinar_registrations::Column::MeetingSpaceId.eq(space.id))
            .filter(webinar_registrations::Column::UserId.eq(actor.user_id))
            .filter(webinar_registrations::Column::Status.eq("REGISTERED"))
            .all(db)
            .await?
            .into_iter()
            .any(|r| r.conference_id.is_none() || r.conference_id == conference_id);
        if registered {
            return Ok(Some(CalendarAccess { full: false }));
        }
        if let Some(c) = conference {
            let took_part = conference_participants::Entity::find()
                .filter(conference_participants::Column::ConferenceId.eq(c.id))
                .filter(conference_participants::Column::UserId.eq(actor.user_id))
                .filter(conference_participants::Column::AdmissionState.is_not_in([
                    AdmissionState::Removed,
                    AdmissionState::Banned,
                    AdmissionState::Denied,
                ]))
                .one(db)
                .await?
                .is_some();
            if took_part {
                return Ok(Some(CalendarAccess { full: false }));
            }
        }
        Ok(None)
    }

    fn summary_and_description(
        space: &meeting_spaces::Model,
        access: &CalendarAccess,
        app_url: &str,
    ) -> (String, String, String) {
        let url = format!("{}/{}", app_url.trim_end_matches('/'), space.meeting_code);
        let summary = if title_restricted(space) && !access.full {
            "CF Meet meeting".to_string()
        } else {
            space.title.clone()
        };
        let description = format!(
            "CF Meet meeting {}\nJoin: {url}\nAccess is checked when you join; this file holds no passcode.",
            space.meeting_code
        );
        (summary, description, url)
    }

    /// `GET /api/v2/conferences/{id}/calendar.ics` — one occurrence.
    pub async fn conference_ics(
        &self,
        ctx: &ActorContext,
        id: Uuid,
        app_url: &str,
    ) -> ApiResult<String> {
        let db = &self.core.db;
        let conference = find_conference(db, id).await?;
        let space = find_space(db, conference.meeting_space_id)
            .await
            .map_err(|_| not_found_conference())?;
        let access = self
            .calendar_access(ctx, &space, Some(&conference))
            .await?
            .ok_or_else(|| permission_denied("You are not permitted to export this conference"))?;
        let start = conference
            .scheduled_start_at
            .ok_or_else(|| state_conflict("This conference has no scheduled time"))?;
        let (summary, description, url) = Self::summary_and_description(&space, &access, app_url);
        let uid = match (conference.recurrence_rule_id, conference.original_start_at) {
            // Stable per occurrence, before and after materialization.
            (Some(rule), Some(original)) => format!(
                "occurrence-{}-{}@cfmeet.com",
                rule.simple(),
                original.format("%Y%m%dT%H%M%SZ")
            ),
            _ => format!("conference-{}@cfmeet.com", conference.id.simple()),
        };
        let event = IcsEvent {
            uid,
            sequence: conference.version - 1,
            summary,
            description: Some(description),
            url: Some(url),
            start,
            end: effective_end(start, conference.scheduled_end_at),
            status: if conference.status == ConferenceStatus::Cancelled {
                IcsStatus::Cancelled
            } else {
                IcsStatus::Confirmed
            },
            rrule: None,
            exdates: vec![],
            recurrence_id: None,
            created: Some(conference.created_at),
            last_modified: conference.updated_at,
        };
        Ok(ics::render(
            &IcsCalendar {
                timezone: space.timezone.parse().ok(),
                name: None,
                events: vec![event],
                timezone_until: None,
            },
            Utc::now(),
        ))
    }

    /// `GET /api/v2/meeting-spaces/{id}/calendar.ics` — the active series (RRULE, EXDATE
    /// for cancelled occurrences, RECURRENCE-ID overrides for rescheduled ones) and the
    /// space's one-off scheduled occurrences.
    pub async fn space_ics(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        app_url: &str,
    ) -> ApiResult<String> {
        let db = &self.core.db;
        let space = find_space(db, space_id).await?;
        let access = self
            .calendar_access(ctx, &space, None)
            .await?
            .ok_or_else(|| permission_denied("You are not permitted to export this meeting"))?;
        let (summary, description, url) = Self::summary_and_description(&space, &access, app_url);
        let now = Utc::now();
        let mut events = vec![];
        let mut timezone: Option<Tz> = space.timezone.parse().ok();
        let mut timezone_until = None;
        let rule = meeting_recurrence_rules::Entity::find()
            .filter(meeting_recurrence_rules::Column::MeetingSpaceId.eq(space.id))
            .filter(meeting_recurrence_rules::Column::Status.eq(RecurrenceStatus::Active))
            .one(db)
            .await?;
        if let Some(rule) = rule {
            let exceptions = meeting_recurrence_exceptions::Entity::find()
                .filter(meeting_recurrence_exceptions::Column::RecurrenceRuleId.eq(rule.id))
                .order_by_asc(meeting_recurrence_exceptions::Column::OriginalStartLocal)
                .all(db)
                .await?;
            if let (Ok(tz), Ok(set)) = (
                rule.timezone.parse::<Tz>(),
                build_rule_set(&rule.rrule, &rule.timezone, rule.dtstart_local),
            ) {
                timezone = Some(tz);
                let until = set
                    .get_rrule()
                    .first()
                    .and_then(|r| r.get_until())
                    .map(|u| u.with_timezone(&Utc));
                timezone_until = until.or(Some(now + Duration::days(366 * 5)));
                if let Some(start) = local_instant(tz, rule.dtstart_local) {
                    let duration = Duration::minutes(rule.duration_minutes as i64);
                    let sequence = series_sequence(&rule, &exceptions);
                    let uid = format!("series-{}@cfmeet.com", rule.id.simple());
                    let mut master = IcsEvent {
                        uid: uid.clone(),
                        sequence,
                        summary: summary.clone(),
                        description: Some(description.clone()),
                        url: Some(url.clone()),
                        start,
                        end: start + duration,
                        status: IcsStatus::Confirmed,
                        rrule: Some(ics::rrule_with_utc_until(&rule.rrule, until)),
                        exdates: vec![],
                        recurrence_id: None,
                        created: Some(rule.created_at),
                        last_modified: rule.updated_at,
                    };
                    let mut overrides = vec![];
                    for e in &exceptions {
                        let Some(original) = local_instant(tz, e.original_start_local) else {
                            continue;
                        };
                        match e.kind {
                            RecurrenceExceptionKind::Cancelled => master.exdates.push(original),
                            RecurrenceExceptionKind::Rescheduled => {
                                let new_start = e.new_start_at.unwrap_or(original);
                                overrides.push(IcsEvent {
                                    uid: uid.clone(),
                                    sequence,
                                    summary: summary.clone(),
                                    description: Some(description.clone()),
                                    url: Some(url.clone()),
                                    start: new_start,
                                    end: e.new_end_at.unwrap_or(new_start + duration),
                                    status: IcsStatus::Confirmed,
                                    rrule: None,
                                    exdates: vec![],
                                    recurrence_id: Some(original),
                                    created: Some(e.created_at),
                                    last_modified: e.updated_at,
                                });
                            }
                        }
                    }
                    events.push(master);
                    events.extend(overrides);
                }
            }
        }
        let singles = conference_sessions::Entity::find()
            .filter(conference_sessions::Column::MeetingSpaceId.eq(space.id))
            .filter(conference_sessions::Column::RecurrenceRuleId.is_null())
            .filter(conference_sessions::Column::Kind.eq(ConferenceKind::Scheduled))
            .filter(conference_sessions::Column::Origin.eq(RecordOrigin::V2))
            .filter(
                conference_sessions::Column::ScheduledStartAt
                    .gte(now - Duration::days(ICS_HISTORY_DAYS)),
            )
            .order_by_asc(conference_sessions::Column::ScheduledStartAt)
            .limit(ICS_MAX_SINGLE_EVENTS)
            .all(db)
            .await?;
        for c in singles {
            let Some(start) = c.scheduled_start_at else {
                continue;
            };
            events.push(IcsEvent {
                uid: format!("conference-{}@cfmeet.com", c.id.simple()),
                sequence: c.version - 1,
                summary: summary.clone(),
                description: Some(description.clone()),
                url: Some(url.clone()),
                start,
                end: effective_end(start, c.scheduled_end_at),
                status: if c.status == ConferenceStatus::Cancelled {
                    IcsStatus::Cancelled
                } else {
                    IcsStatus::Confirmed
                },
                rrule: None,
                exdates: vec![],
                recurrence_id: None,
                created: Some(c.created_at),
                last_modified: c.updated_at,
            });
        }
        let name = (!title_restricted(&space) || access.full).then(|| space.title.clone());
        Ok(ics::render(
            &IcsCalendar {
                timezone,
                name,
                events,
                timezone_until,
            },
            now,
        ))
    }
}

/// The UTC instant of a series wall-clock time (earliest on a fall-back overlap; the
/// later instant after a spring-forward gap, as the recurrence engine does).
pub fn local_instant(tz: Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&local) {
        chrono::LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(a, _) => Some(a.with_timezone(&Utc)),
        chrono::LocalResult::None => {
            let midnight = tz
                .from_local_datetime(&local.date().and_hms_opt(0, 0, 0)?)
                .earliest()?;
            Some((midnight + (local - local.date().and_hms_opt(0, 0, 0)?)).with_timezone(&Utc))
        }
    }
}

/// SEQUENCE of a series: increases with every rule change (version) and every exception
/// change (exception versions). Series edits that drop exceptions advance the rule
/// version by the dropped versions, so the value never decreases.
pub fn series_sequence(
    rule: &meeting_recurrence_rules::Model,
    exceptions: &[meeting_recurrence_exceptions::Model],
) -> i32 {
    (rule.version - 1).saturating_add(exceptions.iter().map(|e| e.version).sum::<i32>())
}

/// Unmaterialized occurrences of one series inside the agenda window, after the
/// cursor, at most `limit + 1` (exceptions applied; rescheduled ones at their new time).
fn virtual_occurrences(
    rule: &meeting_recurrence_rules::Model,
    exceptions: &[meeting_recurrence_exceptions::Model],
    f: &AgendaFilter,
    now: DateTime<Utc>,
) -> Vec<AgendaOccurrence> {
    let Ok(tz) = rule.timezone.parse::<Tz>() else {
        return vec![];
    };
    let Ok(set) = build_rule_set(&rule.rrule, &rule.timezone, rule.dtstart_local) else {
        return vec![];
    };
    let duration = Duration::minutes(rule.duration_minutes as i64);
    // Materialized: originals in [materialization time, materialized_until). Earlier
    // unmaterialized originals are in the past and never happened.
    let unmaterialized_from = rule.materialized_until.unwrap_or(now).max(now);
    let by_local: HashMap<NaiveDateTime, &meeting_recurrence_exceptions::Model> = exceptions
        .iter()
        .map(|e| (e.original_start_local, e))
        .collect();
    let cap = f.limit + 1;
    let key_of = |start: DateTime<Utc>, key: String| AgendaKey {
        start_micros: start.timestamp_micros(),
        space_id: rule.meeting_space_id,
        occurrence_key: key,
    };
    let mut out = vec![];

    let mut lo = f.from.max(unmaterialized_from);
    if let Some(c) = &f.cursor {
        lo = lo.max(c.start());
    }
    let mut iterations = 0usize;
    for occ in set.limit().into_iter() {
        iterations += 1;
        if iterations > MAX_RULE_ITERATIONS || out.len() >= cap {
            break;
        }
        let original = occ.with_timezone(&Utc);
        if original < lo {
            continue;
        }
        if original >= f.to {
            break;
        }
        let exception = by_local.get(&original.with_timezone(&tz).naive_local());
        let (status, is_exception) = match exception.map(|e| e.kind) {
            Some(RecurrenceExceptionKind::Rescheduled) => continue,
            Some(RecurrenceExceptionKind::Cancelled) => (ConferenceStatus::Cancelled, true),
            None => (ConferenceStatus::Created, false),
        };
        if !f.status_allowed(status) {
            continue;
        }
        let key = occurrence_key(rule.id, original);
        if !f.after_cursor(&key_of(original, key.clone())) {
            continue;
        }
        out.push(AgendaOccurrence {
            conference_id: None,
            occurrence_key: key,
            status,
            kind: ConferenceKind::Recurring,
            start: original,
            end: Some(original + duration),
            started_at: None,
            ended_at: None,
            is_exception,
            original_start_at: Some(original),
        });
    }

    if f.status_allowed(ConferenceStatus::Created) {
        for e in exceptions {
            if e.kind != RecurrenceExceptionKind::Rescheduled {
                continue;
            }
            let (Some(original), Some(new_start)) =
                (local_instant(tz, e.original_start_local), e.new_start_at)
            else {
                continue;
            };
            if original < unmaterialized_from || new_start < f.from || new_start >= f.to {
                continue;
            }
            let key = occurrence_key(rule.id, original);
            if !f.after_cursor(&key_of(new_start, key.clone())) {
                continue;
            }
            out.push(AgendaOccurrence {
                conference_id: None,
                occurrence_key: key,
                status: ConferenceStatus::Created,
                kind: ConferenceKind::Recurring,
                start: new_start,
                end: Some(e.new_end_at.unwrap_or(new_start + duration)),
                started_at: None,
                ended_at: None,
                is_exception: true,
                original_start_at: Some(original),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn rule(
        rrule: &str,
        tz: &str,
        start: NaiveDateTime,
        mu: Option<DateTime<Utc>>,
    ) -> meeting_recurrence_rules::Model {
        let now = Utc::now();
        meeting_recurrence_rules::Model {
            id: Uuid::new_v4(),
            meeting_space_id: Uuid::new_v4(),
            rrule: rrule.into(),
            timezone: tz.into(),
            dtstart_local: start,
            duration_minutes: 30,
            status: RecurrenceStatus::Active,
            materialized_until: mu,
            created_by: None,
            created_at: now,
            updated_at: now,
            ended_at: None,
            version: 1,
        }
    }

    fn filter(from: DateTime<Utc>, to: DateTime<Utc>, limit: usize) -> AgendaFilter {
        AgendaFilter {
            from,
            to,
            statuses: None,
            relations: None,
            organization_id: None,
            cursor: None,
            limit,
        }
    }

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    }

    #[test]
    fn schedule_validation_rules() {
        let now = Utc::now();
        assert!(validate_schedule(None, None, now).is_ok());
        assert!(validate_schedule(Some(now - Duration::minutes(4)), None, now).is_ok());
        assert!(validate_schedule(Some(now - Duration::minutes(6)), None, now).is_err());
        assert!(validate_schedule(Some(now), Some(now), now).is_err());
        assert!(validate_schedule(Some(now), Some(now + Duration::hours(25)), now).is_err());
        assert!(validate_schedule(None, Some(now), now).is_err());
        assert!(validate_schedule(Some(now), Some(now + Duration::hours(24)), now).is_ok());
    }

    #[test]
    fn virtual_expansion_starts_at_the_horizon_and_is_bounded() {
        let now = Utc::now();
        let start = (now - Duration::days(10))
            .date_naive()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let mu = now + Duration::days(30);
        let r = rule("FREQ=DAILY", "Africa/Lagos", start, Some(mu));
        let f = filter(now, now + Duration::days(90), 20);
        let v = virtual_occurrences(&r, &[], &f, now);
        assert_eq!(v.len(), 21, "limit + 1 per series");
        assert!(v.iter().all(|o| o.start >= mu && o.conference_id.is_none()));
        assert!(v.windows(2).all(|w| w[0].start < w[1].start));
        // Keys are the materializer's keys.
        assert_eq!(v[0].occurrence_key, occurrence_key(r.id, v[0].start));
    }

    #[test]
    fn virtual_expansion_applies_exceptions_and_cursor() {
        let now = Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap();
        let r = rule("FREQ=DAILY", "Europe/Rome", local(2026, 12, 1, 9, 0), None);
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let day = |d| local_instant(tz, local(2026, 12, d, 9, 0)).unwrap();
        let ex = |d, kind, new: Option<DateTime<Utc>>| meeting_recurrence_exceptions::Model {
            id: Uuid::new_v4(),
            recurrence_rule_id: r.id,
            meeting_space_id: r.meeting_space_id,
            original_start_local: local(2026, 12, d, 9, 0),
            kind,
            new_start_at: new,
            new_end_at: None,
            created_by: None,
            created_at: now,
            updated_at: now,
            version: 1,
        };
        let moved_to = Utc.with_ymd_and_hms(2026, 12, 20, 15, 0, 0).unwrap();
        let exc = vec![
            ex(2, RecurrenceExceptionKind::Cancelled, None),
            ex(3, RecurrenceExceptionKind::Rescheduled, Some(moved_to)),
        ];
        let from = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
        let mut f = filter(from, from + Duration::days(5), 50);
        let v = virtual_occurrences(&r, &exc, &f, now);
        assert_eq!(
            v.len(),
            4,
            "1, 2 (cancelled), 4, 5; the 3rd moved out of range"
        );
        assert_eq!(v[1].status, ConferenceStatus::Cancelled);
        assert!(v.iter().all(|o| o.start != day(3)));
        f.to = from + Duration::days(25);
        let v = virtual_occurrences(&r, &exc, &f, now);
        let moved = v.iter().find(|o| o.start == moved_to).unwrap();
        assert_eq!(moved.original_start_at, Some(day(3)));
        assert!(moved.is_exception);
        f.cursor = Some(AgendaKey {
            start_micros: day(4).timestamp_micros(),
            space_id: r.meeting_space_id,
            occurrence_key: occurrence_key(r.id, day(4)),
        });
        let v = virtual_occurrences(&r, &exc, &f, now);
        assert!(v.iter().all(|o| o.start > day(4)));
    }

    #[test]
    fn spring_forward_and_fall_back_follow_the_recurrence_engine() {
        // America/New_York 2027: spring forward 14 Mar 02:00 → 03:00, fall back 7 Nov.
        let tz: Tz = "America/New_York".parse().unwrap();
        let set = build_rule_set(
            "FREQ=DAILY;COUNT=3",
            "America/New_York",
            local(2027, 3, 13, 2, 30),
        )
        .unwrap();
        let occ: Vec<DateTime<Utc>> = set
            .all(10)
            .dates
            .into_iter()
            .map(|d| d.with_timezone(&Utc))
            .collect();
        assert_eq!(
            occ[1],
            Utc.with_ymd_and_hms(2027, 3, 14, 7, 30, 0).unwrap(),
            "02:30 does not exist: 03:30 EDT"
        );
        assert_eq!(local_instant(tz, local(2027, 3, 14, 2, 30)), Some(occ[1]));
        let set = build_rule_set(
            "FREQ=DAILY;COUNT=3",
            "America/New_York",
            local(2027, 11, 6, 1, 30),
        )
        .unwrap();
        let occ: Vec<DateTime<Utc>> = set
            .all(10)
            .dates
            .into_iter()
            .map(|d| d.with_timezone(&Utc))
            .collect();
        assert_eq!(
            occ[1],
            Utc.with_ymd_and_hms(2027, 11, 7, 5, 30, 0).unwrap(),
            "repeated 01:30: the first (EDT)"
        );
        assert_eq!(local_instant(tz, local(2027, 11, 7, 1, 30)), Some(occ[1]));
        // A series cannot start at a nonexistent or ambiguous local time.
        assert!(
            build_rule_set("FREQ=DAILY", "America/New_York", local(2027, 3, 14, 2, 30)).is_err()
        );
        assert!(
            build_rule_set("FREQ=DAILY", "America/New_York", local(2027, 11, 7, 1, 30)).is_err()
        );
    }

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let k = AgendaKey {
            start_micros: 1,
            space_id: Uuid::new_v4(),
            occurrence_key: "single:x".into(),
        };
        assert_eq!(decode_cursor::<AgendaKey>(&encode_cursor(&k)).unwrap(), k);
        assert!(decode_cursor::<AgendaKey>("not a cursor").is_err());
        assert!(decode_cursor::<AgendaKey>(&"a".repeat(600)).is_err());
    }
}
