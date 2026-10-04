//! RtcControl — the Part E RTC control plane (spec Part E §1–§9).
//!
//! PostgreSQL stays the authority; the provider only enforces committed decisions:
//!
//! ```text
//! PREPARE: CREATED → PREPARING ─(provider room confirmed)→ WAITING
//! END:     LIVE → ENDING ─(joins & credentials refused)→ terminate room
//!          → confirm absence → finalize active sessions → ENDED
//! ```
//!
//! Provider work never happens inside a business transaction. Business transactions
//! write `rtc.*` provider commands to the outbox; after commit the request path
//! dispatches that conference's commands inline (immediate enforcement) and the outbox
//! relay / reconciler retry anything that failed. Every command is idempotent and
//! re-reads authoritative state before acting, so a stale or replayed command can never
//! grant more than PostgreSQL currently allows.

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait,
    QueryFilter, Set, Statement, TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, ConferenceStatus, OutboxChannel, RecordOrigin, SessionState},
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_sessions, outbox_events, participant_sessions},
    services::{
        actor::ActorContext,
        conference::conference_event,
        meeting_core::{
            conference_authority, find_conference, find_scoped_participant, find_space,
            lock_conference, not_found_conference, permission_denied, state_conflict, Core,
        },
        meeting_events::{action, audit, emit, event, AuditActor, AuditRecord},
        meeting_permissions::{can_moderate, Permission, PermissionContext},
        outbox,
        rtc_metrics::{add, inc, RTC_METRICS as M},
        rtc_provider::{RoomSpec, RtcError, RtcParticipantRef, RtcRoomRef, TrackKind},
    },
};

/// Outcome of provider enforcement for a committed business decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// The provider confirmed every queued command.
    Enforced,
    /// Committed; provider enforcement queued (retried by relay / reconciler).
    Pending,
    /// No provider action was needed.
    NotRequired,
}

/// Result of executing one provider command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    Done,
    /// Transient (provider outage): retried with backoff, never dead-lettered.
    Retry(String),
    /// Poison: malformed / unknown / scoped to nothing — dead-lettered.
    Dead(String),
}

#[derive(Debug, Clone)]
pub struct MuteOutcome {
    pub sessions: u32,
    pub muted_tracks: u32,
}

pub struct RtcControl {
    core: Core,
}

fn rtc_unavailable(message: &str) -> ApiError {
    ApiError::domain(ErrorCode::RtcProviderUnavailable, message.to_string())
}

/// Finalize every active session of a conference (CONFERENCE_ENDED). Returns count.
async fn finalize_sessions<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    reason: &str,
) -> ApiResult<u64> {
    let r = conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE participant_sessions
                SET state = 'TERMINATED', disconnected_at = COALESCE(disconnected_at, now()),
                    end_reason = COALESCE(end_reason, $2), updated_at = now()
              WHERE conference_id = $1 AND state IN ('PENDING','CONNECTED')",
            [conference_id.into(), reason.into()],
        ))
        .await?;
    Ok(r.rows_affected())
}

