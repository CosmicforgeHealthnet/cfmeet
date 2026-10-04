//! BreakoutService (Part F §31–§41).
//!
//! A breakout belongs to its parent ConferenceSession — never to a MeetingSpace — and is
//! not an independent meeting: participants keep their single ConferenceParticipant and
//! ParticipantSession identities.
//!
//! **Provider architecture (strategy A — provider room per breakout).** Each breakout
//! has its own LiveKit room `cfbr-<breakout uuid>` (a prefix distinct from the
//! conference rooms `cf-…`, so Part E room scans never mistake it for an unmapped
//! conference room). LiveKit isolates rooms completely, so media of breakout A can never
//! reach breakout B. Every participant has exactly ONE authorized media context,
//! derived from PostgreSQL only ([`media_context`]):
//!
//! ```text
//! host visit (breakout_visit_room_id, room OPEN|CLOSING)
//!   else active assignment to an OPEN|CLOSING room
//!   else MAIN conference room
//! ```
//!
//! Credentials are issued only for the current context (join and
//! `POST …/media-credential`). After an assignment change commits, a
//! `rtc.participant_reroute_requested` command removes the participant's sessions from
//! the previous room; a provider join into any room other than the current context is
//! evicted (webhook + reconciler), so stale credentials cannot keep an old context. The
//! parent conference keeps authority: closing returns everyone to MAIN and terminates
//! the breakout room; ending the conference closes every breakout.
//!
//! Host visits are privileged presence, not assignments: they never count toward
//! balancing or capacity, and leaving a visit returns the host to MAIN.

use std::collections::{HashMap, HashSet};

use chrono::{Duration, Utc};
use rand::{seq::SliceRandom, Rng};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait,
    QueryFilter, QueryOrder, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, ConferenceStatus, IdentityType, Role, SessionState},
    error::{ApiError, ApiResult, ErrorCode},
    models::{
        breakout_assignments, breakout_rooms, conference_participants, conference_sessions,
        outbox_events, participant_sessions,
    },
    services::{
        actor::ActorContext,
        meeting_core::{find_conference, find_scoped_participant, lock_conference, Core},
        meeting_events::{
            action, audit, emit, emit_to, event, Audience, AuditActor, AuditRecord, OutboxRecord,
        },
        meeting_permissions::Permission,
        rtc_control::{CommandResult, Enforcement, RtcControl},
        rtc_provider::{RoomSpec, RtcError, RtcParticipantRef, RtcRoomRef},
    },
};

use super::{
    metrics::{inc, COLLAB_METRICS as CM},
    policy::BreakoutRecordingPolicy,
    Collab, RateAction,
};

pub const BREAKOUT_ROOM_PREFIX: &str = "cfbr-";
pub const MAX_COUNTDOWN_SECS: u32 = 600;

pub fn breakout_room_name(breakout_id: Uuid) -> String {
    format!("{BREAKOUT_ROOM_PREFIX}{}", breakout_id.simple())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BreakoutStatus {
    Created,
    Open,
    Closing,
    Closed,
}

impl BreakoutStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "OPEN" => BreakoutStatus::Open,
            "CLOSING" => BreakoutStatus::Closing,
            "CLOSED" => BreakoutStatus::Closed,
            _ => BreakoutStatus::Created,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            BreakoutStatus::Created => "CREATED",
            BreakoutStatus::Open => "OPEN",
            BreakoutStatus::Closing => "CLOSING",
            BreakoutStatus::Closed => "CLOSED",
        }
    }

    /// Rooms that currently route media.
    pub fn is_active(&self) -> bool {
        matches!(self, BreakoutStatus::Open | BreakoutStatus::Closing)
    }
}

/// Strict breakout transitions (§31, §39).
pub fn breakout_transition_allowed(from: BreakoutStatus, to: BreakoutStatus) -> bool {
    use BreakoutStatus::*;
    matches!(
        (from, to),
        (Created, Open) | (Created, Closed) | (Open, Closing) | (Open, Closed) | (Closing, Closed)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssignmentMode {
    Manual,
    Random,
    Balanced,
}

impl AssignmentMode {
    fn as_str(&self) -> &'static str {
        match self {
            AssignmentMode::Manual => "MANUAL",
            AssignmentMode::Random => "RANDOM",
            AssignmentMode::Balanced => "BALANCED",
        }
    }
}

/// A participant's single authorized media context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaContext {
    Main,
    Breakout {
        breakout_id: Uuid,
        room_name: String,
        visit: bool,
    },
}

impl MediaContext {
    pub fn room_name(&self, conference: &conference_sessions::Model) -> String {
        match self {
            MediaContext::Main => RtcRoomRef::for_conference(conference).room_name,
            MediaContext::Breakout { room_name, .. } => room_name.clone(),
        }
    }

    pub fn breakout_id(&self) -> Option<Uuid> {
        match self {
            MediaContext::Main => None,
            MediaContext::Breakout { breakout_id, .. } => Some(*breakout_id),
        }
    }
}

fn room_ref(room: &breakout_rooms::Model) -> RtcRoomRef {
    RtcRoomRef {
        conference_id: room.conference_id,
        room_name: room
            .rtc_room_name
            .clone()
            .unwrap_or_else(|| breakout_room_name(room.id)),
    }
}

/// Current authorized media context (PostgreSQL only).
pub async fn media_context<C: ConnectionTrait>(
    conn: &C,
    conference: &conference_sessions::Model,
    participant: &conference_participants::Model,
) -> ApiResult<MediaContext> {
    if let Some(visit) = participant.breakout_visit_room_id {
        if let Some(room) = breakout_rooms::Entity::find_by_id(visit)
            .filter(breakout_rooms::Column::ConferenceId.eq(conference.id))
            .one(conn)
            .await?
        {
            if BreakoutStatus::parse(&room.status).is_active() {
                return Ok(MediaContext::Breakout {
                    breakout_id: room.id,
                    room_name: room_ref(&room).room_name,
                    visit: true,
                });
            }
        }
    }
    if let Some(a) = active_assignment(conn, participant.id).await? {
        if let Some(room) = breakout_rooms::Entity::find_by_id(a.breakout_room_id)
            .filter(breakout_rooms::Column::ConferenceId.eq(conference.id))
            .one(conn)
            .await?
        {
            if BreakoutStatus::parse(&room.status).is_active() {
                return Ok(MediaContext::Breakout {
                    breakout_id: room.id,
                    room_name: room_ref(&room).room_name,
                    visit: false,
                });
            }
        }
    }
    Ok(MediaContext::Main)
}

pub async fn active_assignment<C: ConnectionTrait>(
    conn: &C,
    participant_id: Uuid,
) -> ApiResult<Option<breakout_assignments::Model>> {
    Ok(breakout_assignments::Entity::find()
        .filter(breakout_assignments::Column::ParticipantId.eq(participant_id))
        .filter(breakout_assignments::Column::UnassignedAt.is_null())
        .one(conn)
        .await?)
}

/// Provider rooms of a conference that may hold its participants: the main room plus
/// every active breakout room.
pub async fn conference_rooms<C: ConnectionTrait>(
    conn: &C,
    conference: &conference_sessions::Model,
) -> ApiResult<Vec<String>> {
    let mut rooms = vec![RtcRoomRef::for_conference(conference).room_name];
    for r in breakout_rooms::Entity::find()
        .filter(breakout_rooms::Column::ConferenceId.eq(conference.id))
        .filter(breakout_rooms::Column::Status.is_in(["OPEN", "CLOSING"]))
        .all(conn)
        .await?
    {
        rooms.push(room_ref(&r).room_name);
    }
    Ok(rooms)
}

/// Breakout room by provider room name (webhook mapping).
pub async fn breakout_by_room<C: ConnectionTrait>(
    conn: &C,
    room_name: &str,
) -> ApiResult<Option<breakout_rooms::Model>> {
    Ok(breakout_rooms::Entity::find()
        .filter(breakout_rooms::Column::RtcRoomName.eq(room_name))
        .one(conn)
        .await?)
}

