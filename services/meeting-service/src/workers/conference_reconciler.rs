//! Conference reconciler (spec Part E §10). Compares PostgreSQL authority with the RTC
//! provider and repairs operational drift.
//!
//! Detected drift (and the action taken):
//!
//! | drift | action |
//! |---|---|
//! | PREPARING conference without a confirmed room | ensure room → WAITING (repair) |
//! | LIVE conference missing its provider room | re-create room (repair) |
//! | ENDING conference whose room still exists | terminate → confirm absence → ENDED (repair) |
//! | ENDED / CANCELLED / FAILED conference with an orphan room | terminate (repair) |
//! | provider `cf-` room with no conference at all | escalate (never guessed) |
//! | provider participant without an active ParticipantSession | evict (repair) |
//! | provider participant whose PENDING session is not yet CONNECTED (late webhook) | mark CONNECTED (repair) |
//! | session CONNECTED but absent at the provider (beyond grace) | mark DISCONNECTED (repair) |
//! | provider grants ≠ permission engine grants | push authoritative grants (repair) |
//!
//! Safe, deterministic drift is repaired because PostgreSQL says exactly what must be
//! true; anything ambiguous is recorded as ESCALATED in `rtc_drift_events` and logged.
//! The worker is idempotent (every repair is), bounded (`reconciler_batch` per class),
//! multi-instance safe (a `worker_leases` lease makes one instance active; repairs are
//! idempotent even if two overlap) and observable (report + metrics + drift log).

use std::collections::{HashMap, HashSet};

use chrono::{Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde_json::{json, Value};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, ConferenceStatus, RecordOrigin, SessionState},
    error::ApiResult,
    models::{conference_participants, conference_sessions, participant_sessions},
    services::{
        meeting_core::{find_space, lock_conference, Core},
        meeting_events::{emit, event, OutboxRecord},
        meeting_permissions::PermissionContext,
        outbox,
        rtc_control::RtcControl,
        rtc_metrics::{inc, RTC_METRICS as M},
        rtc_provider::{ProviderGrants, RoomSpec, RtcParticipantRef, RtcRoomRef},
    },
};

const LEASE_NAME: &str = "conference_reconciler";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Another instance holds the lease; nothing was done.
    pub skipped_lease: bool,
    pub disabled: bool,
    pub examined: u64,
    pub drift_detected: u64,
    pub repaired: u64,
    pub escalated: u64,
    pub failed: u64,
    /// Drift kinds seen in this run.
    pub kinds: Vec<String>,
}

pub struct ConferenceReconciler {
    core: Core,
    control: RtcControl,
    holder: String,
}

enum Action {
    Repaired,
    Escalated,
    Failed,
}

impl ConferenceReconciler {
    pub fn new(core: Core) -> Self {
        Self {
            control: RtcControl::new(core.clone()),
            core,
            holder: outbox::holder_id("reconciler"),
        }
    }