/// Record provider-confirmed room absence on a LOCKED conference row: ENDING → ENDED
/// (after finalizing sessions); CANCELLED/FAILED only record the timestamp.
pub async fn record_room_terminated(
    txn: &DatabaseTransaction,
    current: conference_sessions::Model,
    actor: &AuditActor,
    confirmed_by: &str,
) -> ApiResult<conference_sessions::Model> {
    if current.status == ConferenceStatus::Ended
        || (current.status != ConferenceStatus::Ending && current.rtc_room_terminated_at.is_some())
    {
        return Ok(current);
    }
    if !matches!(
        current.status,
        ConferenceStatus::Ending | ConferenceStatus::Cancelled | ConferenceStatus::Failed
    ) {
        return Err(state_conflict(format!(
            "A {} conference cannot be finalized",
            current.status
        )));
    }
    let now = Utc::now();
    let ending = current.status == ConferenceStatus::Ending;
    let finalized = if ending {
        finalize_sessions(txn, current.id, "CONFERENCE_ENDED").await?
    } else {
        0
    };
    // Part F: the parent conference owns its breakouts — closing it closes them all.
    crate::services::collaboration::breakouts::close_all_in_txn(txn, &current, actor).await?;
    let mut am: conference_sessions::ActiveModel = current.clone().into();
    am.rtc_room_terminated_at = Set(Some(now));
    if ending {
        am.status = Set(ConferenceStatus::Ended);
        am.ended_at = Set(Some(now));
    }
    am.version = Set(current.version + 1);
    am.updated_at = Set(now);
    let updated = am.update(txn).await?;
    if ending {
        audit(
            txn,
            actor,
            AuditRecord {
                meeting_space_id: updated.meeting_space_id,
                conference_id: Some(updated.id),
                action: action::CONFERENCE_END_COMPLETE,
                target_type: Some("CONFERENCE"),
                target_ref: Some(updated.id.to_string()),
                metadata: json!({
                    "terminationReason": updated.termination_reason,
                    "finalizedSessions": finalized,
                    "confirmedBy": confirmed_by,
                }),
            },
        )
        .await?;
        emit(txn, conference_event(&updated, event::CONFERENCE_ENDED)).await?;
    }
    inc(&M.rooms_terminated);
    Ok(updated)
}

impl RtcControl {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    /// Room lifecycle is provider-enforced (flag on AND provider capable).
    pub fn room_control(&self) -> bool {
        self.core.flags.rtc_provider_control_v2 && self.core.rtc.capabilities().room_control
    }

    /// Participant removal / permissions / mute are provider-enforced.
    pub fn participant_control(&self) -> bool {
        self.core.flags.rtc_provider_control_v2 && self.core.rtc.capabilities().participant_control
    }

    /// Bounded credential lifetime (never longer than the participant session).
    pub fn credential_ttl(&self) -> std::time::Duration {
        let secs = self
            .core
            .config
            .rtc_credential_ttl_secs
            .min(self.core.config.session_ttl_secs)
            .clamp(60, 6 * 60 * 60);
        std::time::Duration::from_secs(secs as u64)
    }

    /// Create/ensure the provider room and record the confirmation. PREPARING moves to
    /// WAITING only here — i.e. only after the provider confirmed the room. Idempotent
    /// and safe under concurrent prepare (LiveKit CreateRoom returns the existing room;
    /// the confirmation is applied under the conference row lock).
    pub async fn ensure_room(&self, conference_id: Uuid) -> ApiResult<conference_sessions::Model> {
        self.ensure_room_inner(conference_id, false).await
    }

    /// Join-time check: the provider must currently have the room (a credential for a
    /// missing room is useless; an unreachable provider is RTC_PROVIDER_UNAVAILABLE).
    /// A room lost at the provider is re-created for a LIVE conference.
    pub async fn verify_room_for_join(&self, conference_id: Uuid) -> ApiResult<()> {
        let c = find_conference(&self.core.db, conference_id).await?;
        let room = RtcRoomRef::for_conference(&c);
        match self.core.rtc.get_room(&room).await {
            Ok(Some(_)) if c.rtc_room_ready_at.is_some() => Ok(()),
            Ok(_) => self
                .ensure_room_inner(conference_id, true)
                .await
                .map(|_| ()),
            Err(e) => {
                inc(&M.provider_errors);
                Err(e.into())
            }
        }
    }