fn not_found() -> ApiError {
    ApiError::domain(ErrorCode::BreakoutNotFound, "Breakout room not found")
}

fn closed() -> ApiError {
    ApiError::domain(ErrorCode::BreakoutClosed, "This breakout room is closed")
}

async fn scoped_room<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    breakout_id: Uuid,
) -> ApiResult<breakout_rooms::Model> {
    breakout_rooms::Entity::find_by_id(breakout_id)
        .filter(breakout_rooms::Column::ConferenceId.eq(conference_id))
        .one(conn)
        .await?
        .ok_or_else(not_found)
}

/// Balanced: fill the emptiest room first (existing occupancy counts), respecting
/// capacity; eligible order is preserved, so the result is deterministic.
pub fn balanced_assignment(
    eligible: &[Uuid],
    rooms: &[(Uuid, usize, Option<usize>)],
) -> Vec<(Uuid, Uuid)> {
    let mut load: Vec<(Uuid, usize, Option<usize>)> = rooms.to_vec();
    let mut out = vec![];
    for pid in eligible {
        let best = load
            .iter_mut()
            .enumerate()
            .filter(|(_, (_, n, cap))| cap.map(|c| *n < c).unwrap_or(true))
            .min_by_key(|(i, (_, n, _))| (*n, *i))
            .map(|(i, _)| i);
        let Some(i) = best else { break };
        load[i].1 += 1;
        out.push((*pid, load[i].0));
    }
    out
}

