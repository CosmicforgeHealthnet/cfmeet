//! ConferenceService — occurrences and the conference state machine (spec §C9–§C14).
//!
//! ```text
//! CREATED ─prepare→ PREPARING ─(provider room confirmed)→ WAITING
//! PREPARING|WAITING ─start→ LIVE          (CREATED start = prepare+start atomically)
//! LIVE ─end→ ENDING ─(provider terminated + absence confirmed)→ ENDED
//! CREATED|PREPARING|WAITING ─cancel→ CANCELLED
//! PREPARING|LIVE|ENDING ─fail→ FAILED
//! ```
//!
//! Every transition runs under `SELECT … FOR UPDATE` on the conference row; a partial
//! unique index guarantees one LIVE v2 conference per meeting space.
//!
//! Part E: with provider control active (`rtc_provider_control_v2` + a provider that
//! controls rooms), WAITING and ENDED are only reached through [`RtcControl`] after the
//! provider confirmed the room / its termination. Provider calls never run inside the
//! business transaction; `rtc.*` commands are committed with the transition and
//! dispatched right after commit (retried by the outbox relay and reconciler).

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::{ConferenceKind, ConferenceStatus, RecordOrigin, SpaceStatus, TerminationReason},
    dto::v2::{CreateConferenceRequest, RescheduleConferenceRequest},
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_sessions, meeting_spaces},
    services::{
        actor::ActorContext,
        meeting_core::{
            conference_authority, find_conference, find_space, is_unique_violation,
            lock_conference, permission_denied, space_role, state_conflict, validation,
            ConferenceAuthority, Core,
        },
        meeting_events::{action, audit, emit, event, AuditActor, AuditRecord, OutboxRecord},
        meeting_permissions::{space_allows, Permission, PermissionContext, SpacePermission},
        meeting_space::schedule_policy,
        rtc_control::{Enforcement, RtcControl},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Prepare,
    MarkReady,
    Start,
    End,
    CompleteEnd,
    Cancel,
    Fail,
}

/// Pure state machine. `Ok(None)` = idempotent no-op (already in target state).
pub fn next_status(
    current: ConferenceStatus,
    t: Transition,
) -> Result<Option<ConferenceStatus>, String> {
    use ConferenceStatus::*;
    let next = match (t, current) {
        (Transition::Prepare, Created) => Preparing,
        (Transition::Prepare, Preparing | Waiting) => return Ok(None),
        (Transition::MarkReady, Preparing) => Waiting,
        (Transition::MarkReady, Waiting) => return Ok(None),
        (Transition::Start, Created | Preparing | Waiting) => Live,
        (Transition::Start, Live) => return Ok(None),
        (Transition::End, Live) => Ending,
        // Repeated END (double click, retries) is an idempotent no-op once closing.
        (Transition::End, Ending | Ended) => return Ok(None),
        (Transition::CompleteEnd, Ending) => Ended,
        (Transition::CompleteEnd, Ended) => return Ok(None),
        (Transition::Cancel, Created | Preparing | Waiting) => Cancelled,
        (Transition::Cancel, Cancelled) => return Ok(None),
        (Transition::Fail, Preparing | Live | Ending) => Failed,
        (Transition::Fail, Failed) => return Ok(None),
        (t, s) => return Err(format!("Cannot {t:?} a conference that is {s}")),
    };
    Ok(Some(next))
}

pub struct NewConference {
    pub kind: ConferenceKind,
    pub occurrence_key: String,
    pub scheduled_start_at: Option<DateTime<Utc>>,
    pub scheduled_end_at: Option<DateTime<Utc>>,
    pub recurrence_rule_id: Option<Uuid>,
    /// CREATED for scheduled occurrences, LIVE for instant meetings.
    pub status: ConferenceStatus,
    pub started_by: Option<String>,
}

fn room_name(id: Uuid) -> String {
    format!("cf-{}", id.simple())
}

