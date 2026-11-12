//! Empty instant conferences (final completion, Section 3) and the archived-space
//! invariant (Section 4).
//!
//! `POST /api/v2/instant-meetings` creates a LIVE conference immediately. If nobody is in
//! it for `CONFERENCE_EMPTY_TIMEOUT_SECS` (default 600 s = 10 minutes) of continuous
//! emptiness it is ended with `EMPTY_TIMEOUT` through the canonical end path (LIVE →
//! ENDING → provider termination → ENDED, `conference.ending` / `conference.ended`).
//!
//! **Presence** (per tick, for a bounded batch of LIVE v2 INSTANT conferences, least
//! recently checked first):
//! * provider participant control available (LiveKit): the provider room's participants
//!   (recorders excluded) are the authority for "someone is in the media room"; a
//!   participant session established less than `JOIN_GRACE` ago (credential issued,
//!   still connecting) also counts as present;
//! * otherwise: an unexpired PENDING/CONNECTED participant session counts as present;
//! * the provider cannot be asked (error/timeout): presence is UNKNOWN and nothing changes
//!   (never end on missing evidence).
//!
//! **Empty interval** (`conference_presence`, durable, restart safe): present → the
//! interval is cleared and `last_occupied_at` recorded; empty → the interval starts at the
//! latest evidence of activity: the conference start, the last time anyone was observed,
//! and the last session establishment or disconnection (so a creation that nobody ever
//! joined is empty from its start, and a restart never shortens an interval). When it is
//! at least the timeout old, [`ConferenceService::end_if_still_empty`] re-checks under the
//! conference row lock (a join racing the sweeper resets the interval instead).
//!
//! Scheduled and recurring conferences keep their own lifecycle (scheduled end + grace);
//! no subscription- or plan-based duration limit exists (owner decision): a conference
//! with participants is never ended for its age.
//!
//! **Archived spaces**: any LIVE conference whose space is ARCHIVED (e.g. archived before
//! archive ended conferences) is ended at once (repair; `ADMIN`, cause `SPACE_ARCHIVED`).
//!
//! Multi-instance safe: every decision is re-checked under row locks and every transition
//! is idempotent; batches are bounded.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    error::ApiResult,
    services::{
        meeting_domain::MeetingDomain,
        ops_metrics::{inc, OPS_METRICS as OM},
        rtc_provider::RtcRoomRef,
    },
};

pub const DEFAULT_EMPTY_TIMEOUT_SECS: i64 = 600;
const JOIN_GRACE_SECS: i64 = 120;
const BATCH: i64 = 100;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EmptySweepReport {
    pub checked: u64,
    pub occupied: u64,
    pub empty: u64,
    pub unknown: u64,
    pub ended: u64,
    pub archived_repaired: u64,
}

#[derive(Debug, FromQueryResult)]
struct Candidate {
    id: Uuid,
    rtc_room_name: Option<String>,
    baseline: DateTime<Utc>,
    empty_since: Option<DateTime<Utc>>,
}

#[derive(Debug, FromQueryResult)]
struct ActiveSession {
    conference_id: Uuid,
    state: String,
    established_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Occupied,
    Empty,
    Unknown,
}

/// Pure presence rule (unit tested).
pub fn presence(
    provider_humans: Option<Result<usize, ()>>,
    db_states: &[(&str, DateTime<Utc>)],
    now: DateTime<Utc>,
) -> Presence {
    let joining = db_states
        .iter()
        .any(|(s, at)| *s == "PENDING" && now - *at < Duration::seconds(JOIN_GRACE_SECS));
    match provider_humans {
        Some(Ok(n)) if n > 0 => Presence::Occupied,
        Some(Ok(_)) if joining => Presence::Occupied,
        Some(Ok(_)) => Presence::Empty,
        Some(Err(())) => Presence::Unknown,
        None if db_states.is_empty() => Presence::Empty,
        None => Presence::Occupied,
    }
}

pub struct EmptyConferenceSweeper {
    meeting: Arc<MeetingDomain>,
    timeout: Duration,
}