/// Random: shuffle participants, then give each a uniformly random room that still has
/// capacity.
pub fn random_assignment<R: Rng>(
    eligible: &[Uuid],
    rooms: &[(Uuid, usize, Option<usize>)],
    rng: &mut R,
) -> Vec<(Uuid, Uuid)> {
    let mut people = eligible.to_vec();
    people.shuffle(rng);
    let mut load: Vec<(Uuid, usize, Option<usize>)> = rooms.to_vec();
    let mut out = vec![];
    for pid in people {
        let open: Vec<usize> = (0..load.len())
            .filter(|i| load[*i].2.map(|c| load[*i].1 < c).unwrap_or(true))
            .collect();
        let Some(&i) = open.choose(rng) else { break };
        load[i].1 += 1;
        out.push((pid, load[i].0));
    }
    out
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BreakoutView {
    pub id: Uuid,
    pub conference_id: Uuid,
    pub name: String,
    pub status: BreakoutStatus,
    pub position: i32,
    pub capacity: Option<i32>,
    pub recording_policy: String,
    pub opened_at: Option<chrono::DateTime<Utc>>,
    pub closing_at: Option<chrono::DateTime<Utc>>,
    pub closed_at: Option<chrono::DateTime<Utc>>,
    pub participant_count: usize,
    /// Assigned participant ids — only for roles that manage breakouts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_ids: Option<Vec<Uuid>>,
    pub version: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaContextView {
    /// MAIN or BREAKOUT.
    pub kind: String,
    pub breakout_id: Option<Uuid>,
    pub visit: bool,
}

impl From<&MediaContext> for MediaContextView {
    fn from(c: &MediaContext) -> Self {
        match c {
            MediaContext::Main => Self {
                kind: "MAIN".into(),
                breakout_id: None,
                visit: false,
            },
            MediaContext::Breakout {
                breakout_id, visit, ..
            } => Self {
                kind: "BREAKOUT".into(),
                breakout_id: Some(*breakout_id),
                visit: *visit,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentResult {
    pub assigned: Vec<AssignmentPair>,
    /// Eligible participants left unassigned (all rooms at capacity).
    pub unassigned: Vec<Uuid>,
    pub enforcement: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentPair {
    pub participant_id: Uuid,
    pub breakout_id: Uuid,
}

fn view(
    r: &breakout_rooms::Model,
    members: &HashMap<Uuid, Vec<Uuid>>,
    manager: bool,
) -> BreakoutView {
    let ids = members.get(&r.id).cloned().unwrap_or_default();
    BreakoutView {
        id: r.id,
        conference_id: r.conference_id,
        name: r.name.clone(),
        status: BreakoutStatus::parse(&r.status),
        position: r.position,
        capacity: r.capacity,
        recording_policy: r.recording_policy.clone(),
        opened_at: r.opened_at,
        closing_at: r.closing_at,
        closed_at: r.closed_at,
        participant_count: ids.len(),
        participant_ids: manager.then_some(ids),
        version: r.version,
    }
}

fn room_payload(r: &breakout_rooms::Model) -> serde_json::Value {
    json!({
        "conferenceId": r.conference_id,
        "breakoutId": r.id,
        "name": r.name,
        "status": r.status,
        "closingAt": r.closing_at,
        "version": r.version,
    })
}

fn room_record<'a>(
    r: &breakout_rooms::Model,
    space_id: Uuid,
    event_type: &'a str,
) -> OutboxRecord<'a> {
    OutboxRecord {
        aggregate_type: "breakout_room",
        aggregate_id: r.id,
        event_type,
        meeting_space_id: Some(space_id),
        conference_id: Some(r.conference_id),
        payload: room_payload(r),
    }
}

/// Self + breakout managers.
fn moved_audience(participant_id: Uuid) -> Audience {
    Audience::Participants {
        ids: vec![participant_id],
        moderator_permission: Some(Permission::BreakoutAssign.key().into()),
    }
}

/// Emit `breakout.participant_moved` and queue provider rerouting when the context
/// changed (from = previous media context).
async fn record_move(
    txn: &DatabaseTransaction,
    conference: &conference_sessions::Model,
    participant_id: Uuid,
    from: &MediaContext,
    to: &MediaContext,
) -> ApiResult<()> {
    if from == to {
        return Ok(());
    }
    emit_to(
        txn,
        OutboxRecord {
            aggregate_type: "conference_participant",
            aggregate_id: participant_id,
            event_type: event::BREAKOUT_PARTICIPANT_MOVED,
            meeting_space_id: Some(conference.meeting_space_id),
            conference_id: Some(conference.id),
            payload: json!({
                "participantId": participant_id,
                "fromBreakoutId": from.breakout_id(),
                "toBreakoutId": to.breakout_id(),
                "context": MediaContextView::from(to),
            }),
        },
        Some(moved_audience(participant_id)),
    )
    .await?;
    emit(
        txn,
        OutboxRecord {
            aggregate_type: "conference_participant",
            aggregate_id: participant_id,
            event_type: event::RTC_PARTICIPANT_REROUTE_REQUESTED,
            meeting_space_id: Some(conference.meeting_space_id),
            conference_id: Some(conference.id),
            payload: json!({
                "conferenceId": conference.id,
                "participantId": participant_id,
                "fromBreakoutId": from.breakout_id(),
            }),
        },
    )
    .await
}

/// Move a participant's assignment (`None` = back to main). Caller holds the
/// conference row lock and has validated the target room.
async fn set_assignment(
    txn: &DatabaseTransaction,
    conference: &conference_sessions::Model,
    participant: &conference_participants::Model,
    to: Option<&breakout_rooms::Model>,
    mode: &str,
    by: &str,
) -> ApiResult<bool> {
    let current = active_assignment(txn, participant.id).await?;
    if current.as_ref().map(|a| a.breakout_room_id) == to.map(|r| r.id) {
        return Ok(false);
    }
    let from = media_context(txn, conference, participant).await?;
    let now = Utc::now();
    if let Some(a) = current {
        let mut am: breakout_assignments::ActiveModel = a.into();
        am.unassigned_at = Set(Some(now));
        am.update(txn).await?;
    }
    if let Some(room) = to {
        breakout_assignments::ActiveModel {
            id: Set(Uuid::new_v4()),
            breakout_room_id: Set(room.id),
            conference_id: Set(conference.id),
            participant_id: Set(participant.id),
            assigned_by: Set(Some(by.to_string())),
            assigned_at: Set(now),
            unassigned_at: Set(None),
            mode: Set(mode.to_string()),
        }
        .insert(txn)
        .await?;
    }
    let to_ctx = media_context(txn, conference, participant).await?;
    record_move(txn, conference, participant.id, &from, &to_ctx).await?;
    Ok(true)
}

async fn assignments_by_room<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
) -> ApiResult<HashMap<Uuid, Vec<Uuid>>> {
    let mut map: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for a in breakout_assignments::Entity::find()
        .filter(breakout_assignments::Column::ConferenceId.eq(conference_id))
        .filter(breakout_assignments::Column::UnassignedAt.is_null())
        .order_by_asc(breakout_assignments::Column::AssignedAt)
        .all(conn)
        .await?
    {
        map.entry(a.breakout_room_id)
            .or_default()
            .push(a.participant_id);
    }
    Ok(map)
}

/// CLOSING|OPEN|CREATED → CLOSED: everyone returns to MAIN, visits end, the provider
/// room is terminated after commit. Caller holds the conference row lock.
pub async fn finalize_close(
    txn: &DatabaseTransaction,
    conference: &conference_sessions::Model,
    room: breakout_rooms::Model,
    actor: &AuditActor,
) -> ApiResult<breakout_rooms::Model> {
    if room.status == "CLOSED" {
        return Ok(room);
    }
    let was_active = BreakoutStatus::parse(&room.status).is_active();
    let now = Utc::now();
    let members: Vec<Uuid> = breakout_assignments::Entity::find()
        .filter(breakout_assignments::Column::BreakoutRoomId.eq(room.id))
        .filter(breakout_assignments::Column::UnassignedAt.is_null())
        .all(txn)
        .await?
        .into_iter()
        .map(|a| a.participant_id)
        .collect();
    let visitors: Vec<conference_participants::Model> = conference_participants::Entity::find()
        .filter(conference_participants::Column::ConferenceId.eq(conference.id))
        .filter(conference_participants::Column::BreakoutVisitRoomId.eq(room.id))
        .all(txn)
        .await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE breakout_assignments SET unassigned_at = $2 WHERE breakout_room_id = $1 AND unassigned_at IS NULL",
        [room.id.into(), now.into()],
    ))
    .await?;
    for v in &visitors {
        let mut am: conference_participants::ActiveModel = v.clone().into();
        am.breakout_visit_room_id = Set(None);
        am.updated_at = Set(now);
        am.update(txn).await?;
    }
    let mut am: breakout_rooms::ActiveModel = room.clone().into();
    am.status = Set("CLOSED".into());
    am.closed_at = Set(Some(now));
    am.version = Set(room.version + 1);
    am.updated_at = Set(now);
    let updated = am.update(txn).await?;
    audit(
        txn,
        actor,
        AuditRecord {
            meeting_space_id: conference.meeting_space_id,
            conference_id: Some(conference.id),
            action: action::BREAKOUT_CLOSE_COMPLETE,
            target_type: Some("BREAKOUT"),
            target_ref: Some(updated.id.to_string()),
            metadata: json!({ "returnedParticipants": members.len() + visitors.len() }),
        },
    )
    .await?;
    let mut payload = room_payload(&updated);
    payload["returnedParticipantIds"] = json!(members);
    emit(
        txn,
        OutboxRecord {
            aggregate_type: "breakout_room",
            aggregate_id: updated.id,
            event_type: event::BREAKOUT_CLOSED,
            meeting_space_id: Some(conference.meeting_space_id),
            conference_id: Some(conference.id),
            payload,
        },
    )
    .await?;
    if was_active {
        let ctx = MediaContext::Breakout {
            breakout_id: updated.id,
            room_name: room_ref(&updated).room_name,
            visit: false,
        };
        let returned: HashSet<Uuid> = members
            .iter()
            .copied()
            .chain(visitors.iter().map(|v| v.id))
            .collect();
        for pid in returned {
            emit_to(
                txn,
                OutboxRecord {
                    aggregate_type: "conference_participant",
                    aggregate_id: pid,
                    event_type: event::BREAKOUT_PARTICIPANT_MOVED,
                    meeting_space_id: Some(conference.meeting_space_id),
                    conference_id: Some(conference.id),
                    payload: json!({
                        "participantId": pid,
                        "fromBreakoutId": ctx.breakout_id(),
                        "toBreakoutId": null,
                        "context": MediaContextView::from(&MediaContext::Main),
                        "reason": "BREAKOUT_CLOSED",
                    }),
                },
                Some(moved_audience(pid)),
            )
            .await?;
        }
        emit(
            txn,
            OutboxRecord {
                aggregate_type: "breakout_room",
                aggregate_id: updated.id,
                event_type: event::RTC_BREAKOUT_TERMINATE_REQUESTED,
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference.id),
                payload: json!({ "conferenceId": conference.id, "breakoutId": updated.id }),
            },
        )
        .await?;
    }
    inc(&CM.breakouts_closed);
    Ok(updated)
}

/// Close every non-closed breakout of a conference (conference closing). Caller holds
/// the conference row lock.
pub async fn close_all_in_txn(
    txn: &DatabaseTransaction,
    conference: &conference_sessions::Model,
    actor: &AuditActor,
) -> ApiResult<u32> {
    let rooms = breakout_rooms::Entity::find()
        .filter(breakout_rooms::Column::ConferenceId.eq(conference.id))
        .filter(breakout_rooms::Column::Status.ne("CLOSED"))
        .all(txn)
        .await?;
    let n = rooms.len() as u32;
    for r in rooms {
        finalize_close(txn, conference, r, actor).await?;
    }
    Ok(n)
}

/// Create/ensure the provider room of an active breakout and record readiness.
pub async fn ensure_breakout_room(core: &Core, room: &breakout_rooms::Model) -> ApiResult<()> {
    if !BreakoutStatus::parse(&room.status).is_active() {
        return Err(closed());
    }
    let r = room_ref(room);
    match core.rtc.get_room(&r).await {
        Ok(Some(_)) if room.rtc_room_ready_at.is_some() => return Ok(()),
        Ok(_) => {}
        Err(e) => {
            inc(&CM.breakout_routing_failures);
            return Err(e.into());
        }
    }
    if let Err(e) = core.rtc.create_room(&r, &RoomSpec::default()).await {
        inc(&CM.breakout_routing_failures);
        return Err(e.into());
    }
    core.db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE breakout_rooms SET rtc_room_ready_at = COALESCE(rtc_room_ready_at, now()) WHERE id = $1",
            [room.id.into()],
        ))
        .await?;
    Ok(())
}

/// Ensure the provider room of a participant's current context exists.
pub async fn ensure_context_room(
    core: &Core,
    rtc: &RtcControl,
    conference: &conference_sessions::Model,
    ctx: &MediaContext,
) -> ApiResult<()> {
    match ctx {
        MediaContext::Main => rtc.verify_room_for_join(conference.id).await,
        MediaContext::Breakout { breakout_id, .. } => {
            let room = scoped_room(&core.db, conference.id, *breakout_id).await?;
            ensure_breakout_room(core, &room).await
        }
    }
}

fn uuid_field(p: &serde_json::Value, key: &str) -> Option<Uuid> {
    p[key].as_str().and_then(|s| Uuid::parse_str(s).ok())
}

