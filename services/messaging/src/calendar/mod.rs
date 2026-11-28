//! Events (ADR 0071): personal and conversation events with RSVP, co-organizers,
//! invitations, per-person reminders, server-side recurrence, ICS export and a link to a
//! CF Meet meeting (Messaging never schedules meetings itself).
//!
//! * One gate, [`visible`]: the organizer; for a PERSONAL event the people on it (no
//!   Messaging block with the organizer); for a CONVERSATION event the conversation's ACTIVE
//!   members; for a COMMUNITY event the community's ACTIVE members (asked of
//!   [`crate::communities`]), all decided live. Anything else is `404 NOT_FOUND`.
//! * [`recurrence`]: the RRULE subset, expanded in the event's IANA zone.
//! * [`ics`]: RFC 5545 export.
//! * [`notices`]: invitation, cancellation and reminder notices for the Notification
//!   Service (off by default) and the reminder worker.
//!
//! The module is named `calendar` because `crate::events` is Messaging's domain-event
//! outbox. Event text is stored server-side; it is NOT end-to-end encrypted.

pub mod http;
pub mod ics;
pub mod notices;
pub mod recurrence;

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, NaiveTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use self::recurrence::{Recurrence, Series};
use crate::{
    communities,
    conversations::{access, service::rfc3339},
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    social::{self, media},
    state::AppState,
};

pub const MAX_TITLE: usize = 200;
pub const MAX_DESCRIPTION: usize = 5000;
pub const MAX_LOCATION: usize = 500;
pub const MAX_CANCEL_REASON: usize = 500;
pub const MAX_CO_ORGANIZERS: usize = 10;
pub const MAX_INVITEES: usize = 500;
pub const MAX_INVITES_PER_REQUEST: usize = 100;
pub const MAX_REMINDERS: usize = 5;
pub const MAX_REMINDER_MINUTES: i32 = 40_320;
pub const MAX_SPAN_DAYS: i64 = 31;
pub const MAX_AHEAD_DAYS: i64 = 5 * 366;
pub const MAX_RANGE_DAYS: i64 = 92;
pub const MAX_ORGANIZED_OPEN: i64 = 500;
pub const CREATES_PER_HOUR: i64 = 60;
pub const WRITES_PER_MINUTE: i64 = 120;
pub const READS_PER_MINUTE: i64 = 120;
/// Candidate events a list examines at most.
pub const MAX_CANDIDATES: i64 = 2000;
/// Upcoming occurrences shown on an event's page.
pub const UPCOMING_SHOWN: usize = 10;
/// The calendar file covers events ended at most this long ago.
pub const FEED_PAST_DAYS: i64 = 90;
pub const FEED_MAX_EVENTS: i64 = 500;

pub fn not_found() -> Error {
    Error::code(ErrorCode::NotFound)
}

fn refuse(code: ErrorCode, reason: &'static str, message: &str) -> Error {
    Error::Code {
        code,
        reason: Some(reason),
        message: Some(message.to_string()),
    }
}

fn not_organizer() -> Error {
    refuse(
        ErrorCode::PrincipalNotAuthorized,
        "NOT_EVENT_ORGANIZER",
        "Only the event's organizers can do this",
    )
}

// ----------------------------------------------------------------------------- rows

#[derive(Debug, Clone, FromQueryResult)]
pub struct EventRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub organizer_principal_id: Uuid,
    pub scope: String,
    pub conversation_id: Option<Uuid>,
    pub community_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub kind: String,
    pub meeting_id: Option<String>,
    pub meeting_url: Option<String>,
    pub cover_media_id: Option<Uuid>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub time_zone: String,
    pub all_day: bool,
    pub recurrence: Option<Value>,
    pub series_ends_at: Option<DateTime<Utc>>,
    pub state: String,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const COLUMNS: &str = "e.id, e.tenant_id, e.application_id, e.environment_id, e.organizer_principal_id, e.scope, \
    e.conversation_id, e.community_id, e.title, e.description, e.location, e.kind, e.meeting_id, e.meeting_url, e.cover_media_id, \
    e.starts_at, e.ends_at, e.time_zone, e.all_day, e.recurrence, e.series_ends_at, e.state, e.cancelled_at, \
    e.cancel_reason, e.version, e.created_at, e.updated_at";

impl EventRow {
    pub fn active(&self) -> bool {
        self.state == "ACTIVE"
    }

    pub fn rule(&self) -> Option<Recurrence> {
        self.recurrence
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn series(&self) -> Series {
        Series {
            start: self.starts_at,
            end: self.ends_at,
            tz: recurrence::zone(&self.time_zone).unwrap_or(chrono_tz::UTC),
            rule: self.rule(),
        }
    }

    pub fn ended(&self, now: DateTime<Utc>) -> bool {
        self.series_ends_at.is_some_and(|e| e <= now)
    }
}

pub async fn load<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<Option<EventRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_events e WHERE e.id = $1 AND e.environment_id = $2{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![id.into(), environment_id.into()],
    )
    .await?)
}

#[derive(Debug, Clone, FromQueryResult)]
struct PersonRow {
    principal_id: Uuid,
    role: String,
    response: Option<String>,
    responded_at: Option<DateTime<Utc>>,
}

async fn person<C: ConnectionTrait>(db: &C, event: Uuid, who: Uuid) -> Result<Option<PersonRow>> {
    Ok(db::one(
        db,
        "SELECT principal_id, role, response, responded_at FROM messaging_event_people WHERE event_id = $1 AND principal_id = $2",
        vec![event.into(), who.into()],
    )
    .await?)
}

/// The caller's relation to an event they may see.
#[derive(Debug, Clone)]
pub struct Access {
    pub role: Option<String>,
    pub response: Option<String>,
}

impl Access {
    pub fn organizer(&self) -> bool {
        self.role.as_deref() == Some("ORGANIZER")
    }
    pub fn may_edit(&self) -> bool {
        matches!(self.role.as_deref(), Some("ORGANIZER" | "CO_ORGANIZER"))
    }
}

/// Whether a Messaging block exists either way between `a` and `b`.
async fn blocked<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    a: Uuid,
    b: Uuid,
) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_blocks WHERE environment_id = $1 AND \
           ((blocker_principal_id = $2 AND blocked_principal_id = $3) OR (blocker_principal_id = $3 AND blocked_principal_id = $2))",
        vec![environment_id.into(), a.into(), b.into()],
    )
    .await?
        > 0)
}

async fn active_member<C: ConnectionTrait>(db: &C, conversation: Uuid, who: Uuid) -> Result<bool> {
    Ok(access::membership(db, conversation, who, false)
        .await?
        .is_some_and(|m| m.is_active()))
}

/// THE gate: the event and the caller's relation to it, or `404`.
pub async fn gate<C: ConnectionTrait>(db: &C, row: &EventRow, me: Uuid) -> Result<Option<Access>> {
    let p = person(db, row.id, me).await?;
    let access = Access {
        role: if row.organizer_principal_id == me {
            Some("ORGANIZER".into())
        } else {
            p.as_ref().map(|p| p.role.clone())
        },
        response: p.as_ref().and_then(|p| p.response.clone()),
    };
    if row.organizer_principal_id == me {
        return Ok(Some(access));
    }
    let ok = match row.scope.as_str() {
        "PERSONAL" => {
            p.is_some() && !blocked(db, row.environment_id, me, row.organizer_principal_id).await?
        }
        "CONVERSATION" => match row.conversation_id {
            Some(c) => active_member(db, c, me).await?,
            None => false,
        },
        "COMMUNITY" => match row.community_id {
            Some(c) => communities::is_active_member(db, c, me).await?,
            None => false,
        },
        _ => false,
    };
    Ok(ok.then_some(access))
}

pub async fn visible<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    me: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<(EventRow, Access)> {
    let row = load(db, ctx.scope.environment_id, id, lock)
        .await?
        .ok_or_else(not_found)?;
    let access = gate(db, &row, me).await?.ok_or_else(not_found)?;
    Ok((row, access))
}

