//! ConferenceScheduler — idempotent, multi-instance-safe v2 scheduling worker
//! (spec §C16, §C18, §3.6). Replaces the legacy auto-start/auto-end workers for v2
//! conferences (the legacy workers keep serving legacy `meetings` rows).
//!
//! Each tick:
//! 1. materializes near-future recurring occurrences (rules claimed with
//!    `FOR UPDATE SKIP LOCKED`, inserts `ON CONFLICT DO NOTHING`);
//! 2. requests the end (LIVE → ENDING, reason SCHEDULED_END) of conferences past their
//!    scheduled end + auto-end grace. `end` locks the row and is idempotent, so
//!    duplicate execution by several instances or after a crash is harmless;
//! 3. expires stale occurrences (Blueprint Section 10, MTG-010): unstarted occurrences
//!    whose start window closed become CANCELLED (`system:expired`), claimed with
//!    `FOR UPDATE SKIP LOCKED`;
//! 4. purges expired `Idempotency-Key` records;
//! 6. audit M3: attends unattended waiting rooms ([`waiting_room_step`]);
//! 5. MF1: warns everyone in a LIVE conference [`AUTO_END_WARNING_MINUTES`] before its
//!    automatic end (`conference.ending_soon`, once per automatic-end instant, so an
//!    extension is warned again).
//!
//! No auto-start: starting is an authorized host decision. Notifications/reminders are
//! never authorization dependencies.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{sea_query::Expr, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde_json::json;
use tokio::sync::watch;

use crate::{
    domain::{ConferenceStatus, RecordOrigin, TerminationReason},
    error::ApiResult,
    models::{conference_sessions, meeting_spaces},
    services::{
        actor::ActorContext, conference::AUTO_END_WARNING_MINUTES, meeting_domain::MeetingDomain,
        meeting_events::event, meeting_space::schedule_policy,
    },
};

const BATCH: u64 = 50;

/// Auto-end eligibility of a LIVE conference, from its space's `schedule_policy`
/// (`autoEnd` default true, `autoEndGraceMinutes` default 15 — see `SchedulePolicy`).
const AUTO_END_DUE: &str = "EXISTS (SELECT 1 FROM meeting_spaces s \
    WHERE s.id = conference_sessions.meeting_space_id \
      AND CASE WHEN jsonb_typeof(s.schedule_policy -> 'autoEnd') = 'boolean' \
               THEN (s.schedule_policy ->> 'autoEnd')::boolean ELSE true END \
      AND conference_sessions.scheduled_end_at + make_interval(mins => \
            CASE WHEN jsonb_typeof(s.schedule_policy -> 'autoEndGraceMinutes') = 'number' \
                 THEN (s.schedule_policy ->> 'autoEndGraceMinutes')::int ELSE 15 END) <= $1)";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerTick {
    pub materialized: u64,
    pub end_requested: u64,
    /// Stale unstarted occurrences expired this tick.
    pub expired: u64,
    /// Gap audit M6: WAITING entries expired this tick (waited longer than
    /// `WAITING_TTL_MINUTES`, or their conference finished).
    pub waiting_expired: u64,
    /// MF1: `conference.ending_soon` warnings emitted this tick.
    pub end_warned: u64,
    /// Audit M3: waiting rooms changed this tick (notice, promotion or ended waits).
    pub waiting_rooms_attended: u64,
}

/// Audit M3: how long people may keep waiting after everyone who can admit them left
/// (the host may just be reconnecting) before the policy acts.
pub const UNATTENDED_GRACE_MINUTES: i64 = 2;
/// Audit M3: reason code of a wait ended because nobody could admit.
pub const NO_HOST_AVAILABLE: &str = "NO_HOST_AVAILABLE";
const NO_HOST_MESSAGE: &str =
    "Nobody who can let you in is in the meeting right now. Please try joining again later.";

/// What an unattended waiting room needs now (pure; see [`waiting_room_step`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitingRoomStep {
    Nothing,
    /// Tell everyone (the waiting people included) that nobody can admit right now.
    NoticeUnattended,
    /// Someone who can admit is present again after a notice.
    NoticeAttended,
    /// The policy allows it: promote this admitted participant to co-host.
    Promote(uuid::Uuid),
    /// End these waits (`NO_HOST_AVAILABLE`).
    EndWaits(Vec<uuid::Uuid>),
}