/// Execute the Part F breakout provider commands (re-reading authoritative state).
pub async fn execute_command(
    core: &Core,
    conference: &conference_sessions::Model,
    row: &outbox_events::Model,
) -> CommandResult {
    let p = &row.payload;
    let retry = |e: ApiError| CommandResult::Retry(e.to_string());
    match row.event_type.as_str() {
        event::RTC_BREAKOUT_PREPARE_REQUESTED => {
            let Some(bid) = uuid_field(p, "breakoutId") else {
                return CommandResult::Dead("missing breakoutId".into());
            };
            let room = match scoped_room(&core.db, conference.id, bid).await {
                Ok(r) => r,
                Err(_) => {
                    return CommandResult::Dead("breakout is not part of this conference".into())
                }
            };
            if !BreakoutStatus::parse(&room.status).is_active()
                || conference.status != ConferenceStatus::Live
            {
                return CommandResult::Done;
            }
            match ensure_breakout_room(core, &room).await {
                Ok(()) => CommandResult::Done,
                Err(e) => retry(e),
            }
        }
        event::RTC_BREAKOUT_TERMINATE_REQUESTED => {
            let Some(bid) = uuid_field(p, "breakoutId") else {
                return CommandResult::Dead("missing breakoutId".into());
            };
            let room = match scoped_room(&core.db, conference.id, bid).await {
                Ok(r) => r,
                Err(_) => {
                    return CommandResult::Dead("breakout is not part of this conference".into())
                }
            };
            if room.status != "CLOSED" {
                return CommandResult::Dead("terminate requested for an active breakout".into());
            }
            match core
                .rtc
                .terminate_room(&room_ref(&room), "BREAKOUT_CLOSED")
                .await
            {
                Ok(()) => CommandResult::Done,
                Err(e) => {
                    inc(&CM.breakout_routing_failures);
                    CommandResult::Retry(e.to_string())
                }
            }
        }
        event::RTC_PARTICIPANT_REROUTE_REQUESTED => {
            let Some(pid) = uuid_field(p, "participantId") else {
                return CommandResult::Dead("missing participantId".into());
            };
            let participant = match find_scoped_participant(&core.db, conference.id, pid, false)
                .await
            {
                Ok(pp) => pp,
                Err(_) => {
                    return CommandResult::Dead("participant is not part of this conference".into())
                }
            };
            let from_room = match uuid_field(p, "fromBreakoutId") {
                None => RtcRoomRef::for_conference(conference).room_name,
                Some(bid) => match scoped_room(&core.db, conference.id, bid).await {
                    Ok(r) => room_ref(&r).room_name,
                    Err(_) => {
                        return CommandResult::Dead(
                            "breakout is not part of this conference".into(),
                        )
                    }
                },
            };
            let current = match media_context(&core.db, conference, &participant).await {
                Ok(c) => c.room_name(conference),
                Err(e) => return retry(e),
            };
            if current == from_room {
                // Moved back in the meantime: nothing to undo.
                return CommandResult::Done;
            }
            let sessions = match participant_sessions::Entity::find()
                .filter(participant_sessions::Column::ConferenceParticipantId.eq(pid))
                .filter(participant_sessions::Column::ConferenceId.eq(conference.id))
                .filter(
                    participant_sessions::Column::State
                        .is_in([SessionState::Pending, SessionState::Connected]),
                )
                .all(&core.db)
                .await
            {
                Ok(s) => s,
                Err(e) => return CommandResult::Retry(e.to_string()),
            };
            for s in sessions {
                let target = RtcParticipantRef {
                    conference_id: conference.id,
                    room_name: from_room.clone(),
                    participant_session_id: s.id,
                    media_identity: s.media_identity.clone(),
                };
                match core
                    .rtc
                    .remove_participant(&target, "BREAKOUT_REROUTED")
                    .await
                {
                    Ok(()) | Err(RtcError::NotFound) => {}
                    Err(e) => {
                        inc(&CM.breakout_routing_failures);
                        return CommandResult::Retry(e.to_string());
                    }
                }
            }
            CommandResult::Done
        }
        other => CommandResult::Dead(format!("unknown breakout command {other}")),
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBreakoutsRequest {
    /// Named rooms. Mutually exclusive with `count`.
    pub names: Option<Vec<String>>,
    /// Number of automatically numbered rooms ("Room N").
    pub count: Option<u16>,
    pub capacity: Option<i32>,
    pub recording_policy: Option<BreakoutRecordingPolicy>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignBreakoutsRequest {
    pub mode: Option<AssignmentMode>,
    /// MANUAL: explicit participant → breakout mapping.
    pub assignments: Option<Vec<ManualAssignment>>,
    /// RANDOM / BALANCED: target rooms (default: every CREATED/OPEN room).
    pub breakout_ids: Option<Vec<Uuid>>,
    /// Include OWNER/HOST/CO_HOST participants (default false).
    #[serde(default)]
    pub include_hosts: bool,
    /// Include VIEWER participants (default true).
    pub include_viewers: Option<bool>,
    /// Only distribute participants without an assignment (default true).
    pub only_unassigned: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualAssignment {
    pub participant_id: Uuid,
    pub breakout_id: Uuid,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoomAssignmentsPatch {
    #[serde(default)]
    pub add: Vec<Uuid>,
    #[serde(default)]
    pub remove: Vec<Uuid>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveParticipantRequest {
    pub participant_id: Uuid,
    /// Target breakout; null = back to the main conference.
    pub to_breakout_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloseBreakoutRequest {
    /// Grace period before participants return (0 = immediate, max 600).
    pub countdown_seconds: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BreakoutAnnouncementRequest {
    pub message: String,
}

pub struct BreakoutService {
    core: Core,
    rtc: RtcControl,
}

fn enforcement_name(e: Enforcement) -> String {
    match e {
        Enforcement::Enforced => "ENFORCED",
        Enforcement::Pending => "PENDING_PROVIDER_CONTROL",
        Enforcement::NotRequired => "NOT_REQUIRED",
    }
    .into()
}

impl BreakoutService {
    pub fn new(core: Core) -> Self {
        Self {
            rtc: RtcControl::new(core.clone()),
            core,
        }
    }

    async fn dispatch(&self, conference_id: Uuid) -> Enforcement {
        if self.rtc.room_control() {
            self.rtc.dispatch_for_conference(conference_id).await
        } else {
            Enforcement::NotRequired
        }
    }

    fn is_manager(c: &Collab) -> bool {
        c.allows(Permission::BreakoutAssign) || c.allows(Permission::BreakoutEnterAny)
    }

    pub async fn list(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<(Vec<BreakoutView>, Option<MediaContext>)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let rooms = breakout_rooms::Entity::find()
            .filter(breakout_rooms::Column::ConferenceId.eq(conference_id))
            .order_by_asc(breakout_rooms::Column::Position)
            .all(&self.core.db)
            .await?;
        let members = assignments_by_room(&self.core.db, conference_id).await?;
        let manager = Self::is_manager(&c);
        let me = match &c.participant {
            Some(p) => Some(media_context(&self.core.db, &c.conference, p).await?),
            None => None,
        };
        Ok((
            rooms.iter().map(|r| view(r, &members, manager)).collect(),
            me,
        ))
    }

    pub async fn get(&self, conference_id: Uuid, breakout_id: Uuid) -> ApiResult<BreakoutView> {
        let room = scoped_room(&self.core.db, conference_id, breakout_id).await?;
        let members = assignments_by_room(&self.core.db, conference_id).await?;
        Ok(view(&room, &members, true))
    }

    pub async fn create(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: CreateBreakoutsRequest,
    ) -> ApiResult<Vec<BreakoutView>> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::BreakoutCreate,
            "Your role does not allow creating breakout rooms",
        )?;
        c.require_configurable()?;
        let names: Vec<Option<String>> = match (&req.names, req.count) {
            (Some(n), None) if !n.is_empty() => {
                n.iter().map(|s| Some(s.trim().to_string())).collect()
            }
            (None, Some(k)) if k > 0 => (0..k).map(|_| None).collect(),
            _ => {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "Provide either a non-empty `names` list or a positive `count`",
                ))
            }
        };
        if let Some(cap) = req.capacity {
            if !(1..=1000).contains(&cap) {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "capacity must be 1–1000",
                ));
            }
        }
        let mut seen = HashSet::new();
        for n in names.iter().flatten() {
            if n.is_empty() || n.chars().count() > 100 {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "Breakout names must be 1–100 characters",
                ));
            }
            if !seen.insert(n.to_lowercase()) {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "Breakout names must be unique",
                ));
            }
        }
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let existing = breakout_rooms::Entity::find()
            .filter(breakout_rooms::Column::ConferenceId.eq(conference_id))
            .all(&txn)
            .await?;
        let active: Vec<&breakout_rooms::Model> =
            existing.iter().filter(|r| r.status != "CLOSED").collect();
        if active.len() + names.len() > c.policy.max_breakouts as usize {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                format!(
                    "At most {} breakout rooms are allowed",
                    c.policy.max_breakouts
                ),
            ));
        }
        let taken: HashSet<String> = active.iter().map(|r| r.name.to_lowercase()).collect();
        if let Some(dup) = names
            .iter()
            .flatten()
            .find(|n| taken.contains(&n.to_lowercase()))
        {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                format!("A breakout room named '{dup}' already exists"),
            ));
        }
        let mut position = existing.iter().map(|r| r.position).max().unwrap_or(0);
        let mut number = existing.len();
        let now = Utc::now();
        let policy = req
            .recording_policy
            .unwrap_or(c.policy.breakout_recording_policy);
        let mut created = vec![];
        for name in names {
            position += 1;
            let name = match name {
                Some(n) => n,
                None => loop {
                    number += 1;
                    let candidate = format!("Room {number}");
                    if !taken.contains(&candidate.to_lowercase()) {
                        break candidate;
                    }
                },
            };
            let id = Uuid::new_v4();
            let room = breakout_rooms::ActiveModel {
                id: Set(id),
                conference_id: Set(conference_id),
                name: Set(name),
                status: Set("CREATED".into()),
                position: Set(position),
                rtc_room_name: Set(Some(breakout_room_name(id))),
                created_at: Set(now),
                updated_at: Set(now),
                opened_at: Set(None),
                closed_at: Set(None),
                capacity: Set(req.capacity),
                created_by: Set(Some(c.audit.reference())),
                closing_at: Set(None),
                close_requested_at: Set(None),
                recording_policy: Set(policy.as_str().into()),
                config: Set(json!({})),
                rtc_room_ready_at: Set(None),
                version: Set(1),
            }
            .insert(&txn)
            .await?;
            emit(
                &txn,
                room_record(&room, conference.meeting_space_id, event::BREAKOUT_CREATED),
            )
            .await?;
            created.push(room);
        }
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::BREAKOUT_CREATE,
                target_type: Some("CONFERENCE"),
                target_ref: Some(conference_id.to_string()),
                metadata: json!({ "rooms": created.iter().map(|r| r.id).collect::<Vec<_>>() }),
            },
        )
        .await?;
        txn.commit().await?;
        for _ in &created {
            inc(&CM.breakouts_created);
        }
        let empty = HashMap::new();
        Ok(created.iter().map(|r| view(r, &empty, true)).collect())
    }

    pub async fn open(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        breakout_id: Uuid,
    ) -> ApiResult<(BreakoutView, bool, Enforcement)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let _ = scoped_room(&self.core.db, conference_id, breakout_id).await?;
        c.require(
            Permission::BreakoutCreate,
            "Your role does not allow opening breakout rooms",
        )?;
        c.require_live()?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let room = scoped_room(&txn, conference_id, breakout_id).await?;
        let status = BreakoutStatus::parse(&room.status);
        if status.is_active() {
            txn.commit().await?;
            return Ok((
                self.get(conference_id, breakout_id).await?,
                false,
                Enforcement::NotRequired,
            ));
        }
        if status == BreakoutStatus::Closed {
            return Err(closed());
        }
        // Context of assigned participants before opening (MAIN or a visit).
        let members: Vec<conference_participants::Model> = {
            let ids: Vec<Uuid> = breakout_assignments::Entity::find()
                .filter(breakout_assignments::Column::BreakoutRoomId.eq(room.id))
                .filter(breakout_assignments::Column::UnassignedAt.is_null())
                .all(&txn)
                .await?
                .into_iter()
                .map(|a| a.participant_id)
                .collect();
            conference_participants::Entity::find()
                .filter(conference_participants::Column::Id.is_in(ids))
                .filter(conference_participants::Column::ConferenceId.eq(conference_id))
                .all(&txn)
                .await?
        };
        let mut before = vec![];
        for m in &members {
            before.push(media_context(&txn, &conference, m).await?);
        }
        let now = Utc::now();
        let mut am: breakout_rooms::ActiveModel = room.clone().into();
        am.status = Set("OPEN".into());
        am.opened_at = Set(Some(now));
        am.version = Set(room.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::BREAKOUT_OPEN,
                target_type: Some("BREAKOUT"),
                target_ref: Some(updated.id.to_string()),
                metadata: json!({ "assigned": members.len() }),
            },
        )
        .await?;
        emit(
            &txn,
            room_record(
                &updated,
                conference.meeting_space_id,
                event::BREAKOUT_OPENED,
            ),
        )
        .await?;
        if self.rtc.room_control() {
            emit(
                &txn,
                OutboxRecord {
                    aggregate_type: "breakout_room",
                    aggregate_id: updated.id,
                    event_type: event::RTC_BREAKOUT_PREPARE_REQUESTED,
                    meeting_space_id: Some(conference.meeting_space_id),
                    conference_id: Some(conference_id),
                    payload: json!({ "conferenceId": conference_id, "breakoutId": updated.id }),
                },
            )
            .await?;
        }
        for (m, from) in members.iter().zip(before) {
            let to = media_context(&txn, &conference, m).await?;
            record_move(&txn, &conference, m.id, &from, &to).await?;
        }
        txn.commit().await?;
        inc(&CM.breakouts_opened);
        let enforcement = self.dispatch(conference_id).await;
        Ok((
            self.get(conference_id, breakout_id).await?,
            true,
            enforcement,
        ))
    }

    /// Validated target rooms for assignment (CREATED or OPEN; never CLOSING/CLOSED).
    async fn target_rooms(
        &self,
        txn: &DatabaseTransaction,
        conference_id: Uuid,
        ids: Option<&Vec<Uuid>>,
    ) -> ApiResult<Vec<breakout_rooms::Model>> {
        let rooms = match ids {
            Some(ids) => {
                let mut out = vec![];
                for id in ids {
                    out.push(scoped_room(txn, conference_id, *id).await?);
                }
                out
            }
            None => {
                breakout_rooms::Entity::find()
                    .filter(breakout_rooms::Column::ConferenceId.eq(conference_id))
                    .filter(breakout_rooms::Column::Status.is_in(["CREATED", "OPEN"]))
                    .order_by_asc(breakout_rooms::Column::Position)
                    .all(txn)
                    .await?
            }
        };
        if rooms
            .iter()
            .any(|r| matches!(r.status.as_str(), "CLOSING" | "CLOSED"))
        {
            return Err(closed());
        }
        if rooms.is_empty() {
            return Err(ApiError::domain(
                ErrorCode::BreakoutNotFound,
                "There are no breakout rooms to assign to",
            ));
        }
        Ok(rooms)
    }

    pub async fn assign(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: AssignBreakoutsRequest,
    ) -> ApiResult<AssignmentResult> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::BreakoutAssign,
            "Your role does not allow assigning breakout rooms",
        )?;
        c.require_configurable()?;
        let mode = req.mode.unwrap_or(if req.assignments.is_some() {
            AssignmentMode::Manual
        } else {
            AssignmentMode::Balanced
        });
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let by = c.audit.reference();
        let mut pairs: Vec<(Uuid, Uuid)> = vec![];
        let mut unassigned = vec![];
        let rooms: HashMap<Uuid, breakout_rooms::Model>;
        match mode {
            AssignmentMode::Manual => {
                let list = req.assignments.clone().unwrap_or_default();
                if list.is_empty() {
                    return Err(ApiError::domain(
                        ErrorCode::ValidationError,
                        "MANUAL assignment needs `assignments`",
                    ));
                }
                let ids: Vec<Uuid> = list
                    .iter()
                    .map(|a| a.breakout_id)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                rooms = self
                    .target_rooms(&txn, conference_id, Some(&ids))
                    .await?
                    .into_iter()
                    .map(|r| (r.id, r))
                    .collect();
                let mut seen = HashSet::new();
                for a in &list {
                    if !seen.insert(a.participant_id) {
                        return Err(ApiError::domain(
                            ErrorCode::BreakoutAssignmentConflict,
                            "A participant can only be assigned to one breakout",
                        ));
                    }
                    pairs.push((a.participant_id, a.breakout_id));
                }
            }
            AssignmentMode::Random | AssignmentMode::Balanced => {
                if req.assignments.is_some() {
                    return Err(ApiError::domain(
                        ErrorCode::ValidationError,
                        "`assignments` is only valid for MANUAL mode",
                    ));
                }
                let target = self
                    .target_rooms(&txn, conference_id, req.breakout_ids.as_ref())
                    .await?;
                let current = assignments_by_room(&txn, conference_id).await?;
                let assigned: HashSet<Uuid> = current.values().flatten().copied().collect();
                let only_unassigned = req.only_unassigned.unwrap_or(true);
                let include_viewers = req.include_viewers.unwrap_or(true);
                let eligible: Vec<Uuid> = conference_participants::Entity::find()
                    .filter(conference_participants::Column::ConferenceId.eq(conference_id))
                    .filter(
                        conference_participants::Column::AdmissionState
                            .eq(AdmissionState::Admitted),
                    )
                    .order_by_asc(conference_participants::Column::AdmittedAt)
                    .order_by_asc(conference_participants::Column::Id)
                    .all(&txn)
                    .await?
                    .into_iter()
                    .filter(|p| {
                        !matches!(p.identity_type, IdentityType::Service | IdentityType::Bot)
                    })
                    .filter(|p| req.include_hosts || p.role.rank() < Role::CoHost.rank())
                    .filter(|p| include_viewers || p.role != Role::Viewer)
                    .filter(|p| !only_unassigned || !assigned.contains(&p.id))
                    .map(|p| p.id)
                    .collect();
                let occupancy: Vec<(Uuid, usize, Option<usize>)> = target
                    .iter()
                    .map(|r| {
                        let n = current
                            .get(&r.id)
                            .map(|v| v.iter().filter(|pid| !eligible.contains(pid)).count())
                            .unwrap_or(0);
                        (r.id, n, r.capacity.map(|c| c as usize))
                    })
                    .collect();
                pairs = if mode == AssignmentMode::Balanced {
                    balanced_assignment(&eligible, &occupancy)
                } else {
                    random_assignment(&eligible, &occupancy, &mut rand::thread_rng())
                };
                let placed: HashSet<Uuid> = pairs.iter().map(|(p, _)| *p).collect();
                unassigned = eligible
                    .into_iter()
                    .filter(|p| !placed.contains(p))
                    .collect();
                rooms = target.into_iter().map(|r| (r.id, r)).collect();
            }
        }
        // Apply (participants locked in a stable order under the conference lock).
        pairs.sort_by_key(|(p, _)| *p);
        let mut counts = assignments_by_room(&txn, conference_id).await?;
        for (pid, bid) in &pairs {
            let participant = find_scoped_participant(&txn, conference_id, *pid, true).await?;
            if participant.admission_state != AdmissionState::Admitted {
                return Err(ApiError::domain(
                    ErrorCode::ParticipantStateConflict,
                    "Only admitted participants can be assigned",
                ));
            }
            let room = rooms.get(bid).ok_or_else(not_found)?;
            if mode == AssignmentMode::Manual {
                if let Some(cap) = room.capacity {
                    let already = counts.get(bid).map(|v| v.contains(pid)).unwrap_or(false);
                    let n = counts.get(bid).map(|v| v.len()).unwrap_or(0);
                    if !already && n >= cap as usize {
                        return Err(ApiError::domain(
                            ErrorCode::BreakoutAssignmentConflict,
                            format!("Breakout '{}' is full", room.name),
                        ));
                    }
                }
            }
            if set_assignment(
                &txn,
                &conference,
                &participant,
                Some(room),
                mode.as_str(),
                &by,
            )
            .await?
            {
                for v in counts.values_mut() {
                    v.retain(|x| x != pid);
                }
                counts.entry(*bid).or_default().push(*pid);
            }
        }
        let assigned: Vec<AssignmentPair> = pairs
            .iter()
            .map(|(p, b)| AssignmentPair {
                participant_id: *p,
                breakout_id: *b,
            })
            .collect();
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::BREAKOUT_ASSIGN,
                target_type: Some("CONFERENCE"),
                target_ref: Some(conference_id.to_string()),
                metadata: json!({ "mode": mode.as_str(), "assigned": assigned.len(), "unassigned": unassigned.len() }),
            },
        )
        .await?;
        emit_to(
            &txn,
            OutboxRecord {
                aggregate_type: "conference",
                aggregate_id: conference_id,
                event_type: event::BREAKOUT_ASSIGNMENT_CHANGED,
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference_id),
                payload: json!({ "mode": mode.as_str(), "assignments": assigned }),
            },
            Some(Audience::Permission {
                permission: Permission::BreakoutAssign.key().into(),
            }),
        )
        .await?;
        txn.commit().await?;
        let enforcement = self.dispatch(conference_id).await;
        Ok(AssignmentResult {
            assigned,
            unassigned,
            enforcement: enforcement_name(enforcement),
        })
    }

    /// PATCH one room's assignments (`add` moves into this room, `remove` returns to main).
    pub async fn patch_room(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        breakout_id: Uuid,
        req: RoomAssignmentsPatch,
    ) -> ApiResult<BreakoutView> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let _ = scoped_room(&self.core.db, conference_id, breakout_id).await?;
        c.require(
            Permission::BreakoutAssign,
            "Your role does not allow assigning breakout rooms",
        )?;
        c.require_configurable()?;
        if req.add.is_empty() && req.remove.is_empty() {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "Nothing to change",
            ));
        }
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let room = scoped_room(&txn, conference_id, breakout_id).await?;
        if !req.add.is_empty() && matches!(room.status.as_str(), "CLOSING" | "CLOSED") {
            return Err(closed());
        }
        let by = c.audit.reference();
        let mut ids: Vec<Uuid> = req.add.iter().chain(req.remove.iter()).copied().collect();
        ids.sort();
        ids.dedup();
        for pid in ids {
            let participant = find_scoped_participant(&txn, conference_id, pid, true).await?;
            if req.add.contains(&pid) {
                if participant.admission_state != AdmissionState::Admitted {
                    return Err(ApiError::domain(
                        ErrorCode::ParticipantStateConflict,
                        "Only admitted participants can be assigned",
                    ));
                }
                if let Some(cap) = room.capacity {
                    let n = breakout_assignments::Entity::find()
                        .filter(breakout_assignments::Column::BreakoutRoomId.eq(room.id))
                        .filter(breakout_assignments::Column::UnassignedAt.is_null())
                        .filter(breakout_assignments::Column::ParticipantId.ne(pid))
                        .all(&txn)
                        .await?
                        .len();
                    if n >= cap as usize {
                        return Err(ApiError::domain(
                            ErrorCode::BreakoutAssignmentConflict,
                            format!("Breakout '{}' is full", room.name),
                        ));
                    }
                }
                set_assignment(&txn, &conference, &participant, Some(&room), "MANUAL", &by).await?;
            } else if active_assignment(&txn, pid)
                .await?
                .map(|a| a.breakout_room_id)
                == Some(room.id)
            {
                set_assignment(&txn, &conference, &participant, None, "MANUAL", &by).await?;
            }
        }
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::BREAKOUT_ASSIGN,
                target_type: Some("BREAKOUT"),
                target_ref: Some(room.id.to_string()),
                metadata: json!({ "added": req.add.len(), "removed": req.remove.len() }),
            },
        )
        .await?;
        emit_to(
            &txn,
            OutboxRecord {
                aggregate_type: "breakout_room",
                aggregate_id: room.id,
                event_type: event::BREAKOUT_ASSIGNMENT_CHANGED,
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference_id),
                payload: json!({ "breakoutId": room.id, "added": req.add, "removed": req.remove }),
            },
            Some(Audience::Permission {
                permission: Permission::BreakoutAssign.key().into(),
            }),
        )
        .await?;
        txn.commit().await?;
        self.dispatch(conference_id).await;
        self.get(conference_id, breakout_id).await
    }

    /// main → breakout, breakout A → B, breakout → main.
    pub async fn move_participant(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: MoveParticipantRequest,
    ) -> ApiResult<(MediaContext, bool, Enforcement)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let _ = find_scoped_participant(&self.core.db, conference_id, req.participant_id, false)
            .await?;
        if let Some(b) = req.to_breakout_id {
            let _ = scoped_room(&self.core.db, conference_id, b).await?;
        }
        c.require(
            Permission::BreakoutMove,
            "Your role does not allow moving participants",
        )?;
        c.require_live()?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let participant =
            find_scoped_participant(&txn, conference_id, req.participant_id, true).await?;
        if participant.admission_state != AdmissionState::Admitted {
            return Err(ApiError::domain(
                ErrorCode::ParticipantStateConflict,
                "Only admitted participants can be moved",
            ));
        }
        let room = match req.to_breakout_id {
            Some(b) => {
                let r = scoped_room(&txn, conference_id, b).await?;
                if matches!(r.status.as_str(), "CLOSING" | "CLOSED") {
                    return Err(closed());
                }
                if let Some(cap) = r.capacity {
                    let n = breakout_assignments::Entity::find()
                        .filter(breakout_assignments::Column::BreakoutRoomId.eq(r.id))
                        .filter(breakout_assignments::Column::UnassignedAt.is_null())
                        .filter(breakout_assignments::Column::ParticipantId.ne(participant.id))
                        .all(&txn)
                        .await?
                        .len();
                    if n >= cap as usize {
                        return Err(ApiError::domain(
                            ErrorCode::BreakoutAssignmentConflict,
                            format!("Breakout '{}' is full", r.name),
                        ));
                    }
                }
                Some(r)
            }
            None => None,
        };
        let changed = set_assignment(
            &txn,
            &conference,
            &participant,
            room.as_ref(),
            "MOVE",
            &c.audit.reference(),
        )
        .await?;
        if changed {
            audit(
                &txn,
                &c.audit,
                AuditRecord {
                    meeting_space_id: conference.meeting_space_id,
                    conference_id: Some(conference_id),
                    action: action::BREAKOUT_MOVE,
                    target_type: Some("PARTICIPANT"),
                    target_ref: Some(participant.id.to_string()),
                    metadata: json!({ "toBreakoutId": req.to_breakout_id }),
                },
            )
            .await?;
        }
        let ctx_now = media_context(&txn, &conference, &participant).await?;
        txn.commit().await?;
        if changed {
            inc(&CM.breakout_moves);
        }
        let enforcement = if changed {
            self.dispatch(conference_id).await
        } else {
            Enforcement::NotRequired
        };
        Ok((ctx_now, changed, enforcement))
    }

    /// Privileged host/co-host visit (`None` = leave the visit, back to the previous
    /// assignment or main). Never changes assignments.
    pub async fn visit(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        breakout_id: Option<Uuid>,
    ) -> ApiResult<(MediaContext, Enforcement)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        if let Some(b) = breakout_id {
            let _ = scoped_room(&self.core.db, conference_id, b).await?;
        }
        c.require(
            Permission::BreakoutEnterAny,
            "Your role does not allow entering breakout rooms",
        )?;
        c.require_live()?;
        let me = c.me()?.id;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let participant = find_scoped_participant(&txn, conference_id, me, true).await?;
        if let Some(b) = breakout_id {
            let room = scoped_room(&txn, conference_id, b).await?;
            if !BreakoutStatus::parse(&room.status).is_active() {
                return Err(ApiError::domain(
                    ErrorCode::BreakoutClosed,
                    "Only open breakout rooms can be visited",
                ));
            }
        }
        if participant.breakout_visit_room_id == breakout_id {
            let ctx_now = media_context(&txn, &conference, &participant).await?;
            txn.commit().await?;
            return Ok((ctx_now, Enforcement::NotRequired));
        }
        let from = media_context(&txn, &conference, &participant).await?;
        let mut am: conference_participants::ActiveModel = participant.clone().into();
        am.breakout_visit_room_id = Set(breakout_id);
        am.collab_version = Set(participant.collab_version + 1);
        am.updated_at = Set(Utc::now());
        let updated = am.update(&txn).await?;
        let to = media_context(&txn, &conference, &updated).await?;
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::BREAKOUT_VISIT,
                target_type: Some("BREAKOUT"),
                target_ref: breakout_id.map(|b| b.to_string()),
                metadata: json!({ "enter": breakout_id.is_some() }),
            },
        )
        .await?;
        record_move(&txn, &conference, updated.id, &from, &to).await?;
        txn.commit().await?;
        let enforcement = self.dispatch(conference_id).await;
        Ok((to, enforcement))
    }

    /// Authoritative host announcement to open breakouts (not cross-room chat).
    pub async fn announce(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        breakout_id: Option<Uuid>,
        req: BreakoutAnnouncementRequest,
    ) -> ApiResult<u32> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        if let Some(b) = breakout_id {
            let _ = scoped_room(&self.core.db, conference_id, b).await?;
        }
        c.require(
            Permission::BreakoutAnnounce,
            "Your role does not allow breakout announcements",
        )?;
        c.require_live()?;
        let message = req.message.trim().to_string();
        if message.is_empty() || message.chars().count() > 1000 {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "Announcements must be 1–1000 characters",
            ));
        }
        let subject = c
            .participant
            .as_ref()
            .map(|p| p.id.to_string())
            .unwrap_or_else(|| c.audit.reference());
        self.core
            .collab_limiter
            .check(RateAction::Announcement, &subject)
            .await?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let mut q = breakout_rooms::Entity::find()
            .filter(breakout_rooms::Column::ConferenceId.eq(conference_id))
            .filter(breakout_rooms::Column::Status.is_in(["OPEN", "CLOSING"]));
        if let Some(b) = breakout_id {
            q = q.filter(breakout_rooms::Column::Id.eq(b));
        }
        let open = q.all(&txn).await?;
        if open.is_empty() {
            return Err(ApiError::domain(
                ErrorCode::BreakoutClosed,
                "There are no open breakout rooms",
            ));
        }
        let from = c.participant.as_ref().map(|p| p.display_name.clone());
        emit_to(
            &txn,
            OutboxRecord {
                aggregate_type: "conference",
                aggregate_id: conference_id,
                event_type: event::BREAKOUT_ANNOUNCEMENT,
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference_id),
                payload: json!({
                    "conferenceId": conference_id,
                    "breakoutId": breakout_id,
                    "message": message,
                    "fromDisplayName": from,
                    "fromParticipantId": c.participant.as_ref().map(|p| p.id),
                }),
            },
            Some(match breakout_id {
                Some(b) => Audience::Breakout { breakout_id: b },
                None => Audience::Breakouts,
            }),
        )
        .await?;
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::BREAKOUT_ANNOUNCE,
                target_type: Some("CONFERENCE"),
                target_ref: breakout_id.map(|b| b.to_string()),
                // Content is delivered, not audited.
                metadata: json!({ "rooms": open.len(), "length": message.chars().count() }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(open.len() as u32)
    }

    /// OPEN → CLOSING (persisted deadline) → CLOSED; countdown 0 closes immediately.
    pub async fn close(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        breakout_id: Option<Uuid>,
        req: CloseBreakoutRequest,
    ) -> ApiResult<(Vec<BreakoutView>, Enforcement)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        if let Some(b) = breakout_id {
            let _ = scoped_room(&self.core.db, conference_id, b).await?;
        }
        c.require(
            Permission::BreakoutClose,
            "Your role does not allow closing breakout rooms",
        )?;
        c.require_configurable()?;
        let countdown = req.countdown_seconds.unwrap_or(60);
        if countdown > MAX_COUNTDOWN_SECS {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                format!("countdownSeconds must be at most {MAX_COUNTDOWN_SECS}"),
            ));
        }
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let rooms = match breakout_id {
            Some(b) => vec![scoped_room(&txn, conference_id, b).await?],
            None => {
                breakout_rooms::Entity::find()
                    .filter(breakout_rooms::Column::ConferenceId.eq(conference_id))
                    .filter(breakout_rooms::Column::Status.ne("CLOSED"))
                    .order_by_asc(breakout_rooms::Column::Position)
                    .all(&txn)
                    .await?
            }
        };
        let now = Utc::now();
        let mut touched = vec![];
        for room in rooms {
            let status = BreakoutStatus::parse(&room.status);
            let result = match status {
                BreakoutStatus::Closed => room,
                BreakoutStatus::Created => {
                    finalize_close(&txn, &conference, room, &c.audit).await?
                }
                _ if countdown == 0 => finalize_close(&txn, &conference, room, &c.audit).await?,
                BreakoutStatus::Closing => room,
                BreakoutStatus::Open => {
                    let mut am: breakout_rooms::ActiveModel = room.clone().into();
                    am.status = Set("CLOSING".into());
                    am.closing_at = Set(Some(now + Duration::seconds(countdown as i64)));
                    am.close_requested_at = Set(Some(now));
                    am.version = Set(room.version + 1);
                    am.updated_at = Set(now);
                    let updated = am.update(&txn).await?;
                    audit(
                        &txn,
                        &c.audit,
                        AuditRecord {
                            meeting_space_id: conference.meeting_space_id,
                            conference_id: Some(conference_id),
                            action: action::BREAKOUT_CLOSE,
                            target_type: Some("BREAKOUT"),
                            target_ref: Some(updated.id.to_string()),
                            metadata: json!({ "countdownSeconds": countdown }),
                        },
                    )
                    .await?;
                    emit(
                        &txn,
                        room_record(
                            &updated,
                            conference.meeting_space_id,
                            event::BREAKOUT_CLOSING,
                        ),
                    )
                    .await?;
                    updated
                }
            };
            touched.push(result.id);
        }
        txn.commit().await?;
        let enforcement = self.dispatch(conference_id).await;
        let members = assignments_by_room(&self.core.db, conference_id).await?;
        let mut out = vec![];
        for id in touched {
            out.push(view(
                &scoped_room(&self.core.db, conference_id, id).await?,
                &members,
                true,
            ));
        }
        Ok((out, enforcement))
    }
}