pub fn empty_timeout_from_env() -> Duration {
    Duration::seconds(
        std::env::var("CONFERENCE_EMPTY_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(DEFAULT_EMPTY_TIMEOUT_SECS)
            .clamp(120, 24 * 3600),
    )
}

impl EmptyConferenceSweeper {
    pub fn new(meeting: Arc<MeetingDomain>, timeout: Duration) -> Self {
        Self { meeting, timeout }
    }

    /// End LIVE conferences of archived spaces (repair of the archive invariant).
    async fn repair_archived(&self) -> ApiResult<u64> {
        let db = &self.meeting.core.db;
        let rows = db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT DISTINCT c.meeting_space_id AS id FROM conference_sessions c
                   JOIN meeting_spaces s ON s.id = c.meeting_space_id
                  WHERE c.status = 'LIVE' AND c.origin = 'V2' AND s.status = 'ARCHIVED'
                  LIMIT $1",
                [BATCH.into()],
            ))
            .await?;
        let mut ended = 0;
        for row in rows {
            let space_id: Uuid = row.try_get("", "id")?;
            let txn = sea_orm::TransactionTrait::begin(db).await?;
            // Space first, then its conferences (the archive lock order).
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM meeting_spaces WHERE id = $1 FOR UPDATE",
                [space_id.into()],
            ))
            .await?;
            let ids = self
                .meeting
                .conferences
                .end_live_for_archive(
                    &txn,
                    space_id,
                    &crate::services::meeting_events::AuditActor::system("archive-invariant"),
                    crate::domain::TerminationReason::Admin,
                )
                .await?;
            txn.commit().await?;
            ended += ids.len() as u64;
            self.meeting.conferences.complete_ending(&ids).await;
        }
        if ended > 0 {
            crate::services::ops_metrics::add(&OM.conferences_archive_ended, ended);
            tracing::warn!(ended, "ended LIVE conferences of archived meeting spaces");
        }
        Ok(ended)
    }

    pub async fn tick(&self, now: DateTime<Utc>) -> ApiResult<EmptySweepReport> {
        let mut report = EmptySweepReport {
            archived_repaired: self.repair_archived().await?,
            ..Default::default()
        };
        let db = &self.meeting.core.db;
        let candidates = Candidate::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT c.id, c.rtc_room_name, p.empty_since,
                    GREATEST(COALESCE(c.started_at, c.created_at),
                             COALESCE(p.last_occupied_at, c.created_at),
                             COALESCE((SELECT max(GREATEST(ps.established_at,
                                                  COALESCE(ps.disconnected_at, ps.established_at)))
                                         FROM participant_sessions ps
                                        WHERE ps.conference_id = c.id), c.created_at)) AS baseline
               FROM conference_sessions c
               LEFT JOIN conference_presence p ON p.conference_id = c.id
              WHERE c.status = 'LIVE' AND c.origin = 'V2' AND c.kind = 'INSTANT'
              ORDER BY p.last_checked_at ASC NULLS FIRST, c.id
              LIMIT $1",
            [BATCH.into()],
        ))
        .all(db)
        .await?;
        if candidates.is_empty() {
            return Ok(report);
        }
        let ids: Vec<Uuid> = candidates.iter().map(|c| c.id).collect();
        let mut sessions: HashMap<Uuid, Vec<(String, DateTime<Utc>)>> = HashMap::new();
        for s in ActiveSession::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT conference_id, state, established_at FROM participant_sessions
              WHERE conference_id = ANY($1) AND state IN ('PENDING', 'CONNECTED')
                AND (expires_at IS NULL OR expires_at > $2)",
            [ids.clone().into(), now.into()],
        ))
        .all(db)
        .await?
        {
            sessions
                .entry(s.conference_id)
                .or_default()
                .push((s.state, s.established_at));
        }
        let core = &self.meeting.core;
        let provider_presence =
            core.flags.rtc_provider_control_v2 && core.rtc.capabilities().participant_control;
        for c in candidates {
            report.checked += 1;
            let provider = if provider_presence {
                let room = RtcRoomRef {
                    conference_id: c.id,
                    room_name: c
                        .rtc_room_name
                        .clone()
                        .unwrap_or_else(|| crate::services::rtc_provider::room_name_for(c.id)),
                };
                Some(
                    core.rtc
                        .list_participants(&room)
                        .await
                        .map(|list| list.iter().filter(|p| !p.provider_service).count())
                        .map_err(|_| ()),
                )
            } else {
                None
            };
            let db_states: Vec<(&str, DateTime<Utc>)> = sessions
                .get(&c.id)
                .map(|v| v.iter().map(|(s, at)| (s.as_str(), *at)).collect())
                .unwrap_or_default();
            match presence(provider, &db_states, now) {
                Presence::Unknown => {
                    report.unknown += 1;
                    inc(&OM.conference_presence_checks_unknown);
                    db.execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "INSERT INTO conference_presence (conference_id, last_checked_at) VALUES ($1, now())
                         ON CONFLICT (conference_id) DO UPDATE SET last_checked_at = now()",
                        [c.id.into()],
                    ))
                    .await?;
                }
                Presence::Occupied => {
                    report.occupied += 1;
                    db.execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "INSERT INTO conference_presence (conference_id, empty_since, last_occupied_at, last_checked_at)
                         VALUES ($1, NULL, $2, now())
                         ON CONFLICT (conference_id) DO UPDATE
                           SET empty_since = NULL, last_occupied_at = $2, last_checked_at = now()",
                        [c.id.into(), now.into()],
                    ))
                    .await?;
                }
                Presence::Empty => {
                    report.empty += 1;
                    let since = c.empty_since.unwrap_or_else(|| c.baseline.min(now));
                    db.execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "INSERT INTO conference_presence (conference_id, empty_since, last_checked_at)
                         VALUES ($1, $2, now())
                         ON CONFLICT (conference_id) DO UPDATE
                           SET empty_since = COALESCE(conference_presence.empty_since, $2),
                               last_checked_at = now()",
                        [c.id.into(), since.into()],
                    ))
                    .await?;
                    // Re-read what is recorded (another instance may have set it first).
                    let recorded: Option<DateTime<Utc>> = db
                        .query_one(Statement::from_sql_and_values(
                            DbBackend::Postgres,
                            "SELECT empty_since FROM conference_presence WHERE conference_id = $1",
                            [c.id.into()],
                        ))
                        .await?
                        .map(|r| r.try_get("", "empty_since"))
                        .transpose()?
                        .flatten();
                    let Some(recorded) = recorded else { continue };
                    if now - recorded >= self.timeout {
                        match self.meeting.conferences.end_if_still_empty(c.id, recorded).await {
                            Ok(true) => {
                                report.ended += 1;
                                inc(&OM.conferences_empty_ended);
                                tracing::info!(conference_id = %c.id,
                                    empty_seconds = (now - recorded).num_seconds(),
                                    "empty instant conference ended (EMPTY_TIMEOUT)");
                            }
                            Ok(false) => {}
                            Err(e) => tracing::warn!(conference_id = %c.id,
                                "empty conference end failed: {e}"),
                        }
                    }
                }
            }
        }
        Ok(report)
    }

    pub async fn run(&self, poll_secs: u64, mut shutdown: watch::Receiver<bool>) {
        tracing::info!(
            timeout_secs = self.timeout.num_seconds(),
            "empty conference sweeper started (polling every {poll_secs}s)"
        );
        loop {
            if *shutdown.borrow() {
                break;
            }
            match self.tick(Utc::now()).await {
                Ok(r) if r.ended > 0 || r.archived_repaired > 0 || r.unknown > 0 => {
                    tracing::info!(?r, "empty conference sweep")
                }
                Ok(_) => {}
                Err(e) => tracing::error!("empty conference sweep failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(poll_secs)) => {}
                _ = shutdown.changed() => {}
            }
        }
    }
}

pub struct EmptyConferenceSweeperHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl EmptyConferenceSweeperHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

pub fn spawn_empty_conference_sweeper(
    sweeper: EmptyConferenceSweeper,
    poll_secs: u64,
) -> EmptyConferenceSweeperHandle {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move { sweeper.run(poll_secs.max(5), shutdown_rx).await });
    EmptyConferenceSweeperHandle { shutdown_tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_presence_is_authoritative_with_a_join_grace() {
        let now = Utc::now();
        let old = now - Duration::minutes(30);
        assert_eq!(presence(Some(Ok(1)), &[], now), Presence::Occupied);
        // A stale PENDING/CONNECTED row does not keep an empty provider room alive…
        assert_eq!(presence(Some(Ok(0)), &[("CONNECTED", old), ("PENDING", old)], now), Presence::Empty);
        // …but someone connecting right now does.
        assert_eq!(presence(Some(Ok(0)), &[("PENDING", now - Duration::seconds(30))], now), Presence::Occupied);
        assert_eq!(presence(Some(Err(())), &[], now), Presence::Unknown);
    }

    #[test]
    fn without_provider_control_sessions_decide() {
        let now = Utc::now();
        assert_eq!(presence(None, &[], now), Presence::Empty);
        assert_eq!(presence(None, &[("CONNECTED", now - Duration::hours(1))], now), Presence::Occupied);
    }
}