/// The SQL condition "`$me` may see event `e`" (`env` / `me` are parameter numbers).
fn visible_sql(env: usize, me: usize) -> String {
    format!(
        "e.environment_id = ${env} AND (e.organizer_principal_id = ${me} \
         OR (e.scope = 'PERSONAL' AND EXISTS (SELECT 1 FROM messaging_event_people vp WHERE vp.event_id = e.id AND vp.principal_id = ${me}) \
             AND NOT EXISTS (SELECT 1 FROM messaging_blocks vb WHERE vb.environment_id = ${env} AND \
                 ((vb.blocker_principal_id = ${me} AND vb.blocked_principal_id = e.organizer_principal_id) \
               OR (vb.blocked_principal_id = ${me} AND vb.blocker_principal_id = e.organizer_principal_id)))) \
         OR (e.scope = 'CONVERSATION' AND EXISTS (SELECT 1 FROM messaging_conversation_members vm \
             WHERE vm.conversation_id = e.conversation_id AND vm.principal_id = ${me} AND vm.state = 'ACTIVE')) \
         OR (e.scope = 'COMMUNITY' AND {community}))",
        community = communities::active_member_sql("e.community_id", &format!("${me}"))
    )
}

/// Media `id` is the cover of an event `viewer` may see.
pub async fn cover_readable(
    state: &AppState,
    environment_id: Uuid,
    viewer: Uuid,
    id: Uuid,
) -> Result<bool> {
    let rows: Vec<EventRow> = db::all(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_events e WHERE e.environment_id = $1 AND e.cover_media_id = $2 LIMIT 50"
        ),
        vec![environment_id.into(), id.into()],
    )
    .await?;
    for row in rows {
        if gate(&state.db, &row, viewer).await?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

// --------------------------------------------------------------------------- limits

async fn limit(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    what: &str,
    window: Window,
    max: i64,
) -> Result<()> {
    let key = format!(
        "events-{what}:{}:{}",
        ctx.scope.environment_id.simple(),
        me.simple()
    );
    if let Some(retry) = limits::hit(&state.db, &key, window, max, state.now()).await? {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    Ok(())
}

pub async fn read_limit(state: &AppState, ctx: &TrustedContext, me: Uuid) -> Result<()> {
    limit(state, ctx, me, "read", Window::Minute, READS_PER_MINUTE).await
}

pub async fn write_limit(state: &AppState, ctx: &TrustedContext, me: Uuid) -> Result<()> {
    limit(state, ctx, me, "write", Window::Minute, WRITES_PER_MINUTE).await
}

// ----------------------------------------------------------------------- validation

/// Event details as stored (validated).
#[derive(Debug, Clone)]
struct Details {
    title: String,
    description: Option<String>,
    location: Option<String>,
    kind: String,
    meeting_id: Option<String>,
    meeting_url: Option<String>,
    cover_media_id: Option<Uuid>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    time_zone: String,
    all_day: bool,
    recurrence: Option<Recurrence>,
    series_ends_at: Option<DateTime<Utc>>,
}

/// Details as given (before validation).
#[derive(Debug, Clone, Default)]
struct Raw {
    title: Option<String>,
    description: Option<String>,
    location: Option<String>,
    kind: Option<String>,
    meeting_id: Option<String>,
    meeting_url: Option<String>,
    cover_media_id: Option<Uuid>,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    time_zone: Option<String>,
    all_day: bool,
    recurrence: Option<Recurrence>,
}

fn valid_meeting_id(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_meeting_url(v: &str) -> bool {
    v.len() > "https://".len()
        && v.len() <= 2048
        && v.starts_with("https://")
        && !v.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn validate(raw: Raw, now: DateTime<Utc>, timing_changed: bool) -> Result<Details> {
    let title = social::clean_text(&raw.title, MAX_TITLE, "The title")?
        .ok_or_else(|| Error::validation("An event needs a title"))?;
    let description = social::clean_text(&raw.description, MAX_DESCRIPTION, "The description")?;
    let location = social::clean_text(&raw.location, MAX_LOCATION, "The location")?;
    let kind = raw.kind.unwrap_or_default();
    if !matches!(kind.as_str(), "ONLINE" | "PHYSICAL" | "CALL") {
        return Err(Error::validation("type must be ONLINE, PHYSICAL or CALL"));
    }
    let meeting_id = raw
        .meeting_id
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if meeting_id.as_deref().is_some_and(|v| !valid_meeting_id(v)) {
        return Err(Error::validation(
            "meetingId must be 1-64 letters, digits, - or _",
        ));
    }
    let meeting_url = raw
        .meeting_url
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if meeting_url
        .as_deref()
        .is_some_and(|v| !valid_meeting_url(v))
    {
        return Err(Error::validation("meetingUrl must be an https link"));
    }
    if kind == "ONLINE" && meeting_id.is_none() && meeting_url.is_none() {
        return Err(Error::validation(
            "An online event links to a CF Meet meeting",
        ));
    }
    let time_zone = raw.time_zone.unwrap_or_else(|| "UTC".into());
    let tz = recurrence::zone(&time_zone).ok_or_else(|| Error::validation("Unknown time zone"))?;
    let starts_at = raw
        .starts_at
        .ok_or_else(|| Error::validation("startsAt is required"))?;
    let ends_at = raw
        .ends_at
        .ok_or_else(|| Error::validation("endsAt is required"))?;
    if ends_at <= starts_at {
        return Err(Error::validation("The event must end after it starts"));
    }
    if ends_at - starts_at > Duration::days(MAX_SPAN_DAYS) {
        return Err(Error::validation("An event can last at most 31 days"));
    }
    if starts_at > now + Duration::days(MAX_AHEAD_DAYS) {
        return Err(Error::validation(
            "An event can start at most 5 years from now",
        ));
    }
    if raw.all_day {
        let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default();
        let local = |t: DateTime<Utc>| t.with_timezone(&tz).naive_local().time();
        if local(starts_at) != midnight || local(ends_at) != midnight {
            return Err(Error::validation(
                "An all-day event starts and ends at midnight in its time zone",
            ));
        }
    }
    let series = Series {
        start: starts_at,
        end: ends_at,
        tz,
        rule: raw.recurrence.clone(),
    };
    series.validate().map_err(Error::validation)?;
    let series_ends_at = series.last_end();
    if timing_changed && series_ends_at.is_some_and(|e| e <= now) {
        return Err(Error::validation("The event would already be over"));
    }
    Ok(Details {
        title,
        description,
        location,
        kind,
        meeting_id,
        meeting_url,
        cover_media_id: raw.cover_media_id,
        starts_at,
        ends_at,
        time_zone,
        all_day: raw.all_day,
        recurrence: raw.recurrence,
        series_ends_at,
    })
}

fn reminders(values: &[i32]) -> Result<Vec<i32>> {
    let mut out: Vec<i32> = values.to_vec();
    out.sort_unstable();
    out.dedup();
    if out.len() > MAX_REMINDERS {
        return Err(Error::validation("At most 5 reminders"));
    }
    if out.iter().any(|m| !(0..=MAX_REMINDER_MINUTES).contains(m)) {
        return Err(Error::validation("A reminder is 0 to 40320 minutes before"));
    }
    Ok(out)
}

fn people(ids: &[Uuid], me: Uuid, max: usize, what: &str) -> Result<Vec<Uuid>> {
    let mut out: Vec<Uuid> = ids.iter().copied().filter(|p| *p != me).collect();
    out.sort();
    out.dedup();
    if out.len() > max {
        return Err(Error::validation(format!("At most {max} {what}")));
    }
    Ok(out)
}

/// Where an event belongs: its conversation (CONVERSATION) or community (COMMUNITY).
#[derive(Debug, Clone, Copy)]
struct Place {
    conversation: Option<Uuid>,
    community: Option<Uuid>,
}

impl Place {
    fn of(row: &EventRow) -> Self {
        Place {
            conversation: row.conversation_id,
            community: row.community_id,
        }
    }
}

/// Everyone in `ids` may be put on an event of `scope` by `inviter`: their mutual,
/// unblocked contacts (PERSONAL), ACTIVE members of the conversation (CONVERSATION) or of
/// the community (COMMUNITY).
async fn require_eligible(
    state: &AppState,
    ctx: &TrustedContext,
    scope: &str,
    place: Place,
    inviter: Uuid,
    ids: &[Uuid],
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let ok = match (scope, place.conversation, place.community) {
        ("COMMUNITY", _, Some(c)) => {
            communities::active_members_among(&state.db, c, ids).await? == ids.len() as i64
        }
        ("CONVERSATION", Some(c), _) => {
            db::scalar_i64(
                &state.db,
                "SELECT count(DISTINCT principal_id) FROM messaging_conversation_members \
                  WHERE conversation_id = $1 AND principal_id = ANY($2) AND state = 'ACTIVE'",
                vec![c.into(), ids.to_vec().into()],
            )
            .await?
                == ids.len() as i64
        }
        ("PERSONAL", _, _) => {
            let contacts = social::eligible_contacts(state, ctx, inviter, Some(ids)).await?;
            let blocks = social::blocked_with(&state.db, ctx.scope.environment_id, inviter).await?;
            ids.iter()
                .all(|p| contacts.contains_key(p) && !blocks.contains(p))
        }
        _ => false,
    };
    if !ok {
        return Err(refuse(
            ErrorCode::ValidationError,
            "INVITEE_NOT_ELIGIBLE",
            match scope {
                "CONVERSATION" => "Only members of this chat can be added",
                "COMMUNITY" => "Only members of this community can be added",
                _ => "Only your contacts can be invited",
            },
        ));
    }
    Ok(())
}

/// The cover must be a completed EVENT upload of `owner`.
async fn require_cover(
    state: &AppState,
    ctx: &TrustedContext,
    owner: Uuid,
    id: Uuid,
) -> Result<()> {
    let m = media::usable(&state.db, ctx, owner, media::Purpose::Event, id).await?;
    if m.kind != "IMAGE" {
        return Err(Error::validation("A cover is a photo"));
    }
    Ok(())
}

// ------------------------------------------------------------------------ reminders

fn next_fire(series: &Series, minutes: i32, not_before: DateTime<Utc>) -> Option<DateTime<Utc>> {
    series
        .occurrences()
        .map(|(s, _)| s - Duration::minutes(i64::from(minutes)))
        .find(|f| *f >= not_before)
}

#[derive(FromQueryResult)]
struct ReminderKey {
    principal_id: Uuid,
    minutes_before: i32,
}

/// Recomputes every reminder of an event (after a change of its time or state).
async fn reschedule<C: ConnectionTrait>(txn: &C, row: &EventRow, now: DateTime<Utc>) -> Result<()> {
    let keys: Vec<ReminderKey> = db::all(
        txn,
        "SELECT principal_id, minutes_before FROM messaging_event_reminders WHERE event_id = $1",
        vec![row.id.into()],
    )
    .await?;
    let series = row.series();
    for k in keys {
        let fire = if row.active() {
            next_fire(&series, k.minutes_before, now)
        } else {
            None
        };
        db::exec(
            txn,
            "UPDATE messaging_event_reminders SET next_fire_at = $4 WHERE event_id = $1 AND principal_id = $2 AND minutes_before = $3",
            vec![row.id.into(), k.principal_id.into(), k.minutes_before.into(), fire.into()],
        )
        .await?;
    }
    Ok(())
}

async fn replace_reminders<C: ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    row: &EventRow,
    who: Uuid,
    minutes: &[i32],
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        txn,
        "DELETE FROM messaging_event_reminders WHERE event_id = $1 AND principal_id = $2",
        vec![row.id.into(), who.into()],
    )
    .await?;
    let series = row.series();
    for m in minutes {
        let fire = if row.active() {
            next_fire(&series, *m, now)
        } else {
            None
        };
        db::exec(
            txn,
            "INSERT INTO messaging_event_reminders (event_id, principal_id, minutes_before, next_fire_at, tenant_id, \
               application_id, environment_id, service_account_id, environment_kind, first_party, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            vec![
                row.id.into(),
                who.into(),
                (*m).into(),
                fire.into(),
                ctx.scope.tenant_id.into(),
                ctx.scope.application_id.into(),
                ctx.scope.environment_id.into(),
                ctx.scope.service_account_id.into(),
                ctx.scope.environment_kind.as_str().into(),
                ctx.first_party.into(),
                now.into(),
            ],
        )
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------- views

#[derive(FromQueryResult)]
struct CountRow {
    event_id: Uuid,
    going: i64,
    maybe: i64,
    not_going: i64,
    awaiting_invited: i64,
    answered: i64,
}

#[derive(FromQueryResult)]
struct MemberCount {
    conversation_id: Uuid,
    members: i64,
}

#[derive(FromQueryResult)]
struct RoleRow {
    event_id: Uuid,
    principal_id: Uuid,
    role: String,
    response: Option<String>,
}

#[derive(FromQueryResult)]
struct MyReminder {
    event_id: Uuid,
    minutes_before: i32,
}

/// Counts, the caller's role / answer / reminders and co-organizers of `rows`, batched.
struct Extras {
    counts: HashMap<Uuid, Value>,
    mine: HashMap<Uuid, (Option<String>, Option<String>)>,
    co: HashMap<Uuid, Vec<Uuid>>,
    reminders: HashMap<Uuid, Vec<i32>>,
}

async fn extras<C: ConnectionTrait>(db: &C, rows: &[&EventRow], me: Uuid) -> Result<Extras> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut out = Extras {
        counts: HashMap::new(),
        mine: HashMap::new(),
        co: HashMap::new(),
        reminders: HashMap::new(),
    };
    if ids.is_empty() {
        return Ok(out);
    }
    let counts: Vec<CountRow> = db::all(
        db,
        &format!(
            "SELECT p.event_id, \
                count(*) FILTER (WHERE p.response = 'GOING') AS going, \
                count(*) FILTER (WHERE p.response = 'MAYBE') AS maybe, \
                count(*) FILTER (WHERE p.response = 'NOT_GOING') AS not_going, \
                count(*) FILTER (WHERE p.response IS NULL AND p.role IN ('INVITEE', 'CO_ORGANIZER')) AS awaiting_invited, \
                count(*) FILTER (WHERE p.response IS NOT NULL) AS answered \
           FROM messaging_event_people p JOIN messaging_events e ON e.id = p.event_id \
          WHERE p.event_id = ANY($1) AND (e.scope NOT IN ('CONVERSATION', 'COMMUNITY') \
                OR (e.scope = 'CONVERSATION' AND EXISTS (SELECT 1 FROM messaging_conversation_members m \
                    WHERE m.conversation_id = e.conversation_id AND m.principal_id = p.principal_id AND m.state = 'ACTIVE')) \
                OR (e.scope = 'COMMUNITY' AND {community})) \
          GROUP BY p.event_id",
            community = communities::active_member_sql("e.community_id", "p.principal_id")
        ),
        vec![ids.clone().into()],
    )
    .await?;
    let conversations: Vec<Uuid> = rows
        .iter()
        .filter(|r| r.scope == "CONVERSATION")
        .filter_map(|r| r.conversation_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let members: HashMap<Uuid, i64> = if conversations.is_empty() {
        HashMap::new()
    } else {
        let v: Vec<MemberCount> = db::all(
            db,
            "SELECT conversation_id, count(*) AS members FROM messaging_conversation_members \
              WHERE conversation_id = ANY($1) AND state = 'ACTIVE' GROUP BY conversation_id",
            vec![conversations.into()],
        )
        .await?;
        v.into_iter()
            .map(|m| (m.conversation_id, m.members))
            .collect()
    };
    let community_ids: Vec<Uuid> = rows
        .iter()
        .filter(|r| r.scope == "COMMUNITY")
        .filter_map(|r| r.community_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let community_members = communities::active_member_counts(db, &community_ids).await?;
    let by_event: HashMap<Uuid, CountRow> = counts.into_iter().map(|c| (c.event_id, c)).collect();
    for r in rows {
        let c = by_event.get(&r.id);
        let (going, maybe, not_going, invited, answered) = c
            .map(|c| {
                (
                    c.going,
                    c.maybe,
                    c.not_going,
                    c.awaiting_invited,
                    c.answered,
                )
            })
            .unwrap_or((0, 0, 0, 0, 0));
        let awaiting = match (r.scope.as_str(), r.conversation_id, r.community_id) {
            ("CONVERSATION", Some(cid), _) => {
                (members.get(&cid).copied().unwrap_or(0) - answered).max(0)
            }
            ("COMMUNITY", _, Some(cid)) => {
                (community_members.get(&cid).copied().unwrap_or(0) - answered).max(0)
            }
            _ => invited,
        };
        out.counts.insert(
            r.id,
            json!({ "going": going, "maybe": maybe, "notGoing": not_going, "awaiting": awaiting }),
        );
    }
    let roles: Vec<RoleRow> = db::all(
        db,
        "SELECT event_id, principal_id, role, response FROM messaging_event_people \
          WHERE event_id = ANY($1) AND (principal_id = $2 OR role = 'CO_ORGANIZER') ORDER BY added_at, principal_id",
        vec![ids.clone().into(), me.into()],
    )
    .await?;
    for r in roles {
        if r.role == "CO_ORGANIZER" {
            out.co.entry(r.event_id).or_default().push(r.principal_id);
        }
        if r.principal_id == me {
            out.mine.insert(r.event_id, (Some(r.role), r.response));
        }
    }
    let mine: Vec<MyReminder> = db::all(
        db,
        "SELECT event_id, minutes_before FROM messaging_event_reminders WHERE event_id = ANY($1) AND principal_id = $2 \
          ORDER BY minutes_before",
        vec![ids.into(), me.into()],
    )
    .await?;
    for m in mine {
        out.reminders
            .entry(m.event_id)
            .or_default()
            .push(m.minutes_before);
    }
    Ok(out)
}

fn occurrence(o: Option<(DateTime<Utc>, DateTime<Utc>)>) -> Value {
    match o {
        Some((s, e)) => json!({ "startsAt": rfc3339(s), "endsAt": rfc3339(e) }),
        None => Value::Null,
    }
}

fn view(row: &EventRow, x: &Extras, me: Uuid, now: DateTime<Utc>) -> Value {
    let (role, response) = if row.organizer_principal_id == me {
        (
            Some("ORGANIZER".to_string()),
            x.mine.get(&row.id).and_then(|m| m.1.clone()),
        )
    } else {
        x.mine.get(&row.id).cloned().unwrap_or((None, None))
    };
    let may_edit = matches!(role.as_deref(), Some("ORGANIZER" | "CO_ORGANIZER"));
    let series = row.series();
    json!({
        "eventId": row.id,
        "scope": row.scope,
        "conversationId": row.conversation_id,
        "communityId": row.community_id,
        "title": row.title,
        "description": row.description,
        "location": row.location,
        "type": row.kind,
        "meetingId": row.meeting_id,
        "meetingUrl": row.meeting_url,
        "coverMediaId": row.cover_media_id,
        "startsAt": rfc3339(row.starts_at),
        "endsAt": rfc3339(row.ends_at),
        "timeZone": row.time_zone,
        "allDay": row.all_day,
        "recurrence": row.rule().map(|r| serde_json::to_value(r).unwrap_or(Value::Null)),
        "rrule": series.rrule(),
        "seriesEndsAt": row.series_ends_at.map(rfc3339),
        "state": row.state,
        "cancelledAt": row.cancelled_at.map(rfc3339),
        "cancelReason": row.cancel_reason,
        "organizerPrincipalId": row.organizer_principal_id,
        "coOrganizerPrincipalIds": x.co.get(&row.id).cloned().unwrap_or_default(),
        "myRole": role,
        "myResponse": response,
        "myReminders": x.reminders.get(&row.id).cloned().unwrap_or_default(),
        "counts": x.counts.get(&row.id).cloned().unwrap_or_else(|| json!({ "going": 0, "maybe": 0, "notGoing": 0, "awaiting": 0 })),
        "canEdit": may_edit && row.active(),
        "canDelete": row.organizer_principal_id == me,
        "version": row.version,
        "createdAt": rfc3339(row.created_at),
        "updatedAt": rfc3339(row.updated_at),
        "nextOccurrence": occurrence(series.next_after(now)),
    })
}

async fn render_one(state: &AppState, row: &EventRow, me: Uuid) -> Result<Value> {
    let now = state.now();
    let x = extras(&state.db, &[row], me).await?;
    let mut v = view(row, &x, me, now);
    let upcoming: Vec<Value> = row
        .series()
        .occurrences()
        .filter(|(_, e)| *e > now)
        .take(UPCOMING_SHOWN)
        .map(|o| occurrence(Some(o)))
        .collect();
    v["upcomingOccurrences"] = Value::Array(upcoming);
    Ok(v)
}

// --------------------------------------------------------------------------- create

/// `POST /v1/messaging/events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEvent {
    pub scope: String,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    #[serde(default)]
    pub community_id: Option<Uuid>,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub meeting_id: Option<String>,
    #[serde(default)]
    pub meeting_url: Option<String>,
    #[serde(default)]
    pub cover_media_id: Option<Uuid>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub recurrence: Option<Recurrence>,
    #[serde(default)]
    pub co_organizer_principal_ids: Vec<Uuid>,
    #[serde(default)]
    pub invitee_principal_ids: Vec<Uuid>,
    #[serde(default)]
    pub reminders_minutes_before: Vec<i32>,
}

async fn replay(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    stored: Value,
) -> Result<(u16, Value)> {
    let id = stored["eventId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok());
    if let Some(id) = id {
        if let Ok((row, _)) = visible(&state.db, ctx, me, id, false).await {
            return Ok((200, render_one(state, &row, me).await?));
        }
    }
    Ok((200, stored))
}

pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: CreateEvent,
    key: Option<String>,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    let env = ctx.scope.environment_id;
    let idem = key.map(|k| {
        social::Idempotent::new(
            "event.create",
            k,
            me,
            &serde_json::to_value(&req).unwrap_or(Value::Null),
        )
    });
    if let Some(idem) = &idem {
        if let Some(stored) = social::replayed(&state.db, env, idem, state.now()).await? {
            return replay(state, ctx, me, stored).await;
        }
    }
    let now = state.now();
    match req.scope.as_str() {
        "PERSONAL" => {
            if req.conversation_id.is_some() || req.community_id.is_some() {
                return Err(Error::validation(
                    "A personal event has no conversationId or communityId",
                ));
            }
        }
        "CONVERSATION" => {
            if req.community_id.is_some() {
                return Err(Error::validation("A chat event has no communityId"));
            }
            let cid = req
                .conversation_id
                .ok_or_else(|| Error::validation("A chat event needs its conversationId"))?;
            let (_, m) = access::as_member(&state.db, ctx, cid, me).await?;
            if !m.may_send(now) {
                return Err(Error::code(ErrorCode::SendRestricted));
            }
        }
        "COMMUNITY" => {
            if req.conversation_id.is_some() {
                return Err(Error::validation("A community event has no conversationId"));
            }
            let cid = req
                .community_id
                .ok_or_else(|| Error::validation("A community event needs its communityId"))?;
            // Members only (404 for anyone else); the community's owner and admins
            // organize its events, as they post its Announcements.
            let (_, m) = communities::as_member(&state.db, ctx, cid, me, false).await?;
            if !m.is_admin() {
                return Err(refuse(
                    ErrorCode::PrincipalNotAuthorized,
                    "NOT_COMMUNITY_ADMIN",
                    "Only community admins create community events",
                ));
            }
        }
        "ORGANIZATION" => {
            return Err(refuse(
                ErrorCode::ContextNotAllowed,
                "EVENT_SCOPE_NOT_AVAILABLE",
                "Events for organizations aren't available yet",
            ));
        }
        _ => {
            return Err(Error::validation(
                "scope must be PERSONAL, CONVERSATION or COMMUNITY",
            ))
        }
    }
    limit(state, ctx, me, "create", Window::Hour, CREATES_PER_HOUR).await?;
    let open = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_events WHERE environment_id = $1 AND organizer_principal_id = $2 \
           AND (series_ends_at IS NULL OR series_ends_at > $3)",
        vec![env.into(), me.into(), now.into()],
    )
    .await?;
    if open >= MAX_ORGANIZED_OPEN {
        return Err(Error::reason(ErrorCode::QuotaExceeded, "TOO_MANY_EVENTS"));
    }
    let d = validate(
        Raw {
            title: Some(req.title.clone()),
            description: req.description.clone(),
            location: req.location.clone(),
            kind: Some(req.kind.clone()),
            meeting_id: req.meeting_id.clone(),
            meeting_url: req.meeting_url.clone(),
            cover_media_id: req.cover_media_id,
            starts_at: Some(req.starts_at),
            ends_at: Some(req.ends_at),
            time_zone: req.time_zone.clone(),
            all_day: req.all_day,
            recurrence: req.recurrence.clone(),
        },
        now,
        true,
    )?;
    if let Some(c) = d.cover_media_id {
        require_cover(state, ctx, me, c).await?;
    }
    let co = people(
        &req.co_organizer_principal_ids,
        me,
        MAX_CO_ORGANIZERS,
        "co-organizers",
    )?;
    let invitees: Vec<Uuid> = people(&req.invitee_principal_ids, me, MAX_INVITEES, "invitees")?
        .into_iter()
        .filter(|p| !co.contains(p))
        .collect();
    let mine = reminders(&req.reminders_minutes_before)?;
    let everyone: Vec<Uuid> = co.iter().chain(invitees.iter()).copied().collect();
    let place = Place {
        conversation: req.conversation_id,
        community: req.community_id,
    };
    require_eligible(state, ctx, &req.scope, place, me, &everyone).await?;

    let id = Uuid::new_v4();
    let txn = state.db.begin().await.map_err(Error::from)?;
    if let Some(idem) = &idem {
        if let Some(stored) = social::claim(&txn, env, idem, now).await? {
            txn.rollback().await.map_err(Error::from)?;
            return replay(state, ctx, me, stored).await;
        }
    }
    db::exec(
        &txn,
        "INSERT INTO messaging_events (id, tenant_id, application_id, environment_id, organizer_principal_id, scope, \
           conversation_id, title, description, location, kind, meeting_id, meeting_url, cover_media_id, starts_at, \
           ends_at, time_zone, all_day, recurrence, series_ends_at, state, version, created_at, updated_at, community_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, 'ACTIVE', 1, $21, $21, \
           $22)",
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            env.into(),
            me.into(),
            req.scope.clone().into(),
            req.conversation_id.into(),
            d.title.clone().into(),
            d.description.clone().into(),
            d.location.clone().into(),
            d.kind.clone().into(),
            d.meeting_id.clone().into(),
            d.meeting_url.clone().into(),
            d.cover_media_id.into(),
            d.starts_at.into(),
            d.ends_at.into(),
            d.time_zone.clone().into(),
            d.all_day.into(),
            d.recurrence
                .as_ref()
                .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
                .into(),
            d.series_ends_at.into(),
            now.into(),
            req.community_id.into(),
        ],
    )
    .await?;
    db::exec(
        &txn,
        "INSERT INTO messaging_event_people (event_id, principal_id, role, response, responded_at, added_by, added_at) \
         VALUES ($1, $2, 'ORGANIZER', 'GOING', $3, $2, $3)",
        vec![id.into(), me.into(), now.into()],
    )
    .await?;
    for (role, list) in [("CO_ORGANIZER", &co), ("INVITEE", &invitees)] {
        for p in list.iter() {
            db::exec(
                &txn,
                "INSERT INTO messaging_event_people (event_id, principal_id, role, added_by, added_at) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                vec![id.into(), (*p).into(), role.into(), me.into(), now.into()],
            )
            .await?;
            notices::enqueue(
                state,
                &txn,
                &notices::NoticeScope::of(ctx),
                notices::Kind::Invited,
                id,
                *p,
                None,
                now,
                notices::invite_expiry(now, d.series_ends_at),
            )
            .await?;
        }
    }
    let row = load(&txn, env, id, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    if !mine.is_empty() {
        replace_reminders(&txn, ctx, &row, me, &mine, now).await?;
    }
    if let Some(idem) = &idem {
        social::remember(&txn, env, idem, &json!({ "eventId": id })).await?;
    }
    txn.commit().await.map_err(Error::from)?;
    crate::metrics::inc(
        "messaging_events_created_total",
        &[("scope", scope_label(&row.scope))],
    );
    Ok((201, render_one(state, &row, me).await?))
}

fn scope_label(scope: &str) -> &'static str {
    match scope {
        "CONVERSATION" => "conversation",
        "COMMUNITY" => "community",
        _ => "personal",
    }
}

// ----------------------------------------------------------------------------- reads

pub async fn get(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    read_limit(state, ctx, me).await?;
    let (row, _) = visible(&state.db, ctx, me, id, false).await?;
    render_one(state, &row, me).await
}

/// The views of the dashboard (ADR 0071 decision 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Upcoming,
    Today,
    Past,
    Mine,
    Invitations,
    Accepted,
}