    /// Acquire (or renew) the singleton lease.
    async fn acquire_lease(&self, secs: i64) -> ApiResult<bool> {
        let row = self
            .core
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO worker_leases (name, holder, expires_at, updated_at)
                 VALUES ($1, $2, now() + make_interval(secs => $3), now())
                 ON CONFLICT (name) DO UPDATE
                   SET holder = EXCLUDED.holder, expires_at = EXCLUDED.expires_at, updated_at = now()
                 WHERE worker_leases.expires_at < now() OR worker_leases.holder = EXCLUDED.holder
                 RETURNING holder",
                [LEASE_NAME.into(), self.holder.clone().into(), secs.into()],
            ))
            .await?;
        Ok(row.is_some())
    }

    pub async fn release_lease(&self) -> ApiResult<()> {
        self.core
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM worker_leases WHERE name = $1 AND holder = $2",
                [LEASE_NAME.into(), self.holder.clone().into()],
            ))
            .await?;
        Ok(())
    }

    async fn record(
        &self,
        report: &mut ReconcileReport,
        conference_id: Option<Uuid>,
        room_name: Option<&str>,
        kind: &str,
        action: Action,
        detail: Value,
    ) {
        report.drift_detected += 1;
        inc(&M.drift_detected);
        if !report.kinds.iter().any(|k| k == kind) {
            report.kinds.push(kind.to_string());
        }
        let action_str = match action {
            Action::Repaired => {
                report.repaired += 1;
                inc(&M.drift_repaired);
                "REPAIRED"
            }
            Action::Escalated => {
                report.escalated += 1;
                inc(&M.drift_escalated);
                tracing::error!(?conference_id, room_name, kind, alert = "RTC_DRIFT_ESCALATED", %detail, "reconciler escalated drift");
                "ESCALATED"
            }
            Action::Failed => {
                report.failed += 1;
                tracing::warn!(?conference_id, room_name, kind, %detail, "reconciler repair failed");
                "FAILED"
            }
        };
        let _ = self
            .core
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO rtc_drift_events (id, conference_id, room_name, kind, action, detail)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                [
                    Uuid::new_v4().into(),
                    conference_id.into(),
                    room_name.map(str::to_string).into(),
                    kind.into(),
                    action_str.into(),
                    detail.into(),
                ],
            ))
            .await;
    }

    async fn conferences(
        &self,
        statuses: &[ConferenceStatus],
        extra: Option<sea_orm::Condition>,
    ) -> ApiResult<Vec<conference_sessions::Model>> {
        let mut q = conference_sessions::Entity::find()
            .filter(conference_sessions::Column::Origin.eq(RecordOrigin::V2))
            .filter(conference_sessions::Column::Status.is_in(statuses.iter().copied()));
        if let Some(c) = extra {
            q = q.filter(c);
        }
        Ok(q.order_by_asc(conference_sessions::Column::UpdatedAt)
            .limit(self.core.config.reconciler_batch)
            .all(&self.core.db)
            .await?)
    }

    /// One bounded reconciliation pass.
    pub async fn tick(&self) -> ApiResult<ReconcileReport> {
        let mut report = ReconcileReport::default();
        if !self.control.room_control() {
            report.disabled = true;
            return Ok(report);
        }
        let lease = (self.core.config.reconciler_poll_secs as i64 * 2).max(10);
        if !self.acquire_lease(lease).await? {
            report.skipped_lease = true;
            return Ok(report);
        }
        inc(&M.reconciler_runs);

        // 1. PREPARING without a confirmed room.
        for c in self
            .conferences(&[ConferenceStatus::Preparing], None)
            .await?
        {
            report.examined += 1;
            let action = match self.control.ensure_room(c.id).await {
                Ok(_) => Action::Repaired,
                Err(_) => Action::Failed,
            };
            self.record(
                &mut report,
                Some(c.id),
                c.rtc_room_name.as_deref(),
                "PREPARING_NOT_READY",
                action,
                json!({}),
            )
            .await;
        }

        // 2. ENDING: terminate → confirm absence → ENDED.
        for c in self.conferences(&[ConferenceStatus::Ending], None).await? {
            report.examined += 1;
            let action = match self.control.finalize_room(c.id).await {
                Ok(_) => Action::Repaired,
                Err(_) => Action::Failed,
            };
            self.record(
                &mut report,
                Some(c.id),
                c.rtc_room_name.as_deref(),
                "ENDING_NOT_FINALIZED",
                action,
                json!({}),
            )
            .await;
        }

        // 3. Closed conferences whose room may still exist (recent, bounded).
        let since = Utc::now() - Duration::hours(24);
        let closed = self
            .conferences(
                &[
                    ConferenceStatus::Ended,
                    ConferenceStatus::Cancelled,
                    ConferenceStatus::Failed,
                ],
                Some(
                    sea_orm::Condition::all()
                        .add(conference_sessions::Column::UpdatedAt.gte(since))
                        .add(conference_sessions::Column::RtcRoomName.is_not_null()),
                ),
            )
            .await?;
        for c in closed {
            report.examined += 1;
            let room = RtcRoomRef::for_conference(&c);
            match self.core.rtc.get_room(&room).await {
                Ok(Some(_)) => {
                    let action = match self.core.rtc.terminate_room(&room, "ORPHAN").await {
                        Ok(()) => Action::Repaired,
                        Err(_) => Action::Failed,
                    };
                    self.record(
                        &mut report,
                        Some(c.id),
                        Some(&room.room_name),
                        "ORPHAN_ROOM",
                        action,
                        json!({ "status": c.status }),
                    )
                    .await;
                }
                Ok(None) => {}
                Err(e) => {
                    self.record(
                        &mut report,
                        Some(c.id),
                        Some(&room.room_name),
                        "PROVIDER_UNAVAILABLE",
                        Action::Failed,
                        json!({ "error": e.to_string() }),
                    )
                    .await;
                }
            }
        }

        // 4. LIVE conferences: room presence, participants, grants.
        let live = self.conferences(&[ConferenceStatus::Live], None).await?;
        let mut live_rooms = HashSet::new();
        for c in live {
            report.examined += 1;
            live_rooms.insert(RtcRoomRef::for_conference(&c).room_name);
            if let Err(e) = self.reconcile_live(&c, &mut report).await {
                self.record(
                    &mut report,
                    Some(c.id),
                    c.rtc_room_name.as_deref(),
                    "RECONCILE_ERROR",
                    Action::Failed,
                    json!({ "error": e.to_string() }),
                )
                .await;
            }
        }

        // 5. Provider rooms with no valid active conference.
        if let Ok(rooms) = self.core.rtc.list_rooms("cf-").await {
            for room in rooms
                .into_iter()
                .take(self.core.config.reconciler_batch as usize)
            {
                if live_rooms.contains(&room.room_name) {
                    continue;
                }
                let conference = conference_sessions::Entity::find()
                    .filter(conference_sessions::Column::RtcRoomName.eq(room.room_name.clone()))
                    .one(&self.core.db)
                    .await?;
                match conference {
                    None => {
                        // Not ours to judge: never delete a room we cannot map.
                        self.record(
                            &mut report,
                            None,
                            Some(&room.room_name),
                            "UNMAPPED_PROVIDER_ROOM",
                            Action::Escalated,
                            json!({ "participants": room.participant_count }),
                        )
                        .await;
                    }
                    Some(c)
                        if matches!(
                            c.status,
                            ConferenceStatus::Ended
                                | ConferenceStatus::Cancelled
                                | ConferenceStatus::Failed
                                | ConferenceStatus::Created
                        ) =>
                    {
                        let action = match self
                            .core
                            .rtc
                            .terminate_room(&RtcRoomRef::for_conference(&c), "NO_ACTIVE_CONFERENCE")
                            .await
                        {
                            Ok(()) => Action::Repaired,
                            Err(_) => Action::Failed,
                        };
                        self.record(
                            &mut report,
                            Some(c.id),
                            Some(&room.room_name),
                            "ROOM_WITHOUT_ACTIVE_CONFERENCE",
                            action,
                            json!({ "status": c.status }),
                        )
                        .await;
                    }
                    // LIVE beyond this batch, ENDING / PREPARING / WAITING: handled above.
                    Some(_) => {}
                }
            }
        }
        Ok(report)
    }

    async fn reconcile_live(
        &self,
        c: &conference_sessions::Model,
        report: &mut ReconcileReport,
    ) -> ApiResult<()> {
        let room = RtcRoomRef::for_conference(c);
        let snapshot = match self.core.rtc.reconcile(&room).await {
            Ok(s) => s,
            Err(e) => {
                self.record(
                    report,
                    Some(c.id),
                    Some(&room.room_name),
                    "PROVIDER_UNAVAILABLE",
                    Action::Failed,
                    json!({ "error": e.to_string() }),
                )
                .await;
                return Ok(());
            }
        };
        if snapshot.room.is_none() {
            // Deterministic: the conference is LIVE, so its room must exist.
            let action = match self.core.rtc.create_room(&room, &RoomSpec::default()).await {
                Ok(info) => {
                    let txn = self.core.db.begin().await?;
                    let current = lock_conference(&txn, c.id).await?;
                    if current.status == ConferenceStatus::Live {
                        let mut am: conference_sessions::ActiveModel = current.clone().into();
                        am.rtc_room_id = Set(info.room_id);
                        am.rtc_room_ready_at = Set(Some(Utc::now()));
                        am.updated_at = Set(Utc::now());
                        am.update(&txn).await?;
                    }
                    txn.commit().await?;
                    Action::Repaired
                }
                Err(_) => Action::Failed,
            };
            self.record(
                report,
                Some(c.id),
                Some(&room.room_name),
                "LIVE_ROOM_MISSING",
                action,
                json!({}),
            )
            .await;
            // Sessions marked CONNECTED cannot be connected to a missing room.
            self.close_absent_sessions(c, &HashSet::new(), report)
                .await?;
            return Ok(());
        }

        let space = find_space(&self.core.db, c.meeting_space_id).await?;
        let pctx = PermissionContext::for_space(&space);
        let sessions = participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceId.eq(c.id))
            .filter(
                participant_sessions::Column::State
                    .is_in([SessionState::Pending, SessionState::Connected]),
            )
            .all(&self.core.db)
            .await?;
        let by_identity: HashMap<&str, &participant_sessions::Model> = sessions
            .iter()
            .map(|s| (s.media_identity.as_str(), s))
            .collect();
        let participant_ids: Vec<Uuid> = sessions
            .iter()
            .map(|s| s.conference_participant_id)
            .collect();
        let participants: HashMap<Uuid, conference_participants::Model> =
            conference_participants::Entity::find()
                .filter(conference_participants::Column::Id.is_in(participant_ids))
                .all(&self.core.db)
                .await?
                .into_iter()
                .map(|p| (p.id, p))
                .collect();

        let mut present = HashSet::new();
        for p in snapshot.participants.iter().filter(|p| p.connected) {
            present.insert(p.media_identity.clone());
            let session = by_identity.get(p.media_identity.as_str()).copied();
            let participant = session.and_then(|s| participants.get(&s.conference_participant_id));
            let authorized = participant
                .map(|pp| pp.admission_state == AdmissionState::Admitted)
                .unwrap_or(false);
            let target = RtcParticipantRef {
                conference_id: c.id,
                room_name: room.room_name.clone(),
                participant_session_id: session.map(|s| s.id).unwrap_or(Uuid::nil()),
                media_identity: p.media_identity.clone(),
            };
            let (Some(session), true) = (session, authorized) else {
                let action = match self
                    .core
                    .rtc
                    .remove_participant(&target, "NO_ACTIVE_SESSION")
                    .await
                {
                    Ok(()) => Action::Repaired,
                    Err(_) => Action::Failed,
                };
                self.record(
                    report,
                    Some(c.id),
                    Some(&room.room_name),
                    "PROVIDER_PARTICIPANT_WITHOUT_SESSION",
                    action,
                    json!({}),
                )
                .await;
                continue;
            };
            let participant = participant.expect("authorized implies participant");
            if session.state == SessionState::Pending {
                // Webhook delayed/lost: the provider is authoritative for connectivity.
                let action = match self.mark_connected(c, session, participant).await {
                    Ok(()) => Action::Repaired,
                    Err(_) => Action::Failed,
                };
                self.record(
                    report,
                    Some(c.id),
                    Some(&room.room_name),
                    "SESSION_CONNECTED_NOT_RECORDED",
                    action,
                    json!({ "participantSessionId": session.id }),
                )
                .await;
            }
            let expected = ProviderGrants::from_permissions(&pctx.media(participant.role));
            if p.grants.as_ref() != Some(&expected) {
                let action = match self
                    .core
                    .rtc
                    .update_permissions(&target, &pctx.media(participant.role))
                    .await
                {
                    Ok(()) => Action::Repaired,
                    Err(_) => Action::Failed,
                };
                self.record(
                    report,
                    Some(c.id),
                    Some(&room.room_name),
                    "PERMISSION_DRIFT",
                    action,
                    json!({ "participantId": participant.id, "role": participant.role }),
                )
                .await;
            }
        }
        self.close_absent_sessions(c, &present, report).await
    }

    async fn mark_connected(
        &self,
        c: &conference_sessions::Model,
        session: &participant_sessions::Model,
        participant: &conference_participants::Model,
    ) -> ApiResult<()> {
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, c.id).await?;
        let current = participant_sessions::Entity::find_by_id(session.id)
            .one(&txn)
            .await?;
        if let Some(current) = current.filter(|s| s.state == SessionState::Pending) {
            let now = Utc::now();
            let mut am: participant_sessions::ActiveModel = current.clone().into();
            am.state = Set(SessionState::Connected);
            am.connected_at = Set(Some(now));
            am.updated_at = Set(now);
            let updated = am.update(&txn).await?;
            emit(
                &txn,
                OutboxRecord {
                    aggregate_type: "participant_session",
                    aggregate_id: updated.id,
                    event_type: event::PARTICIPANT_JOINED,
                    meeting_space_id: Some(c.meeting_space_id),
                    conference_id: Some(c.id),
                    payload: json!({
                        "conferenceId": c.id,
                        "participantId": participant.id,
                        "participantSessionId": updated.id,
                        "role": participant.role,
                        "admissionState": participant.admission_state,
                        "version": participant.version,
                        "source": "RECONCILER",
                    }),
                },
            )
            .await?;
        }
        txn.commit().await?;
        Ok(())
    }

    /// CONNECTED sessions the provider no longer reports (beyond the grace period).
    async fn close_absent_sessions(
        &self,
        c: &conference_sessions::Model,
        present: &HashSet<String>,
        report: &mut ReconcileReport,
    ) -> ApiResult<()> {
        let grace = Utc::now() - Duration::seconds(self.core.config.reconciler_presence_grace_secs);
        let stale: Vec<participant_sessions::Model> = participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceId.eq(c.id))
            .filter(participant_sessions::Column::State.eq(SessionState::Connected))
            .filter(participant_sessions::Column::ConnectedAt.lte(grace))
            .all(&self.core.db)
            .await?
            .into_iter()
            .filter(|s| !present.contains(&s.media_identity))
            .collect();
        for s in stale {
            let txn = self.core.db.begin().await?;
            lock_conference(&txn, c.id).await?;
            let current = participant_sessions::Entity::find_by_id(s.id)
                .one(&txn)
                .await?;
            if let Some(current) = current.filter(|x| x.state == SessionState::Connected) {
                let now = Utc::now();
                let mut am: participant_sessions::ActiveModel = current.clone().into();
                am.state = Set(SessionState::Disconnected);
                am.disconnected_at = Set(Some(now));
                am.end_reason = Set(Some("PROVIDER_ABSENT".into()));
                am.updated_at = Set(now);
                let updated = am.update(&txn).await?;
                emit(
                    &txn,
                    OutboxRecord {
                        aggregate_type: "conference_participant",
                        aggregate_id: updated.conference_participant_id,
                        event_type: event::PARTICIPANT_LEFT,
                        meeting_space_id: Some(c.meeting_space_id),
                        conference_id: Some(c.id),
                        payload: json!({
                            "conferenceId": c.id,
                            "participantId": updated.conference_participant_id,
                            "participantSessionId": updated.id,
                            "sessionIds": [updated.id],
                            "source": "RECONCILER",
                        }),
                    },
                )
                .await?;
            }
            txn.commit().await?;
            self.record(
                report,
                Some(c.id),
                c.rtc_room_name.as_deref(),
                "SESSION_CONNECTED_BUT_ABSENT",
                Action::Repaired,
                json!({ "participantSessionId": s.id }),
            )
            .await;
        }
        Ok(())
    }
}

pub fn spawn_conference_reconciler(
    reconciler: ConferenceReconciler,
    poll_secs: u64,
) -> watch::Sender<bool> {
    let (tx, mut rx) = watch::channel(false);
    tokio::spawn(async move {
        loop {
            if *rx.borrow() {
                break;
            }
            match reconciler.tick().await {
                Ok(r) if r.drift_detected > 0 => tracing::info!(
                    examined = r.examined,
                    drift = r.drift_detected,
                    repaired = r.repaired,
                    escalated = r.escalated,
                    failed = r.failed,
                    kinds = ?r.kinds,
                    "conference reconciler pass"
                ),
                Ok(_) => {}
                Err(e) => tracing::error!("conference reconciler pass failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(poll_secs)) => {}
                _ = rx.changed() => {}
            }
        }
        let _ = reconciler.release_lease().await;
    });
    tx
}