/// Audit M3 decision for one LIVE conference with people waiting.
/// * `admitter_present`: an admitted participant with an active session holds
///   `participant.admit`;
/// * `last_notice`: the latest unattended (`true`) / attended (`false`) notice and when;
/// * `waiting`: (participant, requested_at);
/// * `promotable`: the earliest-admitted present participant the policy lets the system
///   promote (`None` for regulated meetings or when nobody qualifies).
pub fn waiting_room_step(
    admitter_present: bool,
    last_notice: Option<(bool, DateTime<Utc>)>,
    waiting: &[(uuid::Uuid, DateTime<Utc>)],
    promotable: Option<uuid::Uuid>,
    now: DateTime<Utc>,
) -> WaitingRoomStep {
    let unattended_since = last_notice.filter(|(u, _)| *u).map(|(_, at)| at);
    if admitter_present {
        return if unattended_since.is_some() {
            WaitingRoomStep::NoticeAttended
        } else {
            WaitingRoomStep::Nothing
        };
    }
    if waiting.is_empty() {
        return WaitingRoomStep::Nothing;
    }
    let Some(since) = unattended_since else {
        return WaitingRoomStep::NoticeUnattended;
    };
    // Someone who knocked after the last notice has not been told yet.
    if waiting.iter().any(|(_, at)| *at > since) {
        return WaitingRoomStep::NoticeUnattended;
    }
    let grace = Duration::minutes(UNATTENDED_GRACE_MINUTES);
    if now < since + grace {
        return WaitingRoomStep::Nothing;
    }
    if let Some(p) = promotable {
        return WaitingRoomStep::Promote(p);
    }
    WaitingRoomStep::EndWaits(waiting.iter().map(|(id, _)| *id).collect())
}

/// MF1: LIVE v2 conferences whose automatic end (scheduled end + `autoEndGraceMinutes`,
/// `autoEnd` on) is at most `$2` minutes away and not yet past; `$1` = now.
const AUTO_END_SOON: &str = "SELECT c.id, c.scheduled_end_at + make_interval(mins => \
        CASE WHEN jsonb_typeof(s.schedule_policy -> 'autoEndGraceMinutes') = 'number' \
             THEN (s.schedule_policy ->> 'autoEndGraceMinutes')::int ELSE 15 END) AS auto_end_at \
      FROM conference_sessions c JOIN meeting_spaces s ON s.id = c.meeting_space_id \
     WHERE c.status = 'LIVE' AND c.origin = 'V2' AND c.scheduled_end_at IS NOT NULL \
       AND CASE WHEN jsonb_typeof(s.schedule_policy -> 'autoEnd') = 'boolean' \
                THEN (s.schedule_policy ->> 'autoEnd')::boolean ELSE true END";