impl View {
    fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "UPCOMING" => View::Upcoming,
            "TODAY" => View::Today,
            "PAST" => View::Past,
            "MINE" => View::Mine,
            "INVITATIONS" => View::Invitations,
            "ACCEPTED" => View::Accepted,
            _ => return None,
        })
    }
}

/// The query of `GET /v1/messaging/events`.
#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub view: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub conversation_id: Option<String>,
    pub q: Option<String>,
    pub kind: Option<String>,
    pub limit: Option<String>,
    pub cursor: Option<String>,
}

fn instant(v: &str, what: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| Error::validation(format!("{what} must be an RFC 3339 time")))
}

/// `{start µs}_{event id}`.
fn encode_cursor(start: DateTime<Utc>, id: Uuid) -> String {
    format!("{}_{}", start.timestamp_micros(), id.simple())
}

fn decode_cursor(v: &str) -> Result<(DateTime<Utc>, Uuid)> {
    let bad = || Error::validation("cursor is invalid");
    let (t, id) = v.split_once('_').ok_or_else(bad)?;
    let micros: i64 = t.parse().map_err(|_| bad())?;
    let start = DateTime::<Utc>::from_timestamp_micros(micros).ok_or_else(bad)?;
    let id = Uuid::parse_str(id).map_err(|_| bad())?;
    Ok((start, id))
}