/// Insert a conference occurrence (and its audit/outbox) on the caller's transaction.
pub async fn insert_conference<C: ConnectionTrait>(
    conn: &C,
    space: &meeting_spaces::Model,
    new: NewConference,
    actor: &AuditActor,
) -> ApiResult<conference_sessions::Model> {
    let now = Utc::now();
    let id = Uuid::new_v4();
    let live = new.status == ConferenceStatus::Live;
    let model = conference_sessions::ActiveModel {
        id: Set(id),
        meeting_space_id: Set(space.id),
        occurrence_key: Set(new.occurrence_key),
        kind: Set(new.kind),
        origin: Set(RecordOrigin::V2),
        legacy_meeting_id: Set(None),
        recurrence_rule_id: Set(new.recurrence_rule_id),
        is_exception: Set(false),
        original_start_at: Set(new.recurrence_rule_id.and(new.scheduled_start_at)),
        status: Set(new.status),
        scheduled_start_at: Set(new.scheduled_start_at),
        scheduled_end_at: Set(new.scheduled_end_at),
        started_at: Set(live.then_some(now)),
        started_by: Set(if live { new.started_by } else { None }),
        ending_requested_at: Set(None),
        ended_at: Set(None),
        ended_by: Set(None),
        cancelled_at: Set(None),
        cancelled_by: Set(None),
        termination_reason: Set(None),
        failure_reason: Set(None),
        is_locked: Set(false),
        locked_at: Set(None),
        locked_by: Set(None),
        rtc_provider: Set("LIVEKIT".into()),
        rtc_room_name: Set(Some(room_name(id))),
        rtc_room_id: Set(None),
        region: Set(None),
        recording_state: Set("OFF".into()),
        transcription_state: Set("OFF".into()),
        created_at: Set(now),
        updated_at: Set(now),
        version: Set(1),
        event_seq: Set(0),
        rtc_room_ready_at: Set(None),
        rtc_room_terminated_at: Set(None),
        collaboration_policy: Set(json!({})),
        collaboration_version: Set(1),
    };
    let conference = model.insert(conn).await.map_err(|e| {
        if is_unique_violation(&e) {
            state_conflict("Another conference of this meeting is already live")
        } else {
            e.into()
        }
    })?;
    audit(
        conn,
        actor,
        AuditRecord {
            meeting_space_id: space.id,
            conference_id: Some(id),
            action: action::CONFERENCE_CREATE,
            target_type: Some("CONFERENCE"),
            target_ref: Some(id.to_string()),
            metadata: json!({ "kind": conference.kind, "status": conference.status }),
        },
    )
    .await?;
    emit(
        conn,
        conference_event(&conference, event::CONFERENCE_CREATED),
    )
    .await?;
    if live {
        emit(
            conn,
            conference_event(&conference, event::CONFERENCE_PREPARING),
        )
        .await?;
        emit(
            conn,
            conference_event(&conference, event::CONFERENCE_STARTED),
        )
        .await?;
    }
    Ok(conference)
}

pub fn conference_event<'a>(
    c: &conference_sessions::Model,
    event_type: &'a str,
) -> OutboxRecord<'a> {
    OutboxRecord {
        aggregate_type: "conference",
        aggregate_id: c.id,
        event_type,
        meeting_space_id: Some(c.meeting_space_id),
        conference_id: Some(c.id),
        payload: json!({
            "conferenceId": c.id,
            "meetingSpaceId": c.meeting_space_id,
            "status": c.status,
            "isLocked": c.is_locked,
            "version": c.version,
        }),
    }
}

/// Result of a state-changing conference operation.
#[derive(Debug, Clone)]
pub struct ConferenceOutcome {
    pub conference: conference_sessions::Model,
    pub changed: bool,
    /// Provider enforcement of the committed transition (Part E).
    pub enforcement: Enforcement,
}

impl ConferenceOutcome {
    fn unchanged(conference: conference_sessions::Model) -> Self {
        Self {
            conference,
            changed: false,
            enforcement: Enforcement::NotRequired,
        }
    }

    fn changed(conference: conference_sessions::Model, enforcement: Enforcement) -> Self {
        Self {
            conference,
            changed: true,
            enforcement,
        }
    }
}

pub struct ConferenceService {
    core: Core,
    rtc: RtcControl,
}

impl ConferenceService {
    pub fn new(core: Core) -> Self {
        Self {
            rtc: RtcControl::new(core.clone()),
            core,
        }
    }

    /// After commit: execute this conference's queued provider commands and reload.
    async fn enforce(&self, id: Uuid) -> ApiResult<(conference_sessions::Model, Enforcement)> {
        let enforcement = self.rtc.dispatch_for_conference(id).await;
        Ok((find_conference(&self.core.db, id).await?, enforcement))
    }