/// MF1 / audit M2: emits `conference.ending_soon` for `conference_id` (automatic end
/// `auto_end`) once per automatic-end instant. The conference row is locked first, so
/// concurrent scheduler instances emit it once. Returns whether it was emitted now.
pub async fn warn_ending_soon(
    db: &sea_orm::DatabaseConnection,
    conference_id: uuid::Uuid,
    auto_end: DateTime<Utc>,
    now: DateTime<Utc>,
) -> ApiResult<bool> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
    let txn = db.begin().await?;
    let c = crate::services::meeting_core::lock_conference(&txn, conference_id).await?;
    if c.status != ConferenceStatus::Live || auto_end <= now {
        txn.rollback().await?;
        return Ok(false);
    }
    let already = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS x FROM outbox_events WHERE aggregate_type = 'conference' AND aggregate_id = $1 \
               AND event_type = $2 AND (payload ->> 'autoEndAt')::timestamptz = $3 LIMIT 1",
            [
                conference_id.into(),
                crate::services::meeting_events::event::CONFERENCE_ENDING_SOON.into(),
                auto_end.into(),
            ],
        ))
        .await?
        .is_some();
    if already {
        txn.rollback().await?;
        return Ok(false);
    }
    crate::services::meeting_events::emit(
        &txn,
        crate::services::conference::ending_soon_event(&c, auto_end, now),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

pub struct ConferenceScheduler {
    meeting: Arc<MeetingDomain>,
}

impl ConferenceScheduler {
    pub fn new(meeting: Arc<MeetingDomain>) -> Self {
        Self { meeting }
    }

    pub async fn tick(&self, now: DateTime<Utc>) -> ApiResult<SchedulerTick> {
        let materialized = self.meeting.recurrence.materialize_due(now, BATCH).await?;
        let end_requested = self.auto_end(now).await?;
        let expired = crate::services::calendar::expire_stale(&self.meeting.core.db, now).await?;
        crate::services::idempotency::purge_expired(&self.meeting.core.db, now).await?;
        let waiting_expired = self.expire_waiting(now).await?;
        let end_warned = self.warn_ending(now).await?;
        let waiting_rooms_attended = self.attend_waiting_rooms(now).await?;
        Ok(SchedulerTick {
            materialized,
            end_requested,
            expired,
            waiting_expired,
            end_warned,
            waiting_rooms_attended,
        })
    }

    /// Audit M3: LIVE conferences where people wait while nobody who can admit them is
    /// present. Everyone is told at once (`conference.waiting_room_unattended`, which the
    /// waiting people receive); after [`UNATTENDED_GRACE_MINUTES`] the meeting's policy
    /// acts: an ordinary meeting promotes its earliest-admitted present member (an active
    /// member of the meeting space with a CF Meet account) to co-host, so the queue can be
    /// handled; a regulated meeting (HEALTHCARE / HIGH_SECURITY / appointment) or one with
    /// nobody to promote ends the waits with `NO_HOST_AVAILABLE` and a clear message instead
    /// of leaving them until the 60-minute expiry. Meetings of a partner environment (CF Meet
    /// Platform) are left alone: their admissions are made through the API by the partner.
    /// Bounded per tick; idempotent.
    async fn attend_waiting_rooms(&self, now: DateTime<Utc>) -> ApiResult<u64> {
        use crate::domain::AdmissionState;
        use crate::models::conference_participants;
        use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
        if !self.meeting.core.flags.waiting_room_v2_enabled {
            return Ok(0);
        }
        let db = &self.meeting.core.db;
        let ids: Vec<uuid::Uuid> = db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT DISTINCT w.conference_id FROM conference_participants w \
                   JOIN conference_sessions c ON c.id = w.conference_id \
                   JOIN meeting_spaces s ON s.id = c.meeting_space_id \
                  WHERE w.admission_state = 'WAITING' AND c.status = 'LIVE' AND c.origin = 'V2' \
                    AND s.platform_environment_id IS NULL \
                  LIMIT $1",
                [(BATCH as i64).into()],
            ))
            .await?
            .iter()
            .filter_map(|r| r.try_get::<uuid::Uuid>("", "conference_id").ok())
            .collect();
        let mut changed = 0;
        for cid in ids {
            let txn = db.begin().await?;
            let conference = crate::services::meeting_core::lock_conference(&txn, cid).await?;
            if conference.status != ConferenceStatus::Live {
                txn.rollback().await?;
                continue;
            }
            let space =
                crate::services::meeting_core::find_space(&txn, conference.meeting_space_id)
                    .await?;
            let pctx = crate::services::meeting_permissions::PermissionContext::for_space(&space);
            let people = conference_participants::Entity::find()
                .filter(conference_participants::Column::ConferenceId.eq(cid))
                .filter(
                    conference_participants::Column::AdmissionState
                        .is_in([AdmissionState::Admitted, AdmissionState::Waiting]),
                )
                .order_by_asc(conference_participants::Column::AdmittedAt)
                .all(&txn)
                .await?;
            let present: std::collections::HashSet<uuid::Uuid> = txn
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT DISTINCT s.conference_participant_id AS id FROM participant_sessions s \
                       JOIN conference_participants p ON p.id = s.conference_participant_id \
                      WHERE p.conference_id = $1 AND s.state IN ('PENDING', 'CONNECTED')",
                    [cid.into()],
                ))
                .await?
                .iter()
                .filter_map(|r| r.try_get::<uuid::Uuid>("", "id").ok())
                .collect();
            let admitted_present: Vec<_> = people
                .iter()
                .filter(|p| {
                    p.admission_state == AdmissionState::Admitted && present.contains(&p.id)
                })
                .collect();
            let admitter_present = admitted_present.iter().any(|p| {
                pctx.allows(
                    p.role,
                    crate::services::meeting_permissions::Permission::ParticipantAdmit,
                )
            });
            let waiting: Vec<(uuid::Uuid, DateTime<Utc>)> = people
                .iter()
                .filter(|p| p.admission_state == AdmissionState::Waiting)
                .map(|p| (p.id, p.requested_at))
                .collect();
            let last_notice = txn
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT event_type, created_at FROM outbox_events \
                      WHERE aggregate_type = 'conference' AND aggregate_id = $1 AND event_type IN ($2, $3) \
                      ORDER BY created_at DESC, id DESC LIMIT 1",
                    [
                        cid.into(),
                        event::CONFERENCE_WAITING_ROOM_UNATTENDED.into(),
                        event::CONFERENCE_WAITING_ROOM_ATTENDED.into(),
                    ],
                ))
                .await?
                .and_then(|r| {
                    Some((
                        r.try_get::<String>("", "event_type").ok()?
                            == event::CONFERENCE_WAITING_ROOM_UNATTENDED,
                        r.try_get::<DateTime<Utc>>("", "created_at").ok()?,
                    ))
                });
            let regulated = matches!(
                space.security_profile,
                crate::domain::SecurityProfile::Healthcare
                    | crate::domain::SecurityProfile::HighSecurity
            ) || space.meeting_type == crate::domain::MeetingType::Healthcare
                || space.access_policy == crate::domain::AccessPolicy::HealthcareAppointment;
            let promotable = if regulated {
                None
            } else {
                let mut found = None;
                for p in &admitted_present {
                    let Some(user) = p.user_id else { continue };
                    if p.identity_type != crate::domain::IdentityType::RegisteredUser
                        || p.role.rank() >= crate::domain::Role::CoHost.rank()
                    {
                        continue;
                    }
                    let member = crate::models::meeting_members::Entity::find()
                        .filter(crate::models::meeting_members::Column::MeetingSpaceId.eq(space.id))
                        .filter(crate::models::meeting_members::Column::UserId.eq(user))
                        .filter(
                            crate::models::meeting_members::Column::Status
                                .eq(crate::domain::MemberStatus::Active),
                        )
                        .one(&txn)
                        .await?;
                    if member.is_some() || space.owner_user_id == Some(user) {
                        found = Some(p.id);
                        break;
                    }
                }
                found
            };
            let step = waiting_room_step(admitter_present, last_notice, &waiting, promotable, now);
            let notice = |kind: &'static str| {
                let mut record = crate::services::conference::conference_event(&conference, kind);
                record.payload["waitingCount"] = json!(waiting.len());
                if kind == event::CONFERENCE_WAITING_ROOM_UNATTENDED {
                    record.payload["actionAt"] =
                        json!(now + Duration::minutes(UNATTENDED_GRACE_MINUTES));
                    record.payload["policy"] = json!(if promotable.is_some() {
                        "PROMOTE_MEMBER"
                    } else {
                        "END_WAIT"
                    });
                }
                record
            };
            match step {
                WaitingRoomStep::Nothing => {
                    txn.rollback().await?;
                    continue;
                }
                WaitingRoomStep::NoticeUnattended => {
                    crate::services::meeting_events::emit(
                        &txn,
                        notice(event::CONFERENCE_WAITING_ROOM_UNATTENDED),
                    )
                    .await?;
                    txn.commit().await?;
                }
                WaitingRoomStep::NoticeAttended => {
                    crate::services::meeting_events::emit(
                        &txn,
                        notice(event::CONFERENCE_WAITING_ROOM_ATTENDED),
                    )
                    .await?;
                    txn.commit().await?;
                }
                WaitingRoomStep::Promote(pid) => {
                    txn.rollback().await?;
                    let system = ActorContext::system("waiting-room-attendant");
                    if let Err(e) = self
                        .meeting
                        .admission
                        .change_role(
                            &system,
                            cid,
                            pid,
                            &crate::dto::v2::RoleChangeRequest {
                                role: crate::domain::Role::CoHost,
                                version: None,
                            },
                        )
                        .await
                    {
                        tracing::warn!(conference_id = %cid, "waiting-room promotion failed: {e}");
                        continue;
                    }
                }
                WaitingRoomStep::EndWaits(ids) => {
                    txn.rollback().await?;
                    let system = ActorContext::system("waiting-room-attendant");
                    if let Err(e) = self
                        .meeting
                        .admission
                        .deny(
                            &system,
                            cid,
                            &crate::dto::v2::DenyRequest {
                                participant_ids: ids,
                                all: false,
                                reason_code: Some(NO_HOST_AVAILABLE.into()),
                                message: Some(NO_HOST_MESSAGE.into()),
                                internal_note: Some(
                                    "No host or co-host present; ended by the waiting-room policy"
                                        .into(),
                                ),
                            },
                        )
                        .await
                    {
                        tracing::warn!(conference_id = %cid, "ending unattended waits failed: {e}");
                        continue;
                    }
                }
            }
            changed += 1;
        }
        Ok(changed)
    }

    /// MF1: `conference.ending_soon` for LIVE conferences whose automatic end is at most
    /// [`AUTO_END_WARNING_MINUTES`] away (bounded per tick; idempotent).
    async fn warn_ending(&self, now: DateTime<Utc>) -> ApiResult<u64> {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        let db = &self.meeting.core.db;
        let rows = db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT id, auto_end_at FROM ({AUTO_END_SOON}) d \
                      WHERE d.auto_end_at > $1 AND d.auto_end_at <= $1 + make_interval(mins => $2) \
                      ORDER BY d.auto_end_at LIMIT {BATCH}"
                ),
                [now.into(), (AUTO_END_WARNING_MINUTES as i32).into()],
            ))
            .await?;
        let mut warned = 0;
        for r in rows {
            let (Ok(id), Ok(at)) = (
                r.try_get::<uuid::Uuid>("", "id"),
                r.try_get::<DateTime<Utc>>("", "auto_end_at"),
            ) else {
                continue;
            };
            match warn_ending_soon(db, id, at, now).await {
                Ok(true) => warned += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(conference_id = %id, "ending-soon warning failed: {e}"),
            }
        }
        Ok(warned)
    }

    /// Gap audit M6: WAITING entries that waited longer than the TTL, or whose conference is
    /// no longer LIVE / about to start, leave the queue as DENIED (`WAITING_EXPIRED`).
    /// Bounded per tick; idempotent.
    async fn expire_waiting(&self, now: DateTime<Utc>) -> ApiResult<u64> {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        let result = self
            .meeting
            .core
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE conference_participants p
                    SET admission_state = 'DENIED', denied_at = $1,
                        denied_by = 'system:waiting-expired', denial_reason_code = 'WAITING_EXPIRED',
                        version = p.version + 1, updated_at = $1
                  WHERE p.id IN (
                        SELECT w.id FROM conference_participants w
                          JOIN conference_sessions c ON c.id = w.conference_id
                         WHERE w.admission_state = 'WAITING'
                           AND (w.requested_at < $1 - make_interval(mins => $2)
                                OR c.status IN ('ENDED', 'CANCELLED', 'ENDING'))
                         ORDER BY w.requested_at
                         LIMIT 500
                         FOR UPDATE OF w SKIP LOCKED)",
                [
                    now.into(),
                    (crate::services::admission::WAITING_TTL_MINUTES as i32).into(),
                ],
            ))
            .await?;
        Ok(result.rows_affected())
    }

    async fn auto_end(&self, now: DateTime<Utc>) -> ApiResult<u64> {
        let db = &self.meeting.core.db;
        // Eligibility (auto-end enabled, grace elapsed) is evaluated in SQL, mirroring
        // `SchedulePolicy` defaults, so conferences that overrun but must keep running
        // never occupy the bounded batch and starve due ones.
        let candidates = conference_sessions::Entity::find()
            .filter(conference_sessions::Column::Status.eq(ConferenceStatus::Live))
            .filter(conference_sessions::Column::Origin.eq(RecordOrigin::V2))
            .filter(conference_sessions::Column::ScheduledEndAt.lt(now))
            .filter(Expr::cust_with_values(AUTO_END_DUE, [now]))
            .order_by_asc(conference_sessions::Column::ScheduledEndAt)
            .limit(BATCH)
            .all(db)
            .await?;
        let system = ActorContext::system("conference-scheduler");
        let mut ended = 0;
        for c in candidates {
            let Some(space) = meeting_spaces::Entity::find_by_id(c.meeting_space_id)
                .one(db)
                .await?
            else {
                continue;
            };
            let policy = schedule_policy(&space);
            let due = c
                .scheduled_end_at
                .map(|end| end + Duration::minutes(policy.auto_end_grace_minutes as i64) <= now)
                .unwrap_or(false);
            if !policy.auto_end || !due {
                continue;
            }
            match self
                .meeting
                .conferences
                .end(&system, c.id, TerminationReason::ScheduledEnd)
                .await
            {
                Ok(outcome) if outcome.changed => ended += 1,
                Ok(_) => {}
                Err(e) => tracing::warn!(conference_id = %c.id, "scheduled end failed: {e}"),
            }
        }
        Ok(ended)
    }

    pub async fn run(&self, poll_secs: u64, mut shutdown: watch::Receiver<bool>) {
        tracing::info!("Conference scheduler started (polling every {poll_secs}s)");
        loop {
            if *shutdown.borrow() {
                break;
            }
            match self.tick(Utc::now()).await {
                Ok(t) if t != SchedulerTick::default() => {
                    tracing::info!(?t, "conference scheduler tick")
                }
                Ok(_) => {}
                // PostgreSQL unavailable: do nothing (never act on stale state), retry next tick.
                Err(e) => tracing::error!("conference scheduler tick failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(poll_secs)) => {}
                _ = shutdown.changed() => {}
            }
        }
        tracing::info!("Conference scheduler stopped");
    }
}