/// A prefix full-text query of the words of `q` (letters and digits only).
fn ts_query(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(8)
        .map(|w| format!("{}:*", w.to_lowercase()))
        .collect();
    (!words.is_empty()).then(|| words.join(" & "))
}

pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    q: ListQuery,
) -> Result<Value> {
    let me = acting.principal_id;
    read_limit(state, ctx, me).await?;
    let now = state.now();
    let which = match q.view.as_deref() {
        None | Some("") => View::Upcoming,
        Some(v) => View::parse(v).ok_or_else(|| {
            Error::validation("view must be UPCOMING, TODAY, PAST, MINE, INVITATIONS or ACCEPTED")
        })?,
    };
    let limit: usize = match q.limit.as_deref() {
        None | Some("") => 30,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=100).contains(n))
            .ok_or_else(|| Error::validation("limit must be 1 to 100"))?,
    };
    let cursor = q
        .cursor
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(decode_cursor)
        .transpose()?;
    // (window start, window end, descending)
    let (from, to, desc) = match (q.from.as_deref(), q.to.as_deref()) {
        (Some(f), Some(t)) => {
            let (f, t) = (instant(f, "from")?, instant(t, "to")?);
            if t <= f || t - f > Duration::days(MAX_RANGE_DAYS) {
                return Err(Error::validation("from / to must be at most 92 days apart"));
            }
            (f, t, false)
        }
        (None, None) => match which {
            // The client sends its own day as from / to (the relay takes at most 8 query
            // keys); without them, TODAY is the UTC day.
            View::Today => {
                let day = now.date_naive();
                let start = day.and_time(NaiveTime::MIN).and_utc();
                (start, start + Duration::days(1), false)
            }
            View::Past => (DateTime::<Utc>::MIN_UTC, now, true),
            _ => (now, now + Duration::days(MAX_AHEAD_DAYS + 366), false),
        },
        _ => return Err(Error::validation("from and to go together")),
    };

    let mut params: Vec<sea_orm::Value> = vec![ctx.scope.environment_id.into(), me.into()];
    let mut sql = format!(
        "SELECT {COLUMNS} FROM messaging_events e WHERE {}",
        visible_sql(1, 2)
    );
    fn push(params: &mut Vec<sea_orm::Value>, v: sea_orm::Value) -> usize {
        params.push(v);
        params.len()
    }
    if desc {
        let n = push(&mut params, to.into());
        sql.push_str(&format!(" AND e.starts_at < ${n}"));
    } else {
        let n = push(&mut params, from.into());
        let m = push(&mut params, to.into());
        sql.push_str(&format!(
            " AND (e.series_ends_at IS NULL OR e.series_ends_at > ${n}) AND e.starts_at < ${m}"
        ));
    }
    match which {
        View::Mine => sql.push_str(
            " AND (e.organizer_principal_id = $2 OR EXISTS (SELECT 1 FROM messaging_event_people fp \
               WHERE fp.event_id = e.id AND fp.principal_id = $2 AND fp.role = 'CO_ORGANIZER'))",
        ),
        View::Invitations => sql.push_str(
            " AND e.state = 'ACTIVE' AND e.organizer_principal_id <> $2 AND ( \
               (e.scope = 'PERSONAL' AND EXISTS (SELECT 1 FROM messaging_event_people fp WHERE fp.event_id = e.id \
                  AND fp.principal_id = $2 AND fp.role = 'INVITEE' AND fp.response IS NULL)) \
               OR (e.scope IN ('CONVERSATION', 'COMMUNITY') AND NOT EXISTS (SELECT 1 FROM messaging_event_people fp \
                  WHERE fp.event_id = e.id AND fp.principal_id = $2 AND (fp.response IS NOT NULL OR fp.role = 'CO_ORGANIZER'))))",
        ),
        View::Accepted => sql.push_str(
            " AND EXISTS (SELECT 1 FROM messaging_event_people fp WHERE fp.event_id = e.id AND fp.principal_id = $2 \
               AND fp.response = 'GOING')",
        ),
        _ => {}
    }
    if let Some(c) = q.conversation_id.as_deref().filter(|c| !c.is_empty()) {
        let c =
            Uuid::parse_str(c).map_err(|_| Error::validation("conversationId must be a UUID"))?;
        let n = push(&mut params, c.into());
        sql.push_str(&format!(" AND e.conversation_id = ${n}"));
    }
    if let Some(k) = q.kind.as_deref().filter(|k| !k.is_empty()) {
        if !matches!(k, "ONLINE" | "PHYSICAL" | "CALL") {
            return Err(Error::validation("type must be ONLINE, PHYSICAL or CALL"));
        }
        let n = push(&mut params, k.to_string().into());
        sql.push_str(&format!(" AND e.kind = ${n}"));
    }
    if let Some(text) = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        if text.chars().count() > 200 {
            return Err(Error::validation("q can be at most 200 characters"));
        }
        if let Some(tsq) = ts_query(text) {
            let n = push(&mut params, tsq.into());
            sql.push_str(&format!(
                " AND e.search_vector @@ to_tsquery('simple', ${n})"
            ));
        }
    }
    sql.push_str(if desc {
        " ORDER BY e.starts_at DESC, e.id DESC"
    } else {
        " ORDER BY e.starts_at, e.id"
    });
    let n = push(&mut params, MAX_CANDIDATES.into());
    sql.push_str(&format!(" LIMIT ${n}"));
    let rows: Vec<EventRow> = db::all(&state.db, &sql, params).await?;

    // Expand: each event contributes at most `limit + 1` occurrences past the cursor.
    let take = limit + 1;
    let mut items: Vec<(DateTime<Utc>, DateTime<Utc>, usize)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let series = row.series();
        let after_cursor = |s: DateTime<Utc>| match cursor {
            None => true,
            Some((cs, cid)) if desc => (s, row.id) < (cs, cid),
            Some((cs, cid)) => (s, row.id) > (cs, cid),
        };
        if desc {
            let mut ring: std::collections::VecDeque<(DateTime<Utc>, DateTime<Utc>)> =
                std::collections::VecDeque::new();
            let upper = cursor.map(|(cs, _)| cs).unwrap_or(to);
            for (s, e) in series.occurrences().take_while(|(s, _)| *s <= upper) {
                if e <= to && after_cursor(s) {
                    ring.push_back((s, e));
                    if ring.len() > take {
                        ring.pop_front();
                    }
                }
            }
            items.extend(ring.into_iter().map(|(s, e)| (s, e, i)));
        } else {
            for (s, e) in series
                .occurrences()
                .take_while(|(s, _)| *s < to)
                .filter(|(s, e)| *e > from && after_cursor(*s))
                .take(take)
            {
                items.push((s, e, i));
            }
        }
    }
    items.sort_by(|a, b| {
        let ka = (a.0, rows[a.2].id);
        let kb = (b.0, rows[b.2].id);
        if desc {
            kb.cmp(&ka)
        } else {
            ka.cmp(&kb)
        }
    });
    let more = items.len() > limit;
    items.truncate(limit);
    let next = if more {
        items.last().map(|(s, _, i)| encode_cursor(*s, rows[*i].id))
    } else {
        None
    };
    let used: Vec<&EventRow> = {
        let mut seen = HashSet::new();
        items
            .iter()
            .filter(|(_, _, i)| seen.insert(*i))
            .map(|(_, _, i)| &rows[*i])
            .collect()
    };
    let x = extras(&state.db, &used, me).await?;
    let mut views: HashMap<Uuid, Value> = HashMap::new();
    for r in &used {
        views.insert(r.id, view(r, &x, me, now));
    }
    let out: Vec<Value> = items
        .iter()
        .map(|(s, e, i)| {
            json!({
                "startsAt": rfc3339(*s),
                "endsAt": rfc3339(*e),
                "event": views.get(&rows[*i].id).cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(json!({ "items": out, "nextCursor": next }))
}

// --------------------------------------------------------------------------- update

const UPDATABLE: &[&str] = &[
    "title",
    "description",
    "location",
    "type",
    "meetingId",
    "meetingUrl",
    "coverMediaId",
    "startsAt",
    "endsAt",
    "timeZone",
    "allDay",
    "recurrence",
    "coOrganizerPrincipalIds",
    "version",
];

fn opt_string(v: &Value, key: &str) -> Result<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(Error::validation(format!("{key} must be a string or null"))),
    }
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value, key: &str) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|e| Error::validation(format!("{key}: {e}")))
}