    fn terminate_command(c: &conference_sessions::Model) -> OutboxRecord<'static> {
        OutboxRecord {
            aggregate_type: "conference",
            aggregate_id: c.id,
            event_type: event::RTC_ROOM_TERMINATE_REQUESTED,
            meeting_space_id: Some(c.meeting_space_id),
            conference_id: Some(c.id),
            payload: json!({ "conferenceId": c.id, "terminationReason": c.termination_reason }),
        }
    }

    fn legacy_guard(c: &conference_sessions::Model) -> ApiResult<()> {
        if c.origin == RecordOrigin::Legacy {
            return Err(state_conflict(
                "This conference is managed by the legacy meeting API until migration certification",
            ));
        }
        Ok(())
    }

    async fn authority_for<C: ConnectionTrait>(
        &self,
        conn: &C,
        space: &meeting_spaces::Model,
        conference_id: Uuid,
        ctx: &ActorContext,
        permission: Permission,
    ) -> ApiResult<ConferenceAuthority> {
        let authority = conference_authority(conn, space, conference_id, ctx)
            .await?
            .ok_or_else(|| permission_denied("You are not permitted to manage this conference"))?;
        if !PermissionContext::for_space(space).allows(authority.role, permission) {
            return Err(permission_denied("Your role does not allow this action"));
        }
        Ok(authority)
    }

    /// `POST /api/v2/meeting-spaces/{id}/conferences` — one-time scheduled occurrence.
    pub async fn create_occurrence(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        req: CreateConferenceRequest,
    ) -> ApiResult<conference_sessions::Model> {
        let actor = ctx.require_registered()?;
        self.core.require_v2_write(actor)?;
        if let (Some(s), Some(e)) = (req.scheduled_start_at, req.scheduled_end_at) {
            if e <= s {
                return Err(validation("scheduledEndAt must be after scheduledStartAt"));
            }
            if e - s > Duration::hours(24) {
                return Err(validation(
                    "A conference cannot be scheduled for more than 24 hours",
                ));
            }
        }
        if req.scheduled_end_at.is_some() && req.scheduled_start_at.is_none() {
            return Err(validation("scheduledEndAt requires scheduledStartAt"));
        }
        let txn = self.core.db.begin().await?;
        let space = crate::services::meeting_core::lock_space(&txn, space_id).await?;
        if space.status != SpaceStatus::Active {
            return Err(state_conflict(
                "Archived meeting spaces cannot schedule conferences",
            ));
        }
        let role = space_role(&txn, &space, actor).await?.map(|(r, _)| r);
        let pctx = PermissionContext::for_space(&space);
        if !role
            .map(|r| space_allows(r, SpacePermission::ScheduleConferences, &pctx))
            .unwrap_or(false)
        {
            return Err(permission_denied(
                "You are not permitted to schedule conferences",
            ));
        }
        let conference = insert_conference(
            &txn,
            &space,
            NewConference {
                kind: ConferenceKind::Scheduled,
                occurrence_key: format!("single:{}", Uuid::new_v4()),
                scheduled_start_at: req.scheduled_start_at,
                scheduled_end_at: req.scheduled_end_at,
                recurrence_rule_id: None,
                status: ConferenceStatus::Created,
                started_by: None,
            },
            &AuditActor::from(ctx),
        )
        .await?;
        txn.commit().await?;
        Ok(conference)
    }

    /// Conference plus whether the caller gets the full view.
    pub async fn get(
        &self,
        ctx: &ActorContext,
        id: Uuid,
    ) -> ApiResult<(conference_sessions::Model, meeting_spaces::Model, bool)> {
        let conference = find_conference(&self.core.db, id).await?;
        let space = find_space(&self.core.db, conference.meeting_space_id)
            .await
            .map_err(|_| crate::services::meeting_core::not_found_conference())?;
        let full = conference_authority(&self.core.db, &space, id, ctx)
            .await?
            .is_some();
        Ok((conference, space, full))
    }

    pub async fn list_for_space(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> ApiResult<Vec<conference_sessions::Model>> {
        let actor = ctx.require_registered()?;
        let space = find_space(&self.core.db, space_id).await?;
        let role = space_role(&self.core.db, &space, actor).await?;
        if role.is_none() {
            return Err(permission_denied("You are not a member of this meeting"));
        }
        let mut q = conference_sessions::Entity::find()
            .filter(conference_sessions::Column::MeetingSpaceId.eq(space_id));
        if let Some(f) = from {
            q = q.filter(conference_sessions::Column::ScheduledStartAt.gte(f));
        }
        if let Some(t) = to {
            q = q.filter(conference_sessions::Column::ScheduledStartAt.lt(t));
        }
        Ok(
            q.order_by_asc(conference_sessions::Column::ScheduledStartAt)
                .order_by_asc(conference_sessions::Column::CreatedAt)
                .all(&self.core.db)
                .await?,
        )
    }

    async fn apply<C: ConnectionTrait>(
        &self,
        conn: &C,
        conference: conference_sessions::Model,
        mut am: conference_sessions::ActiveModel,
    ) -> ApiResult<conference_sessions::Model> {
        am.version = Set(conference.version + 1);
        am.updated_at = Set(Utc::now());
        am.update(conn).await.map_err(|e| {
            if is_unique_violation(&e) {
                state_conflict("Another conference of this meeting is already live")
            } else {
                e.into()
            }
        })
    }

    async fn record<C: ConnectionTrait>(
        &self,
        conn: &C,
        actor: &AuditActor,
        c: &conference_sessions::Model,
        audit_action: &str,
        events: &[&str],
        metadata: serde_json::Value,
    ) -> ApiResult<()> {
        audit(
            conn,
            actor,
            AuditRecord {
                meeting_space_id: c.meeting_space_id,
                conference_id: Some(c.id),
                action: audit_action,
                target_type: Some("CONFERENCE"),
                target_ref: Some(c.id.to_string()),
                metadata,
            },
        )
        .await?;
        for e in events {
            emit(conn, conference_event(c, e)).await?;
        }
        Ok(())
    }

    /// CREATED → PREPARING. Provider room preparation is enqueued for Part E.
    pub async fn prepare(&self, ctx: &ActorContext, id: Uuid) -> ApiResult<ConferenceOutcome> {
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, id).await?;
        Self::legacy_guard(&conference)?;
        let space = find_space(&txn, conference.meeting_space_id).await?;
        let authority = self
            .authority_for(&txn, &space, id, ctx, Permission::ConferenceStart)
            .await?;
        let next = next_status(conference.status, Transition::Prepare).map_err(state_conflict)?;
        let Some(next) = next else {
            txn.commit().await?;
            if conference.status == ConferenceStatus::Preparing && self.rtc.room_control() {
                // Re-prepare retries provider confirmation (idempotent).
                if let Ok(ready) = self.rtc.ensure_room(id).await {
                    return Ok(ConferenceOutcome::changed(ready, Enforcement::Enforced));
                }
            }
            return Ok(ConferenceOutcome::unchanged(conference));
        };
        let mut am: conference_sessions::ActiveModel = conference.clone().into();
        am.status = Set(next);
        let updated = self.apply(&txn, conference, am).await?;
        self.record(
            &txn,
            &authority.audit,
            &updated,
            action::CONFERENCE_PREPARE,
            &[
                event::CONFERENCE_PREPARING,
                event::RTC_ROOM_PREPARE_REQUESTED,
            ],
            json!({}),
        )
        .await?;
        txn.commit().await?;
        if !self.rtc.room_control() {
            return Ok(ConferenceOutcome::changed(updated, Enforcement::Pending));
        }
        // Provider confirmation (PREPARING → WAITING) happens only in RtcControl; on
        // provider failure the conference stays PREPARING and the command is retried.
        let (current, enforcement) = self.enforce(id).await?;
        let enforcement = if current.status == ConferenceStatus::Preparing {
            Enforcement::Pending
        } else {
            enforcement
        };
        Ok(ConferenceOutcome::changed(current, enforcement))
    }

    /// PREPARING|WAITING (or CREATED, prepared atomically) → LIVE. Concurrent starts
    /// serialize on the row lock: exactly one wins, the rest are idempotent no-ops.
    pub async fn start(&self, ctx: &ActorContext, id: Uuid) -> ApiResult<ConferenceOutcome> {
        if self.rtc.room_control() {
            self.ensure_ready_for_start(ctx, id).await?;
        }
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, id).await?;
        Self::legacy_guard(&conference)?;
        let space = find_space(&txn, conference.meeting_space_id).await?;
        let authority = self
            .authority_for(&txn, &space, id, ctx, Permission::ConferenceStart)
            .await?;
        let next = next_status(conference.status, Transition::Start).map_err(state_conflict)?;
        let Some(next) = next else {
            txn.commit().await?;
            return Ok(ConferenceOutcome::unchanged(conference));
        };
        if space.status != SpaceStatus::Active {
            return Err(state_conflict(
                "Archived meeting spaces cannot start conferences",
            ));
        }
        check_start_window(&conference, &schedule_policy(&space), Utc::now())?;
        let was_created = conference.status == ConferenceStatus::Created;
        let mut am: conference_sessions::ActiveModel = conference.clone().into();
        am.status = Set(next);
        am.started_at = Set(Some(Utc::now()));
        am.started_by = Set(Some(authority.audit.reference()));
        let updated = self.apply(&txn, conference, am).await?;
        let events: &[&str] = if was_created {
            &[event::CONFERENCE_PREPARING, event::CONFERENCE_STARTED]
        } else {
            &[event::CONFERENCE_STARTED]
        };
        self.record(
            &txn,
            &authority.audit,
            &updated,
            action::CONFERENCE_START,
            events,
            json!({}),
        )
        .await?;
        txn.commit().await?;
        let enforcement = if updated.rtc_room_ready_at.is_some() {
            Enforcement::Enforced
        } else {
            Enforcement::NotRequired
        };
        Ok(ConferenceOutcome::changed(updated, enforcement))
    }

    /// Provider control: a conference only goes LIVE on a provider-confirmed room.
    /// Authorization and the start window are checked BEFORE any provider call; a
    /// provider failure leaves the conference PREPARING (RTC_PROVIDER_UNAVAILABLE).
    async fn ensure_ready_for_start(&self, ctx: &ActorContext, id: Uuid) -> ApiResult<()> {
        let conference = find_conference(&self.core.db, id).await?;
        Self::legacy_guard(&conference)?;
        if !matches!(
            conference.status,
            ConferenceStatus::Created | ConferenceStatus::Preparing | ConferenceStatus::Waiting
        ) {
            return Ok(());
        }
        let space = find_space(&self.core.db, conference.meeting_space_id).await?;
        self.authority_for(&self.core.db, &space, id, ctx, Permission::ConferenceStart)
            .await?;
        if space.status != SpaceStatus::Active {
            return Err(state_conflict(
                "Archived meeting spaces cannot start conferences",
            ));
        }
        check_start_window(&conference, &schedule_policy(&space), Utc::now())?;
        if conference.status == ConferenceStatus::Created {
            self.prepare(ctx, id).await?;
        }
        let current = find_conference(&self.core.db, id).await?;
        if current.status == ConferenceStatus::Preparing || current.rtc_room_ready_at.is_none() {
            self.rtc.ensure_room(id).await?;
        }
        Ok(())
    }

    /// LIVE + locked. Join/admission consult this authoritative flag.
    pub async fn set_lock(
        &self,
        ctx: &ActorContext,
        id: Uuid,
        locked: bool,
    ) -> ApiResult<ConferenceOutcome> {
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, id).await?;
        Self::legacy_guard(&conference)?;
        let space = find_space(&txn, conference.meeting_space_id).await?;
        let authority = self
            .authority_for(&txn, &space, id, ctx, Permission::ConferenceLock)
            .await?;
        if conference.status != ConferenceStatus::Live {
            return Err(ApiError::domain(
                ErrorCode::ConferenceNotLive,
                "Only a live conference can be locked or unlocked",
            ));
        }
        if conference.is_locked == locked {
            txn.commit().await?;
            return Ok(ConferenceOutcome::unchanged(conference));
        }
        let mut am: conference_sessions::ActiveModel = conference.clone().into();
        am.is_locked = Set(locked);
        am.locked_at = Set(locked.then(Utc::now));
        am.locked_by = Set(locked.then(|| authority.audit.reference()));
        let updated = self.apply(&txn, conference, am).await?;
        let (a, e) = if locked {
            (action::CONFERENCE_LOCK, event::CONFERENCE_LOCKED)
        } else {
            (action::CONFERENCE_UNLOCK, event::CONFERENCE_UNLOCKED)
        };
        self.record(&txn, &authority.audit, &updated, a, &[e], json!({}))
            .await?;
        txn.commit().await?;
        Ok(ConferenceOutcome::changed(
            updated,
            Enforcement::NotRequired,
        ))
    }

    /// LIVE → ENDING (atomic). From this commit on no admission and no new usable join
    /// credential is issued. ENDED is only reached once the provider confirms
    /// termination (Part E) — it is never faked here.
    pub async fn end(
        &self,
        ctx: &ActorContext,
        id: Uuid,
        reason: TerminationReason,
    ) -> ApiResult<ConferenceOutcome> {
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, id).await?;
        Self::legacy_guard(&conference)?;
        let space = find_space(&txn, conference.meeting_space_id).await?;
        let authority = self
            .authority_for(&txn, &space, id, ctx, Permission::ConferenceEnd)
            .await?;
        let next = next_status(conference.status, Transition::End).map_err(|m| {
            if conference.status.is_terminal_or_closing() {
                state_conflict(m)
            } else {
                ApiError::domain(ErrorCode::ConferenceNotLive, m)
            }
        })?;
        let Some(next) = next else {
            txn.commit().await?;
            if conference.status == ConferenceStatus::Ending && self.rtc.room_control() {
                // Repeated END retries provider termination now (idempotent), even while
                // the queued command is in retry backoff.
                if let Err(e) = self.rtc.finalize_room(id).await {
                    tracing::warn!(conference_id = %id, "termination retry failed: {e}");
                }
                let (current, _) = self.enforce(id).await?;
                let enforcement = if current.status == ConferenceStatus::Ended {
                    Enforcement::Enforced
                } else {
                    Enforcement::Pending
                };
                return Ok(ConferenceOutcome {
                    changed: false,
                    enforcement,
                    conference: current,
                });
            }
            return Ok(ConferenceOutcome::unchanged(conference));
        };
        let mut am: conference_sessions::ActiveModel = conference.clone().into();
        am.status = Set(next);
        am.ending_requested_at = Set(Some(Utc::now()));
        am.ended_by = Set(Some(authority.audit.reference()));
        am.termination_reason = Set(Some(reason));
        let updated = self.apply(&txn, conference, am).await?;
        self.record(
            &txn,
            &authority.audit,
            &updated,
            action::CONFERENCE_END_REQUEST,
            &[
                event::CONFERENCE_ENDING,
                event::RTC_ROOM_TERMINATE_REQUESTED,
            ],
            json!({ "terminationReason": reason }),
        )
        .await?;
        txn.commit().await?;
        if !self.rtc.room_control() {
            // Credential-only provider: ENDING is never faked into ENDED.
            return Ok(ConferenceOutcome::changed(updated, Enforcement::Pending));
        }
        // terminate room → confirm absence → finalize sessions → ENDED. On failure the
        // conference stays ENDING (joins/credentials refused) and the relay retries.
        let (current, _) = self.enforce(id).await?;
        let enforcement = if current.status == ConferenceStatus::Ended {
            Enforcement::Enforced
        } else {
            Enforcement::Pending
        };
        Ok(ConferenceOutcome::changed(current, enforcement))
    }

    /// CREATED|PREPARING|WAITING → CANCELLED. For a recurring occurrence this is a
    /// single-occurrence exception; the series is untouched.
    pub async fn cancel(&self, ctx: &ActorContext, id: Uuid) -> ApiResult<ConferenceOutcome> {
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, id).await?;
        Self::legacy_guard(&conference)?;
        let space = find_space(&txn, conference.meeting_space_id).await?;
        let authority = self
            .authority_for(&txn, &space, id, ctx, Permission::ConferenceStart)
            .await?;
        let next = next_status(conference.status, Transition::Cancel).map_err(state_conflict)?;
        let Some(next) = next else {
            txn.commit().await?;
            return Ok(ConferenceOutcome::unchanged(conference));
        };
        let mut am: conference_sessions::ActiveModel = conference.clone().into();
        am.status = Set(next);
        am.cancelled_at = Set(Some(Utc::now()));
        am.cancelled_by = Set(Some(authority.audit.reference()));
        am.termination_reason = Set(Some(TerminationReason::Cancelled));
        if conference.recurrence_rule_id.is_some() {
            am.is_exception = Set(true);
            crate::services::recurrence::record_exception_for_conference(
                &txn,
                &conference,
                crate::domain::RecurrenceExceptionKind::Cancelled,
                None,
                None,
                authority.audit.reference(),
            )
            .await?;
        }
        let had_room = conference.rtc_room_ready_at.is_some();
        let updated = self.apply(&txn, conference, am).await?;
        self.record(
            &txn,
            &authority.audit,
            &updated,
            action::CONFERENCE_CANCEL,
            &[event::CONFERENCE_CANCELLED],
            json!({}),
        )
        .await?;
        if had_room {
            emit(&txn, Self::terminate_command(&updated)).await?;
        }
        txn.commit().await?;
        if had_room && self.rtc.room_control() {
            let (current, enforcement) = self.enforce(id).await?;
            return Ok(ConferenceOutcome::changed(current, enforcement));
        }
        Ok(ConferenceOutcome::changed(
            updated,
            Enforcement::NotRequired,
        ))
    }

    /// One-off reschedule of an unstarted occurrence (series untouched).
    pub async fn reschedule(
        &self,
        ctx: &ActorContext,
        id: Uuid,
        req: RescheduleConferenceRequest,
    ) -> ApiResult<ConferenceOutcome> {
        if let Some(end) = req.scheduled_end_at {
            if end <= req.scheduled_start_at {
                return Err(validation("scheduledEndAt must be after scheduledStartAt"));
            }
        }
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, id).await?;
        Self::legacy_guard(&conference)?;
        let space = find_space(&txn, conference.meeting_space_id).await?;
        let authority = self
            .authority_for(&txn, &space, id, ctx, Permission::ConferenceStart)
            .await?;
        if conference.status != ConferenceStatus::Created {
            return Err(state_conflict(
                "Only an unstarted conference can be rescheduled",
            ));
        }
        let mut am: conference_sessions::ActiveModel = conference.clone().into();
        am.scheduled_start_at = Set(Some(req.scheduled_start_at));
        am.scheduled_end_at = Set(req.scheduled_end_at);
        if conference.recurrence_rule_id.is_some() {
            am.is_exception = Set(true);
            crate::services::recurrence::record_exception_for_conference(
                &txn,
                &conference,
                crate::domain::RecurrenceExceptionKind::Rescheduled,
                Some(req.scheduled_start_at),
                req.scheduled_end_at,
                authority.audit.reference(),
            )
            .await?;
        }
        let updated = self.apply(&txn, conference.clone(), am).await?;
        self.record(
            &txn,
            &authority.audit,
            &updated,
            action::CONFERENCE_RESCHEDULE,
            &[event::CONFERENCE_RESCHEDULED],
            json!({ "from": conference.scheduled_start_at, "to": req.scheduled_start_at }),
        )
        .await?;
        txn.commit().await?;
        Ok(ConferenceOutcome::changed(
            updated,
            Enforcement::NotRequired,
        ))
    }

    /// PREPARING|LIVE|ENDING → FAILED with a safe internal classification. Recovery is
    /// decided later by the reconciler (Part E).
    pub async fn fail(&self, id: Uuid, failure_reason: &str) -> ApiResult<ConferenceOutcome> {
        let actor = AuditActor::system("conference-service");
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, id).await?;
        Self::legacy_guard(&conference)?;
        let next = next_status(conference.status, Transition::Fail).map_err(state_conflict)?;
        let Some(next) = next else {
            txn.commit().await?;
            return Ok(ConferenceOutcome::unchanged(conference));
        };
        let reason: String = failure_reason
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(64)
            .collect();
        let mut am: conference_sessions::ActiveModel = conference.clone().into();
        am.status = Set(next);
        am.failure_reason = Set(Some(if reason.is_empty() {
            "UNSPECIFIED".into()
        } else {
            reason.clone()
        }));
        am.termination_reason = Set(Some(TerminationReason::Failure));
        let updated = self.apply(&txn, conference, am).await?;
        self.record(
            &txn,
            &actor,
            &updated,
            action::CONFERENCE_FAIL,
            &[event::CONFERENCE_FAILED],
            json!({ "failureReason": reason }),
        )
        .await?;
        if updated.rtc_room_ready_at.is_some() {
            emit(&txn, Self::terminate_command(&updated)).await?;
        }
        txn.commit().await?;
        let (current, enforcement) = if self.rtc.room_control() {
            self.enforce(id).await?
        } else {
            (updated, Enforcement::Pending)
        };
        Ok(ConferenceOutcome::changed(current, enforcement))
    }
}