pub struct ConferenceSchedulerHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl ConferenceSchedulerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

pub fn spawn_conference_scheduler(meeting: Arc<MeetingDomain>) -> ConferenceSchedulerHandle {
    let poll = meeting.core.config.scheduler_poll_secs.max(5);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let scheduler = ConferenceScheduler::new(meeting);
    tokio::spawn(async move { scheduler.run(poll, shutdown_rx).await });
    ConferenceSchedulerHandle { shutdown_tx }
}

#[cfg(test)]
mod waiting_room_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn unattended_waiting_rooms_are_told_then_handled_by_policy() {
        let t0 = Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap();
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let waiting = [(a, t0 - Duration::minutes(5))];
        // A host is present: nothing (or "attended" after an absence notice).
        assert_eq!(
            waiting_room_step(true, None, &waiting, None, t0),
            WaitingRoomStep::Nothing
        );
        assert_eq!(
            waiting_room_step(true, Some((true, t0)), &waiting, None, t0),
            WaitingRoomStep::NoticeAttended
        );
        assert_eq!(
            waiting_room_step(true, Some((false, t0)), &waiting, None, t0),
            WaitingRoomStep::Nothing
        );
        // Nobody can admit: tell everyone first.
        assert_eq!(
            waiting_room_step(false, None, &waiting, Some(b), t0),
            WaitingRoomStep::NoticeUnattended
        );
        assert_eq!(
            waiting_room_step(false, Some((false, t0)), &waiting, Some(b), t0),
            WaitingRoomStep::NoticeUnattended
        );
        // Within the grace: wait for the host to come back.
        let told = t0;
        let soon = t0 + Duration::minutes(UNATTENDED_GRACE_MINUTES) - Duration::seconds(1);
        assert_eq!(
            waiting_room_step(false, Some((true, told)), &waiting, Some(b), soon),
            WaitingRoomStep::Nothing
        );
        // Then the policy: promote, or end the waits with a reason.
        let later = t0 + Duration::minutes(UNATTENDED_GRACE_MINUTES);
        assert_eq!(
            waiting_room_step(false, Some((true, told)), &waiting, Some(b), later),
            WaitingRoomStep::Promote(b)
        );
        assert_eq!(
            waiting_room_step(false, Some((true, told)), &waiting, None, later),
            WaitingRoomStep::EndWaits(vec![a])
        );
        // Someone knocking after the notice is told before anything happens to them.
        let new = [
            (a, t0 - Duration::minutes(5)),
            (b, t0 + Duration::seconds(30)),
        ];
        assert_eq!(
            waiting_room_step(false, Some((true, told)), &new, None, later),
            WaitingRoomStep::NoticeUnattended
        );
        // Nobody waiting: nothing to do.
        assert_eq!(
            waiting_room_step(false, Some((true, told)), &[], None, later),
            WaitingRoomStep::Nothing
        );
    }
}