/// Restart-safe countdown completion: due CLOSING rooms are finalized under the
/// conference row lock (idempotent; safe with several instances).
pub async fn close_due(core: &Core, limit: u64) -> ApiResult<u32> {
    use sea_orm::QuerySelect;
    let due = breakout_rooms::Entity::find()
        .filter(breakout_rooms::Column::Status.eq("CLOSING"))
        .filter(breakout_rooms::Column::ClosingAt.lte(Utc::now()))
        .order_by_asc(breakout_rooms::Column::ClosingAt)
        .limit(limit)
        .all(&core.db)
        .await?;
    let rtc = RtcControl::new(core.clone());
    let mut closed_n = 0;
    for r in due {
        let txn = core.db.begin().await?;
        let conference = lock_conference(&txn, r.conference_id).await?;
        let room = scoped_room(&txn, r.conference_id, r.id).await?;
        if room.status == "CLOSING" && room.closing_at.map(|t| t <= Utc::now()).unwrap_or(true) {
            finalize_close(
                &txn,
                &conference,
                room,
                &AuditActor::system("breakout-closer"),
            )
            .await?;
            closed_n += 1;
        }
        txn.commit().await?;
        if rtc.room_control() {
            rtc.dispatch_for_conference(conference.id).await;
        }
    }
    Ok(closed_n)
}