/// Start-window policy: early start allowed `earlyStartMinutes` before the scheduled
/// start; starting more than `lateStartGraceMinutes` after the scheduled end is refused.
pub fn check_start_window(
    conference: &conference_sessions::Model,
    policy: &crate::dto::v2::SchedulePolicy,
    now: DateTime<Utc>,
) -> ApiResult<()> {
    if let Some(start) = conference.scheduled_start_at {
        if now < start - Duration::minutes(policy.early_start_minutes as i64) {
            return Err(ApiError::domain(
                ErrorCode::ConferenceStateConflict,
                "This conference cannot be started yet",
            ));
        }
    }
    if let Some(end) = conference.scheduled_end_at {
        if now > end + Duration::minutes(policy.late_start_grace_minutes as i64) {
            return Err(ApiError::domain(
                ErrorCode::ConferenceStateConflict,
                "The start window for this conference has closed",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ConferenceStatus::*;

    #[test]
    fn state_machine_transitions() {
        assert_eq!(
            next_status(Created, Transition::Prepare),
            Ok(Some(Preparing))
        );
        assert_eq!(
            next_status(Preparing, Transition::MarkReady),
            Ok(Some(Waiting))
        );
        assert_eq!(next_status(Preparing, Transition::Start), Ok(Some(Live)));
        assert_eq!(next_status(Waiting, Transition::Start), Ok(Some(Live)));
        assert_eq!(next_status(Created, Transition::Start), Ok(Some(Live)));
        assert_eq!(next_status(Live, Transition::Start), Ok(None));
        assert_eq!(next_status(Live, Transition::End), Ok(Some(Ending)));
        assert_eq!(next_status(Ending, Transition::End), Ok(None));
        assert_eq!(next_status(Ended, Transition::End), Ok(None));
        assert_eq!(
            next_status(Ending, Transition::CompleteEnd),
            Ok(Some(Ended))
        );
        assert_eq!(
            next_status(Waiting, Transition::Cancel),
            Ok(Some(Cancelled))
        );
        assert_eq!(next_status(Live, Transition::Fail), Ok(Some(Failed)));
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        assert!(next_status(Ended, Transition::Start).is_err());
        assert!(next_status(Cancelled, Transition::Start).is_err());
        assert!(next_status(Ending, Transition::Start).is_err());
        assert!(next_status(Live, Transition::Cancel).is_err());
        assert!(next_status(Created, Transition::End).is_err());
        assert!(next_status(Created, Transition::Fail).is_err());
        assert!(
            next_status(Live, Transition::CompleteEnd).is_err(),
            "ENDED only after ENDING"
        );
    }

    fn conf(
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> conference_sessions::Model {
        let now = Utc::now();
        conference_sessions::Model {
            id: Uuid::new_v4(),
            meeting_space_id: Uuid::new_v4(),
            occurrence_key: "k".into(),
            kind: ConferenceKind::Scheduled,
            origin: RecordOrigin::V2,
            legacy_meeting_id: None,
            recurrence_rule_id: None,
            is_exception: false,
            original_start_at: None,
            status: Created,
            scheduled_start_at: start,
            scheduled_end_at: end,
            started_at: None,
            started_by: None,
            ending_requested_at: None,
            ended_at: None,
            ended_by: None,
            cancelled_at: None,
            cancelled_by: None,
            termination_reason: None,
            failure_reason: None,
            is_locked: false,
            locked_at: None,
            locked_by: None,
            rtc_provider: "LIVEKIT".into(),
            rtc_room_name: None,
            rtc_room_id: None,
            region: None,
            recording_state: "OFF".into(),
            transcription_state: "OFF".into(),
            created_at: now,
            updated_at: now,
            version: 1,
            event_seq: 0,
            rtc_room_ready_at: None,
            rtc_room_terminated_at: None,
            collaboration_policy: serde_json::json!({}),
            collaboration_version: 1,
        }
    }

    #[test]
    fn start_window_policy() {
        let p = crate::dto::v2::SchedulePolicy::default();
        let now = Utc::now();
        assert!(check_start_window(&conf(None, None), &p, now).is_ok());
        assert!(
            check_start_window(&conf(Some(now + Duration::minutes(10)), None), &p, now).is_ok()
        );
        assert!(
            check_start_window(&conf(Some(now + Duration::minutes(30)), None), &p, now).is_err()
        );
        let late = conf(
            Some(now - Duration::hours(3)),
            Some(now - Duration::hours(2)),
        );
        assert!(check_start_window(&late, &p, now).is_err());
    }
}