    async fn ensure_room_inner(
        &self,
        conference_id: Uuid,
        force: bool,
    ) -> ApiResult<conference_sessions::Model> {
        let c = find_conference(&self.core.db, conference_id).await?;
        if c.origin == RecordOrigin::Legacy {
            return Err(state_conflict(
                "This conference is managed by the legacy meeting API until migration certification",
            ));
        }
        if !matches!(
            c.status,
            ConferenceStatus::Preparing | ConferenceStatus::Waiting | ConferenceStatus::Live
        ) {
            return Err(state_conflict(format!(
                "Media cannot be prepared for a conference that is {}",
                c.status
            )));
        }
        if !force && c.rtc_room_ready_at.is_some() && c.status != ConferenceStatus::Preparing {
            return Ok(c);
        }
        let room = RtcRoomRef::for_conference(&c);
        let info = match self.core.rtc.create_room(&room, &RoomSpec::default()).await {
            Ok(info) => info,
            Err(e) => {
                inc(&M.room_create_failures);
                inc(&M.provider_errors);
                tracing::error!(conference_id = %conference_id, "provider room creation failed: {e}");
                return Err(e.into());
            }
        };
        inc(&M.rooms_confirmed);
        let txn = self.core.db.begin().await?;
        let current = lock_conference(&txn, conference_id).await?;
        if !matches!(
            current.status,
            ConferenceStatus::Preparing | ConferenceStatus::Waiting | ConferenceStatus::Live
        ) {
            // Closed while the room was being created: the queued terminate command and
            // the reconciler remove the room; readiness is never recorded.
            txn.commit().await?;
            return Ok(current);
        }
        let now = Utc::now();
        let became_ready = current.status == ConferenceStatus::Preparing;
        let set_id = info.room_id.is_some() && current.rtc_room_id != info.room_id;
        if !became_ready && !set_id && current.rtc_room_ready_at.is_some() {
            txn.commit().await?;
            return Ok(current);
        }
        let mut am: conference_sessions::ActiveModel = current.clone().into();
        if set_id {
            am.rtc_room_id = Set(info.room_id.clone());
        }
        if current.rtc_room_ready_at.is_none() {
            am.rtc_room_ready_at = Set(Some(now));
        }
        if became_ready {
            am.status = Set(ConferenceStatus::Waiting);
        }
        am.version = Set(current.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        if became_ready {
            audit(
                &txn,
                &AuditActor::system("rtc-control"),
                AuditRecord {
                    meeting_space_id: updated.meeting_space_id,
                    conference_id: Some(updated.id),
                    action: action::CONFERENCE_READY,
                    target_type: Some("CONFERENCE"),
                    target_ref: Some(updated.id.to_string()),
                    metadata: json!({ "provider": self.core.rtc.name() }),
                },
            )
            .await?;
            emit(&txn, conference_event(&updated, event::CONFERENCE_READY)).await?;
        }
        txn.commit().await?;
        Ok(updated)
    }

    /// Terminate the provider room of a closing conference and record the confirmed
    /// absence. On any provider failure the conference stays ENDING (joins and
    /// credentials remain refused) and the command is retried.
    pub async fn finalize_room(
        &self,
        conference_id: Uuid,
    ) -> ApiResult<conference_sessions::Model> {
        let c = find_conference(&self.core.db, conference_id).await?;
        match c.status {
            ConferenceStatus::Ended => return Ok(c),
            ConferenceStatus::Cancelled | ConferenceStatus::Failed
                if c.rtc_room_terminated_at.is_some() =>
            {
                return Ok(c)
            }
            ConferenceStatus::Ending | ConferenceStatus::Cancelled | ConferenceStatus::Failed => {}
            other => {
                return Err(state_conflict(format!(
                    "A {other} conference has no room to terminate"
                )))
            }
        }
        let room = RtcRoomRef::for_conference(&c);
        let reason = c.termination_reason.map(|r| r.as_str()).unwrap_or("CLOSED");
        if let Err(e) = self.core.rtc.terminate_room(&room, reason).await {
            inc(&M.room_terminate_failures);
            inc(&M.provider_errors);
            tracing::error!(conference_id = %conference_id, alert = "RTC_TERMINATION_FAILED", "provider room termination failed: {e}");
            return Err(e.into());
        }
        // ENDED requires reconciled absence, not just an acknowledged delete.
        match self.core.rtc.get_room(&room).await {
            Ok(None) => {}
            Ok(Some(_)) => {
                inc(&M.room_terminate_failures);
                tracing::error!(conference_id = %conference_id, alert = "RTC_TERMINATION_FAILED", "provider room still present after termination");
                return Err(rtc_unavailable("The media room is still terminating"));
            }
            Err(e) => {
                inc(&M.room_terminate_failures);
                return Err(e.into());
            }
        }
        let txn = self.core.db.begin().await?;
        let current = lock_conference(&txn, conference_id).await?;
        let updated = record_room_terminated(
            &txn,
            current,
            &AuditActor::system("rtc-control"),
            "PROVIDER_API",
        )
        .await?;
        txn.commit().await?;
        Ok(updated)
    }

    /// Execute one `PROVIDER_COMMAND` outbox row. Re-reads authoritative state first.
    pub async fn execute(&self, row: &outbox_events::Model) -> CommandResult {
        if !event::is_provider_command(&row.event_type) {
            return CommandResult::Dead("not a provider command".into());
        }
        let p = &row.payload;
        let Some(conference_id) = row.conference_id.or_else(|| {
            p["conferenceId"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
        }) else {
            return CommandResult::Dead("missing conferenceId".into());
        };
        let conference = match find_conference(&self.core.db, conference_id).await {
            Ok(c) => c,
            Err(ApiError::Domain {
                code: ErrorCode::ConferenceNotFound,
                ..
            }) => return CommandResult::Dead("unknown conference".into()),
            Err(e) => return CommandResult::Retry(e.to_string()),
        };
        let retry = |e: ApiError| CommandResult::Retry(e.to_string());
        match row.event_type.as_str() {
            event::RTC_ROOM_PREPARE_REQUESTED => match conference.status {
                ConferenceStatus::Preparing
                | ConferenceStatus::Waiting
                | ConferenceStatus::Live => match self.ensure_room(conference_id).await {
                    Ok(_) => CommandResult::Done,
                    Err(e) => retry(e),
                },
                // Obsolete: rooms are never created for unstarted or closed conferences.
                _ => CommandResult::Done,
            },
            event::RTC_ROOM_TERMINATE_REQUESTED => match conference.status {
                ConferenceStatus::Ended => CommandResult::Done,
                ConferenceStatus::Ending
                | ConferenceStatus::Cancelled
                | ConferenceStatus::Failed => match self.finalize_room(conference_id).await {
                    Ok(_) => CommandResult::Done,
                    Err(e) => retry(e),
                },
                other => {
                    CommandResult::Dead(format!("terminate requested for a {other} conference"))
                }
            },
            event::RTC_PARTICIPANT_REMOVE_REQUESTED => {
                // The identity comes from the authoritative session row of THIS conference;
                // a payload can never point the provider at another conference's session.
                let session_id = p["participantSessionId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok());
                let (session_id, identity) = match session_id {
                    Some(sid) => match participant_sessions::Entity::find_by_id(sid)
                        .filter(participant_sessions::Column::ConferenceId.eq(conference_id))
                        .one(&self.core.db)
                        .await
                    {
                        Ok(Some(s)) => (s.id, s.media_identity),
                        Ok(None) => {
                            return CommandResult::Dead(
                                "session is not part of this conference".into(),
                            )
                        }
                        Err(e) => return CommandResult::Retry(e.to_string()),
                    },
                    None => match p["mediaIdentity"].as_str() {
                        Some(m) if !m.is_empty() && m.len() <= 128 => (Uuid::nil(), m.to_string()),
                        _ => return CommandResult::Dead("missing provider identity".into()),
                    },
                };
                if conference.status == ConferenceStatus::Ended
                    || conference.rtc_room_terminated_at.is_some()
                {
                    return CommandResult::Done;
                }
                // Part F: a removal disconnects the main room AND every active breakout
                // room; a context eviction (`roomName`) targets only that room.
                let rooms = match p["roomName"].as_str() {
                    Some(r) => {
                        match crate::services::collaboration::breakouts::conference_rooms(
                            &self.core.db,
                            &conference,
                        )
                        .await
                        {
                            Ok(all) if all.iter().any(|x| x == r) => vec![r.to_string()],
                            Ok(_) => {
                                return CommandResult::Dead(
                                    "room is not part of this conference".into(),
                                )
                            }
                            Err(e) => return CommandResult::Retry(e.to_string()),
                        }
                    }
                    None => match crate::services::collaboration::breakouts::conference_rooms(
                        &self.core.db,
                        &conference,
                    )
                    .await
                    {
                        Ok(all) => all,
                        Err(e) => return CommandResult::Retry(e.to_string()),
                    },
                };
                let reason = p["reason"].as_str().unwrap_or("REMOVED");
                for (i, room_name) in rooms.into_iter().enumerate() {
                    let target = RtcParticipantRef {
                        conference_id,
                        room_name,
                        participant_session_id: session_id,
                        media_identity: identity.clone(),
                    };
                    match self.core.rtc.remove_participant(&target, reason).await {
                        Ok(()) => {
                            if i == 0 {
                                inc(&M.participants_removed);
                            }
                        }
                        Err(RtcError::NotFound) if i > 0 => {}
                        Err(e) => {
                            inc(&M.provider_errors);
                            return CommandResult::Retry(e.to_string());
                        }
                    }
                }
                CommandResult::Done
            }
            event::RTC_PERMISSIONS_UPDATE_REQUESTED => {
                let Some(pid) = p["participantId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                else {
                    return CommandResult::Dead("missing participantId".into());
                };
                match self.sync_permissions(&conference, pid).await {
                    Ok(_) => CommandResult::Done,
                    Err(ApiError::Domain {
                        code: ErrorCode::ParticipantNotFound,
                        ..
                    }) => CommandResult::Dead("participant is not part of this conference".into()),
                    Err(e) => retry(e),
                }
            }
            event::RTC_BREAKOUT_PREPARE_REQUESTED
            | event::RTC_BREAKOUT_TERMINATE_REQUESTED
            | event::RTC_PARTICIPANT_REROUTE_REQUESTED => {
                crate::services::collaboration::breakouts::execute_command(
                    &self.core,
                    &conference,
                    row,
                )
                .await
            }
            event::RTC_SCREENSHARE_STOP_REQUESTED => {
                let Some(pid) = p["participantId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                else {
                    return CommandResult::Dead("missing participantId".into());
                };
                if conference.status != ConferenceStatus::Live {
                    return CommandResult::Done;
                }
                match self.enforce_screen_share(&conference, pid).await {
                    Ok(()) => CommandResult::Done,
                    Err(ApiError::Domain {
                        code: ErrorCode::ParticipantNotFound,
                        ..
                    }) => CommandResult::Dead("participant is not part of this conference".into()),
                    Err(e) => retry(e),
                }
            }
            other => CommandResult::Dead(format!("unknown provider command {other}")),
        }
    }

    /// Re-assert grants and stop any screen share the participant may no longer publish.
    async fn enforce_screen_share(
        &self,
        conference: &conference_sessions::Model,
        participant_id: Uuid,
    ) -> ApiResult<()> {
        let participant =
            find_scoped_participant(&self.core.db, conference.id, participant_id, false).await?;
        let permissions = crate::services::collaboration::media::effective_media_for(
            &self.core.db,
            conference,
            &participant,
        )
        .await?;
        self.sync_permissions(conference, participant_id).await?;
        if permissions.screen_share_publish {
            return Ok(());
        }
        let room = crate::services::collaboration::breakouts::media_context(
            &self.core.db,
            conference,
            &participant,
        )
        .await?
        .room_name(conference);
        let sessions = participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceParticipantId.eq(participant_id))
            .filter(participant_sessions::Column::State.eq(SessionState::Connected))
            .all(&self.core.db)
            .await?;
        for s in sessions {
            let target = RtcParticipantRef {
                conference_id: conference.id,
                room_name: room.clone(),
                participant_session_id: s.id,
                media_identity: s.media_identity.clone(),
            };
            match self
                .core
                .rtc
                .mute_track(&target, TrackKind::ScreenShare)
                .await
            {
                Ok(_) | Err(RtcError::NotFound) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Push the CURRENT authoritative grants (from the permission engine) to every live
    /// provider session of a participant. Never uses grants carried in a payload.
    pub async fn sync_permissions(
        &self,
        conference: &conference_sessions::Model,
        participant_id: Uuid,
    ) -> ApiResult<u32> {
        let participant =
            find_scoped_participant(&self.core.db, conference.id, participant_id, false).await?;
        if participant.admission_state != AdmissionState::Admitted
            || conference.status != ConferenceStatus::Live
        {
            return Ok(0);
        }
        // Part F: the single effective projection (role ∧ policy ∧ locks), pushed to the
        // participant's current media context room (main or breakout).
        let permissions = crate::services::collaboration::media::effective_media_for(
            &self.core.db,
            conference,
            &participant,
        )
        .await?;
        let room = crate::services::collaboration::breakouts::media_context(
            &self.core.db,
            conference,
            &participant,
        )
        .await?
        .room_name(conference);
        let sessions = participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceParticipantId.eq(participant.id))
            .filter(
                participant_sessions::Column::State
                    .is_in([SessionState::Pending, SessionState::Connected]),
            )
            .all(&self.core.db)
            .await?;
        let mut updated = 0;
        for s in sessions {
            let target = RtcParticipantRef {
                room_name: room.clone(),
                ..RtcParticipantRef::new(conference, &s)
            };
            match self
                .core
                .rtc
                .update_permissions(&target, &permissions)
                .await
            {
                Ok(()) => {
                    updated += 1;
                    inc(&M.permission_updates);
                }
                // Not connected yet: the credential already carries current grants or the
                // provider-join webhook re-syncs on connect.
                Err(RtcError::NotFound) => {}
                Err(e) => {
                    inc(&M.provider_errors);
                    return Err(e.into());
                }
            }
        }
        // Part F: a confirmed grant withdrawal unpublishes the share at the provider;
        // record it so the authoritative view never shows a share that cannot exist.
        if !permissions.screen_share_publish {
            let txn = self.core.db.begin().await?;
            let locked = lock_conference(&txn, conference.id).await?;
            crate::services::collaboration::media::end_screen_share(
                &txn,
                &locked,
                &participant,
                None,
                "PERMISSION_REVOKED",
            )
            .await?;
            txn.commit().await?;
        }
        Ok(updated)
    }

    /// Claim and execute this conference's due provider commands right after a commit.
    pub async fn dispatch_for_conference(&self, conference_id: Uuid) -> Enforcement {
        if !self.room_control() {
            return Enforcement::Pending;
        }
        let holder = outbox::holder_id("dispatch");
        let rows = match outbox::claim(
            &self.core.db,
            OutboxChannel::ProviderCommand,
            Some(conference_id),
            100,
            30,
            &holder,
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(conference_id = %conference_id, "provider command claim failed: {e}");
                return Enforcement::Pending;
            }
        };
        for row in &rows {
            self.process(row, &holder).await;
        }
        match self.pending_commands(conference_id).await {
            Ok(0) => Enforcement::Enforced,
            _ => Enforcement::Pending,
        }
    }

    /// Execute + record one claimed command row.
    pub async fn process(&self, row: &outbox_events::Model, holder: &str) -> CommandResult {
        let result = self.execute(row).await;
        let recorded = match &result {
            CommandResult::Done => {
                inc(&M.outbox_published);
                outbox::mark_published(&self.core.db, row.id, holder).await
            }
            CommandResult::Retry(e) => {
                inc(&M.outbox_retried);
                outbox::mark_retry(&self.core.db, row, holder, e, true)
                    .await
                    .map(|_| ())
            }
            CommandResult::Dead(e) => {
                inc(&M.outbox_dead_lettered);
                tracing::error!(event_id = %row.id, event_type = %row.event_type, "provider command dead-lettered: {e}");
                outbox::mark_failed(&self.core.db, row.id, holder, e).await
            }
        };
        if let Err(e) = recorded {
            tracing::error!(event_id = %row.id, "failed to record provider command result: {e}");
        }
        result
    }

    pub async fn pending_commands(&self, conference_id: Uuid) -> ApiResult<i64> {
        let row = self
            .core
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT count(*) FROM outbox_events WHERE conference_id = $1 AND channel = 'PROVIDER_COMMAND' AND status = 'PENDING'",
                [conference_id.into()],
            ))
            .await?;
        Ok(row
            .and_then(|r| r.try_get_by_index::<i64>(0).ok())
            .unwrap_or(0))
    }

    /// Server-side mute primitive (host action). Authorization and conference scoping
    /// are resolved BEFORE any provider call: a target outside this conference is
    /// indistinguishable from an unknown participant.
    pub async fn mute(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
        kind: TrackKind,
    ) -> ApiResult<MuteOutcome> {
        let conference = find_conference(&self.core.db, conference_id).await?;
        if conference.origin == RecordOrigin::Legacy {
            return Err(not_found_conference());
        }
        let space = find_space(&self.core.db, conference.meeting_space_id).await?;
        let authority = conference_authority(&self.core.db, &space, conference_id, ctx)
            .await?
            .ok_or_else(|| permission_denied("You are not permitted to manage this conference"))?;
        let pctx = PermissionContext::for_space(&space);
        if !pctx.allows(authority.role, Permission::ParticipantMute) {
            return Err(permission_denied(
                "Your role does not allow muting participants",
            ));
        }
        let target =
            find_scoped_participant(&self.core.db, conference_id, participant_id, false).await?;
        let is_self = authority
            .participant
            .as_ref()
            .map(|p| p.id == target.id)
            .unwrap_or(false);
        if !is_self && !can_moderate(authority.role, target.role) {
            return Err(permission_denied(
                "You can only moderate less privileged participants",
            ));
        }
        if conference.status != ConferenceStatus::Live {
            return Err(ApiError::domain(
                ErrorCode::ConferenceNotLive,
                "Only a live conference has media to mute",
            ));
        }
        if !self.participant_control() {
            return Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Provider media control is not enabled",
            ));
        }
        let sessions = participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceParticipantId.eq(target.id))
            .filter(participant_sessions::Column::ConferenceId.eq(conference_id))
            .filter(participant_sessions::Column::State.eq(SessionState::Connected))
            .all(&self.core.db)
            .await?;
        let room = crate::services::collaboration::breakouts::media_context(
            &self.core.db,
            &conference,
            &target,
        )
        .await?
        .room_name(&conference);
        let mut muted = 0;
        for s in &sessions {
            let target_ref = RtcParticipantRef {
                room_name: room.clone(),
                ..RtcParticipantRef::new(&conference, s)
            };
            match self.core.rtc.mute_track(&target_ref, kind).await {
                Ok(n) => muted += n,
                Err(RtcError::NotFound) => {}
                Err(e) => {
                    inc(&M.provider_errors);
                    return Err(e.into());
                }
            }
        }
        add(&M.tracks_muted, muted as u64);
        audit(
            &self.core.db,
            &authority.audit,
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: Some(conference_id),
                action: action::MEDIA_MUTE,
                target_type: Some("PARTICIPANT"),
                target_ref: Some(target.id.to_string()),
                metadata: json!({ "trackKind": kind.as_str(), "mutedTracks": muted, "sessions": sessions.len() }),
            },
        )
        .await?;
        // Part F: MUTE NOW is a one-off provider action (not a lock); announce it.
        let txn = self.core.db.begin().await?;
        emit(
            &txn,
            crate::services::meeting_events::OutboxRecord {
                aggregate_type: "conference_participant",
                aggregate_id: target.id,
                event_type: event::PARTICIPANT_MUTED,
                meeting_space_id: Some(space.id),
                conference_id: Some(conference_id),
                payload: json!({ "participantId": target.id, "trackKind": kind.as_str(), "mutedTracks": muted }),
            },
        )
        .await?;
        txn.commit().await?;
        crate::services::collaboration::metrics::inc(
            &crate::services::collaboration::metrics::COLLAB_METRICS.media_mutes,
        );
        Ok(MuteOutcome {
            sessions: sessions.len() as u32,
            muted_tracks: muted,
        })
    }
}