/// Conference of a breakout room (webhook / reconciler mapping).
pub async fn conference_of(
    core: &Core,
    room: &breakout_rooms::Model,
) -> ApiResult<conference_sessions::Model> {
    find_conference(&core.db, room.conference_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn ids(n: usize) -> Vec<Uuid> {
        (0..n).map(|_| Uuid::new_v4()).collect()
    }

    #[test]
    fn balanced_distributes_evenly_and_respects_existing_load() {
        let people = ids(10);
        let rooms = ids(3);
        let pairs = balanced_assignment(
            &people,
            &[
                (rooms[0], 0, None),
                (rooms[1], 0, None),
                (rooms[2], 0, None),
            ],
        );
        assert_eq!(pairs.len(), 10);
        let count = |r| pairs.iter().filter(|(_, x)| *x == r).count();
        let counts: Vec<usize> = rooms.iter().map(|r| count(*r)).collect();
        assert!(
            counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1,
            "{counts:?}"
        );
        // Existing occupancy is filled around.
        let pairs = balanced_assignment(&people[..4], &[(rooms[0], 3, None), (rooms[1], 0, None)]);
        assert_eq!(
            pairs.iter().filter(|(_, r)| *r == rooms[1]).count(),
            3,
            "fills the empty room up to the busy one"
        );
        // Capacity is a hard limit.
        let pairs = balanced_assignment(&people, &[(rooms[0], 0, Some(2)), (rooms[1], 0, Some(3))]);
        assert_eq!(pairs.len(), 5);
    }

    #[test]
    fn random_assignment_invariants() {
        let people = ids(40);
        let rooms = ids(4);
        let occ: Vec<_> = rooms.iter().map(|r| (*r, 0, Some(12))).collect();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let pairs = random_assignment(&people, &occ, &mut rng);
        assert_eq!(pairs.len(), 40, "everyone placed");
        let unique: HashSet<_> = pairs.iter().map(|(p, _)| *p).collect();
        assert_eq!(unique.len(), 40, "one assignment per participant");
        for r in &rooms {
            assert!(
                pairs.iter().filter(|(_, x)| x == r).count() <= 12,
                "capacity respected"
            );
        }
        assert!(
            pairs.iter().all(|(_, r)| rooms.contains(r)),
            "only target rooms"
        );
    }

    #[test]
    fn breakout_transitions_are_strict() {
        use BreakoutStatus::*;
        assert!(breakout_transition_allowed(Created, Open));
        assert!(breakout_transition_allowed(Open, Closing));
        assert!(breakout_transition_allowed(Closing, Closed));
        assert!(breakout_transition_allowed(Open, Closed));
        assert!(!breakout_transition_allowed(Closed, Open), "never reopened");
        assert!(!breakout_transition_allowed(Closing, Open));
        assert!(!breakout_transition_allowed(Created, Closing));
    }

    #[test]
    fn breakout_rooms_use_a_distinct_prefix() {
        let id = Uuid::new_v4();
        let name = breakout_room_name(id);
        assert!(name.starts_with("cfbr-"));
        assert!(
            !name.starts_with("cf-"),
            "Part E conference room scans never match"
        );
    }
}