/// `PATCH /v1/messaging/events/{eventId}`: organizers change the event (`null` clears an
/// optional field); only the organizer changes co-organizers.
pub async fn update(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    body: Map<String, Value>,
) -> Result<Value> {
    let me = acting.principal_id;
    write_limit(state, ctx, me).await?;
    if let Some(k) = body.keys().find(|k| !UPDATABLE.contains(&k.as_str())) {
        return Err(Error::validation(format!("{k} can't be changed")));
    }
    let now = state.now();
    let (current, acc) = visible(&state.db, ctx, me, id, false).await?;
    if !acc.may_edit() {
        return Err(not_organizer());
    }
    if !current.active() {
        return Err(refuse(
            ErrorCode::Conflict,
            "EVENT_CANCELLED",
            "The event was cancelled",
        ));
    }
    if let Some(v) = body.get("version") {
        let v: i32 = parse(v, "version")?;
        if v != current.version {
            return Err(Error::code(ErrorCode::VersionConflict));
        }
    }
    let mut raw = Raw {
        title: Some(current.title.clone()),
        description: current.description.clone(),
        location: current.location.clone(),
        kind: Some(current.kind.clone()),
        meeting_id: current.meeting_id.clone(),
        meeting_url: current.meeting_url.clone(),
        cover_media_id: current.cover_media_id,
        starts_at: Some(current.starts_at),
        ends_at: Some(current.ends_at),
        time_zone: Some(current.time_zone.clone()),
        all_day: current.all_day,
        recurrence: current.rule(),
    };
    for (k, v) in &body {
        match k.as_str() {
            "title" => raw.title = opt_string(v, k)?,
            "description" => raw.description = opt_string(v, k)?,
            "location" => raw.location = opt_string(v, k)?,
            "type" => raw.kind = opt_string(v, k)?,
            "meetingId" => raw.meeting_id = opt_string(v, k)?,
            "meetingUrl" => raw.meeting_url = opt_string(v, k)?,
            "coverMediaId" => raw.cover_media_id = parse(v, k)?,
            "startsAt" => raw.starts_at = Some(parse(v, k)?),
            "endsAt" => raw.ends_at = Some(parse(v, k)?),
            "timeZone" => raw.time_zone = Some(parse(v, k)?),
            "allDay" => raw.all_day = parse(v, k)?,
            "recurrence" => raw.recurrence = parse(v, k)?,
            _ => {}
        }
    }
    let timing_changed = raw.starts_at != Some(current.starts_at)
        || raw.ends_at != Some(current.ends_at)
        || raw.time_zone.as_deref() != Some(current.time_zone.as_str())
        || raw.all_day != current.all_day
        || raw.recurrence != current.rule();
    let d = validate(raw, now, timing_changed)?;
    if d.cover_media_id.is_some() && d.cover_media_id != current.cover_media_id {
        require_cover(state, ctx, me, d.cover_media_id.unwrap_or_default()).await?;
    }
    let co: Option<Vec<Uuid>> = match body.get("coOrganizerPrincipalIds") {
        None => None,
        Some(v) => {
            if !acc.organizer() {
                return Err(refuse(
                    ErrorCode::PrincipalNotAuthorized,
                    "NOT_EVENT_ORGANIZER",
                    "Only the organizer changes co-organizers",
                ));
            }
            let ids: Vec<Uuid> = parse(v, "coOrganizerPrincipalIds")?;
            let ids = people(
                &ids,
                current.organizer_principal_id,
                MAX_CO_ORGANIZERS,
                "co-organizers",
            )?;
            require_eligible(state, ctx, &current.scope, Place::of(&current), me, &ids).await?;
            Some(ids)
        }
    };

    let txn = state.db.begin().await.map_err(Error::from)?;
    let locked = load(&txn, ctx.scope.environment_id, id, true)
        .await?
        .ok_or_else(not_found)?;
    if locked.version != current.version {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    db::exec(
        &txn,
        "UPDATE messaging_events SET title = $2, description = $3, location = $4, kind = $5, meeting_id = $6, \
           meeting_url = $7, cover_media_id = $8, starts_at = $9, ends_at = $10, time_zone = $11, all_day = $12, \
           recurrence = $13, series_ends_at = $14, version = version + 1, updated_at = $15 WHERE id = $1",
        vec![
            id.into(),
            d.title.into(),
            d.description.into(),
            d.location.into(),
            d.kind.into(),
            d.meeting_id.into(),
            d.meeting_url.into(),
            d.cover_media_id.into(),
            d.starts_at.into(),
            d.ends_at.into(),
            d.time_zone.into(),
            d.all_day.into(),
            d.recurrence
                .as_ref()
                .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
                .into(),
            d.series_ends_at.into(),
            now.into(),
        ],
    )
    .await?;
    if let Some(ids) = &co {
        db::exec(
            &txn,
            "UPDATE messaging_event_people SET role = 'INVITEE' WHERE event_id = $1 AND role = 'CO_ORGANIZER' \
               AND NOT (principal_id = ANY($2))",
            vec![id.into(), ids.clone().into()],
        )
        .await?;
        for p in ids {
            let added = db::exec(
                &txn,
                "INSERT INTO messaging_event_people (event_id, principal_id, role, added_by, added_at) \
                 VALUES ($1, $2, 'CO_ORGANIZER', $3, $4) \
                 ON CONFLICT (event_id, principal_id) DO UPDATE SET role = 'CO_ORGANIZER'",
                vec![id.into(), (*p).into(), me.into(), now.into()],
            )
            .await?;
            if added > 0 && current.scope == "PERSONAL" {
                notices::enqueue(
                    state,
                    &txn,
                    &notices::NoticeScope::of(ctx),
                    notices::Kind::Invited,
                    id,
                    *p,
                    None,
                    now,
                    notices::invite_expiry(now, d.series_ends_at),
                )
                .await?;
            }
        }
    }
    let row = load(&txn, ctx.scope.environment_id, id, false)
        .await?
        .ok_or_else(not_found)?;
    if timing_changed {
        reschedule(&txn, &row, now).await?;
    }
    txn.commit().await.map_err(Error::from)?;
    if let Some(old) = current
        .cover_media_id
        .filter(|c| Some(*c) != row.cover_media_id)
    {
        media::purge_if_unused(state, ctx.scope.environment_id, &[old]).await?;
    }
    crate::metrics::inc("messaging_events_updated_total", &[]);
    render_one(state, &row, me).await
}

// ------------------------------------------------------------------ cancel / delete

pub async fn cancel(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    reason: Option<String>,
) -> Result<Value> {
    let me = acting.principal_id;
    write_limit(state, ctx, me).await?;
    let reason = social::clean_text(&reason, MAX_CANCEL_REASON, "The reason")?;
    let now = state.now();
    let txn = state.db.begin().await.map_err(Error::from)?;
    let (row, acc) = visible(&txn, ctx, me, id, true).await?;
    if !acc.may_edit() {
        return Err(not_organizer());
    }
    if row.active() {
        db::exec(
            &txn,
            "UPDATE messaging_events SET state = 'CANCELLED', cancelled_at = $2, cancel_reason = $3, \
               version = version + 1, updated_at = $2 WHERE id = $1",
            vec![id.into(), now.into(), reason.into()],
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE messaging_event_reminders SET next_fire_at = NULL WHERE event_id = $1",
            vec![id.into()],
        )
        .await?;
        if !row.ended(now) {
            let told: Vec<PersonRow> = db::all(
                &txn,
                "SELECT principal_id, role, response, responded_at FROM messaging_event_people \
                  WHERE event_id = $1 AND principal_id <> $2 AND response IS DISTINCT FROM 'NOT_GOING'",
                vec![id.into(), me.into()],
            )
            .await?;
            for p in told {
                notices::enqueue(
                    state,
                    &txn,
                    &notices::NoticeScope::of(ctx),
                    notices::Kind::Cancelled,
                    id,
                    p.principal_id,
                    None,
                    now,
                    now + Duration::hours(24),
                )
                .await?;
            }
        }
    }
    let row = load(&txn, ctx.scope.environment_id, id, false)
        .await?
        .ok_or_else(not_found)?;
    txn.commit().await.map_err(Error::from)?;
    crate::metrics::inc("messaging_events_cancelled_total", &[]);
    render_one(state, &row, me).await
}

pub async fn delete(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    write_limit(state, ctx, me).await?;
    let (row, acc) = visible(&state.db, ctx, me, id, false).await?;
    if !acc.organizer() {
        return Err(refuse(
            ErrorCode::PrincipalNotAuthorized,
            "NOT_EVENT_ORGANIZER",
            "Only the organizer can delete the event",
        ));
    }
    db::exec(
        &state.db,
        "DELETE FROM messaging_events WHERE id = $1 AND environment_id = $2",
        vec![id.into(), ctx.scope.environment_id.into()],
    )
    .await?;
    if let Some(c) = row.cover_media_id {
        media::purge_if_unused(state, ctx.scope.environment_id, &[c]).await?;
    }
    crate::metrics::inc("messaging_events_deleted_total", &[]);
    Ok(json!({ "eventId": id, "deleted": true }))
}

// ------------------------------------------------------------------- RSVP / people

fn require_open(row: &EventRow, now: DateTime<Utc>) -> Result<()> {
    if !row.active() {
        return Err(refuse(
            ErrorCode::Conflict,
            "EVENT_CANCELLED",
            "The event was cancelled",
        ));
    }
    if row.ended(now) {
        return Err(refuse(
            ErrorCode::Conflict,
            "EVENT_ENDED",
            "The event is over",
        ));
    }
    Ok(())
}

pub async fn respond(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    response: &str,
) -> Result<Value> {
    let me = acting.principal_id;
    write_limit(state, ctx, me).await?;
    if !matches!(response, "GOING" | "MAYBE" | "NOT_GOING") {
        return Err(Error::validation(
            "response must be GOING, MAYBE or NOT_GOING",
        ));
    }
    let now = state.now();
    let (row, _) = visible(&state.db, ctx, me, id, false).await?;
    require_open(&row, now)?;
    db::exec(
        &state.db,
        "INSERT INTO messaging_event_people (event_id, principal_id, role, response, responded_at, added_by, added_at) \
         VALUES ($1, $2, 'MEMBER', $3, $4, $2, $4) \
         ON CONFLICT (event_id, principal_id) DO UPDATE SET response = EXCLUDED.response, responded_at = EXCLUDED.responded_at",
        vec![id.into(), me.into(), response.into(), now.into()],
    )
    .await?;
    crate::metrics::inc("messaging_event_responses_total", &[]);
    render_one(state, &row, me).await
}

pub async fn attendees(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    read_limit(state, ctx, me).await?;
    let (row, _) = visible(&state.db, ctx, me, id, false).await?;
    attendees_view(state, &row, me).await
}

async fn attendees_view(state: &AppState, row: &EventRow, me: Uuid) -> Result<Value> {
    let rows: Vec<PersonRow> = db::all(
        &state.db,
        &format!(
            "SELECT p.principal_id, p.role, p.response, p.responded_at FROM messaging_event_people p \
          JOIN messaging_events e ON e.id = p.event_id \
          WHERE p.event_id = $1 AND (e.scope NOT IN ('CONVERSATION', 'COMMUNITY') OR p.principal_id = e.organizer_principal_id \
                OR (e.scope = 'CONVERSATION' AND EXISTS (SELECT 1 FROM messaging_conversation_members m \
                    WHERE m.conversation_id = e.conversation_id AND m.principal_id = p.principal_id AND m.state = 'ACTIVE')) \
                OR (e.scope = 'COMMUNITY' AND {community})) \
          ORDER BY CASE p.role WHEN 'ORGANIZER' THEN 0 WHEN 'CO_ORGANIZER' THEN 1 WHEN 'INVITEE' THEN 2 ELSE 3 END, \
                   p.added_at, p.principal_id LIMIT 2000",
            community = communities::active_member_sql("e.community_id", "p.principal_id")
        ),
        vec![row.id.into()],
    )
    .await?;
    let x = extras(&state.db, &[row], me).await?;
    let people: Vec<Value> = rows
        .iter()
        .map(|p| {
            json!({
                "principalId": p.principal_id,
                "role": p.role,
                "response": p.response,
                "respondedAt": p.responded_at.map(rfc3339),
            })
        })
        .collect();
    Ok(json!({
        "eventId": row.id,
        "counts": x.counts.get(&row.id).cloned().unwrap_or(Value::Null),
        "attendees": people,
    }))
}

pub async fn invite(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    ids: Vec<Uuid>,
) -> Result<Value> {
    let me = acting.principal_id;
    write_limit(state, ctx, me).await?;
    let now = state.now();
    let (row, acc) = visible(&state.db, ctx, me, id, false).await?;
    if !acc.may_edit() {
        return Err(not_organizer());
    }
    require_open(&row, now)?;
    let ids: Vec<Uuid> = people(&ids, me, MAX_INVITES_PER_REQUEST, "people at a time")?
        .into_iter()
        .filter(|p| *p != row.organizer_principal_id)
        .collect();
    if ids.is_empty() {
        return Err(Error::validation("Choose someone to invite"));
    }
    require_eligible(state, ctx, &row.scope, Place::of(&row), me, &ids).await?;
    let txn = state.db.begin().await.map_err(Error::from)?;
    let present = db::scalar_i64(
        &txn,
        "SELECT count(*) FROM messaging_event_people WHERE event_id = $1 AND role = 'INVITEE'",
        vec![id.into()],
    )
    .await?;
    if present + ids.len() as i64 > MAX_INVITEES as i64 {
        return Err(Error::reason(ErrorCode::QuotaExceeded, "TOO_MANY_INVITEES"));
    }
    for p in &ids {
        let added = db::exec(
            &txn,
            "INSERT INTO messaging_event_people (event_id, principal_id, role, added_by, added_at) \
             VALUES ($1, $2, 'INVITEE', $3, $4) ON CONFLICT DO NOTHING",
            vec![id.into(), (*p).into(), me.into(), now.into()],
        )
        .await?;
        if added > 0 {
            notices::enqueue(
                state,
                &txn,
                &notices::NoticeScope::of(ctx),
                notices::Kind::Invited,
                id,
                *p,
                None,
                now,
                notices::invite_expiry(now, row.series_ends_at),
            )
            .await?;
        }
    }
    txn.commit().await.map_err(Error::from)?;
    attendees_view(state, &row, me).await
}

pub async fn uninvite(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    who: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    write_limit(state, ctx, me).await?;
    let (row, acc) = visible(&state.db, ctx, me, id, false).await?;
    if !acc.may_edit() {
        return Err(not_organizer());
    }
    if let Some(p) = person(&state.db, id, who).await? {
        if !matches!(p.role.as_str(), "INVITEE" | "MEMBER") {
            return Err(refuse(
                ErrorCode::Conflict,
                "NOT_AN_INVITEE",
                "Organizers are removed by changing the co-organizers",
            ));
        }
        let txn = state.db.begin().await.map_err(Error::from)?;
        db::exec(
            &txn,
            "DELETE FROM messaging_event_people WHERE event_id = $1 AND principal_id = $2",
            vec![id.into(), who.into()],
        )
        .await?;
        db::exec(
            &txn,
            "DELETE FROM messaging_event_reminders WHERE event_id = $1 AND principal_id = $2",
            vec![id.into(), who.into()],
        )
        .await?;
        txn.commit().await.map_err(Error::from)?;
    }
    attendees_view(state, &row, me).await
}

pub async fn set_reminders(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    minutes: Vec<i32>,
) -> Result<Value> {
    let me = acting.principal_id;
    write_limit(state, ctx, me).await?;
    let minutes = reminders(&minutes)?;
    let txn = state.db.begin().await.map_err(Error::from)?;
    let (row, _) = visible(&txn, ctx, me, id, false).await?;
    replace_reminders(&txn, ctx, &row, me, &minutes, state.now()).await?;
    txn.commit().await.map_err(Error::from)?;
    Ok(json!({ "eventId": id, "minutesBefore": minutes }))
}

// ------------------------------------------------------------------------------ ICS

fn ics_event(row: &EventRow) -> ics::IcsEvent {
    let series = row.series();
    ics::IcsEvent {
        id: row.id,
        title: row.title.clone(),
        description: row.description.clone(),
        location: row.location.clone(),
        meeting_url: row.meeting_url.clone(),
        meeting_id: row.meeting_id.clone(),
        start: row.starts_at,
        end: row.ends_at,
        tz: series.tz,
        all_day: row.all_day,
        rrule: series.rrule(),
        cancelled: !row.active(),
        sequence: row.version - 1,
        created: row.created_at,
        updated: row.updated_at,
    }
}

fn calendar_file(filename: String, content: String) -> Value {
    json!({
        "filename": filename,
        "contentType": "text/calendar; charset=utf-8",
        "content": content,
    })
}

pub async fn export_one(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    read_limit(state, ctx, me).await?;
    let (row, _) = visible(&state.db, ctx, me, id, false).await?;
    let content = ics::calendar("CF Meet", &[ics_event(&row)], state.now());
    Ok(calendar_file(ics::filename(&row.title), content))
}

pub async fn export_calendar(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
) -> Result<Value> {
    let me = acting.principal_id;
    read_limit(state, ctx, me).await?;
    let now = state.now();
    let rows: Vec<EventRow> = db::all(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_events e WHERE {} \
               AND (e.series_ends_at IS NULL OR e.series_ends_at > $3) ORDER BY e.starts_at, e.id LIMIT $4",
            visible_sql(1, 2)
        ),
        vec![
            ctx.scope.environment_id.into(),
            me.into(),
            (now - Duration::days(FEED_PAST_DAYS)).into(),
            FEED_MAX_EVENTS.into(),
        ],
    )
    .await?;
    let events: Vec<ics::IcsEvent> = rows.iter().map(ics_event).collect();
    let content = ics::calendar("CF Meet events", &events, now);
    Ok(calendar_file("cfmeet-events.ics".into(), content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let t = DateTime::<Utc>::from_timestamp_micros(1_790_000_000_123_456).unwrap();
        let id = Uuid::new_v4();
        assert_eq!(decode_cursor(&encode_cursor(t, id)).unwrap(), (t, id));
        for bad in ["", "x", "1_", "_abc", "1_notauuid"] {
            assert!(decode_cursor(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn search_words_become_a_safe_prefix_query() {
        assert_eq!(ts_query("Team sync!").unwrap(), "team:* & sync:*");
        assert_eq!(ts_query("a'b|c&d"), Some("a:* & b:* & c:* & d:*".into()));
        assert!(ts_query("  !!! ").is_none());
    }

    #[test]
    fn meeting_links_are_bounded() {
        assert!(valid_meeting_url("https://cfmeet.com/abc-defg-hij"));
        assert!(!valid_meeting_url("http://cfmeet.com/x"));
        assert!(!valid_meeting_url("javascript:alert(1)"));
        assert!(!valid_meeting_url("https://a b"));
        assert!(valid_meeting_id("abc-defg_hij"));
        assert!(!valid_meeting_id("a/b"));
    }

    #[test]
    fn reminders_are_bounded_and_deduplicated() {
        assert_eq!(reminders(&[60, 10, 60]).unwrap(), vec![10, 60]);
        assert!(reminders(&[1, 2, 3, 4, 5, 6]).is_err());
        assert!(reminders(&[-1]).is_err());
        assert!(reminders(&[40_321]).is_err());
    }
}
