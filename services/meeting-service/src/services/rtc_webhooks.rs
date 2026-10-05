//! Provider webhook processing (spec Part E §7–§9).
//!
//! Callbacks are verified by the provider adapter (signature + body hash) before they
//! reach this service. Here they only synchronise *operational* state:
//!
//! - `participant_joined` → the matching ParticipantSession becomes CONNECTED and
//!   `participant.joined` is emitted (admission ≠ joined: admission is business state,
//!   joined means media actually connected);
//! - `participant_left` → the session becomes DISCONNECTED, `participant.left`;
//! - `room_finished` → confirmed room absence (ENDING → ENDED, sessions finalized);
//! - `room_started` → records the provider room id.
//!
//! Callbacks can never create participants or sessions, nor change role, admission,
//! membership or access. A provider identity that does not map to an ACTIVE session of
//! THIS conference (unknown, stale token, removed, banned, replaced) is evicted.
//! Every callback is deduplicated through `rtc_webhook_events` in the same
//! transaction as its state change, so replays never double-apply.

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Set,
    Statement, TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, ConferenceStatus, SessionState},
    error::ApiResult,
    models::{conference_participants, conference_sessions, participant_sessions},
    services::{
        collaboration::{breakouts, media, media::queue_permission_sync},
        meeting_core::{lock_conference, Core},
        meeting_events::{action, audit, emit, event, AuditActor, AuditRecord, OutboxRecord},
        rtc_control::{record_room_terminated, RtcControl},
        rtc_metrics::{inc, RTC_METRICS as M},
        rtc_provider::{ProviderEvent, ProviderEventKind, TrackKind},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookOutcome {
    Applied,
    Duplicate,
    UnknownRoom,
    UnknownIdentityEvicted,
    StaleSessionEvicted,
    Ignored,
    /// Part G: recording evidence without a matching recording (no mutation).
    UnknownRecording,
    /// Part G: recording evidence whose room does not match (cross-conference guard).
    Rejected,
}

impl WebhookOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            WebhookOutcome::Applied => "APPLIED",
            WebhookOutcome::Duplicate => "DUPLICATE",
            WebhookOutcome::UnknownRoom => "UNKNOWN_ROOM",
            WebhookOutcome::UnknownIdentityEvicted => "UNKNOWN_IDENTITY_EVICTED",
            WebhookOutcome::StaleSessionEvicted => "STALE_SESSION_EVICTED",
            WebhookOutcome::Ignored => "IGNORED",
            WebhookOutcome::UnknownRecording => "UNKNOWN_RECORDING",
            WebhookOutcome::Rejected => "REJECTED",
        }
    }
}

pub struct RtcWebhookService {
    core: Core,
    control: RtcControl,
}

fn session_payload(
    p: &conference_participants::Model,
    s: &participant_sessions::Model,
) -> serde_json::Value {
    json!({
        "conferenceId": p.conference_id,
        "participantId": p.id,
        "participantSessionId": s.id,
        "role": p.role,
        "admissionState": p.admission_state,
        "version": p.version,
    })
}

impl RtcWebhookService {
    pub fn new(core: Core) -> Self {
        Self {
            control: RtcControl::new(core.clone()),
            core,
        }
    }

    /// Verify + apply one raw provider callback.
    pub fn verify(
        &self,
        body: &str,
        authorization: Option<&str>,
    ) -> Result<ProviderEvent, crate::services::rtc_provider::RtcError> {
        self.core.rtc.parse_webhook(body, authorization)
    }

    pub async fn handle(&self, e: &ProviderEvent) -> ApiResult<WebhookOutcome> {
        inc(&M.webhooks_received);
        if let (
            ProviderEventKind::RecordingStarted
            | ProviderEventKind::RecordingUpdated
            | ProviderEventKind::RecordingEnded,
            Some(recording),
        ) = (e.kind, &e.recording)
        {
            return self.recording_event(e, recording).await;
        }
        let txn = self.core.db.begin().await?;
        let inserted = txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO rtc_webhook_events (event_id, provider, event_type, room_name, outcome)
                 VALUES ($1, $2, $3, $4, 'RECEIVED') ON CONFLICT (event_id) DO NOTHING",
                [
                    e.id.clone().into(),
                    self.core.rtc.name().into(),
                    e.event_type.clone().into(),
                    e.room_name.clone().into(),
                ],
            ))
            .await?
            .rows_affected();
        if inserted == 0 {
            txn.commit().await?;
            inc(&M.webhooks_duplicate);
            return Ok(WebhookOutcome::Duplicate);
        }
        // Map provider room → ConferenceSession (opaque mapping, never client input).
        // Part F: a breakout room maps to its parent conference.
        let mut breakout = None;
        let mut backstage = false;
        let conference = match &e.room_name {
            Some(name) => {
                match conference_sessions::Entity::find()
                    .filter(conference_sessions::Column::RtcRoomName.eq(name.clone()))
                    .one(&txn)
                    .await?
                {
                    Some(c) => Some(c),
                    // Part H: the backstage room maps to its own conference.
                    None => match conference_sessions::Entity::find()
                        .filter(conference_sessions::Column::BackstageRoomName.eq(name.clone()))
                        .one(&txn)
                        .await?
                    {
                        Some(c) => {
                            backstage = true;
                            Some(c)
                        }
                        None => match breakouts::breakout_by_room(&txn, name).await? {
                            Some(room) => {
                                let c = conference_sessions::Entity::find_by_id(room.conference_id)
                                    .one(&txn)
                                    .await?;
                                breakout = Some(room);
                                c
                            }
                            None => None,
                        },
                    },
                }
            }
            None => None,
        };
        let Some(conference) = conference else {
            self.finish(&txn, e, None, WebhookOutcome::UnknownRoom)
                .await?;
            txn.commit().await?;
            inc(&M.webhooks_unknown_room);
            tracing::warn!(event_id = %e.id, event_type = %e.event_type, "provider webhook for an unknown room ignored");
            return Ok(WebhookOutcome::UnknownRoom);
        };
        // Lock order: conference first (same as every business transaction).
        let conference = lock_conference(&txn, conference.id).await?;
        let outcome = match (e.kind, &breakout) {
            // Breakout room lifecycle callbacks never touch the parent conference.
            (
                ProviderEventKind::RoomStarted
                | ProviderEventKind::RoomFinished
                | ProviderEventKind::Other,
                Some(_),
            ) => WebhookOutcome::Ignored,
            // Backstage room lifecycle never touches the conference either.
            (
                ProviderEventKind::RoomStarted
                | ProviderEventKind::RoomFinished
                | ProviderEventKind::Other,
                None,
            ) if backstage => WebhookOutcome::Ignored,
            (ProviderEventKind::RoomStarted, None) => {
                if conference.rtc_room_id.is_none() {
                    if let Some(sid) = &e.room_id {
                        let mut am: conference_sessions::ActiveModel = conference.clone().into();
                        am.rtc_room_id = Set(Some(sid.clone()));
                        am.updated_at = Set(Utc::now());
                        am.update(&txn).await?;
                    }
                }
                WebhookOutcome::Applied
            }
            (ProviderEventKind::RoomFinished, None) => {
                self.room_finished(&txn, conference.clone(), e).await?
            }
            (ProviderEventKind::ParticipantJoined, _) => {
                self.participant_joined(&txn, &conference, e).await?
            }
            (ProviderEventKind::ParticipantLeft, _) => {
                self.participant_left(&txn, &conference, e).await?
            }
            (ProviderEventKind::TrackPublished | ProviderEventKind::TrackUnpublished, _) => {
                self.track_event(&txn, &conference, e).await?
            }
            (ProviderEventKind::Other, None) => WebhookOutcome::Ignored,
            (
                ProviderEventKind::RecordingStarted
                | ProviderEventKind::RecordingUpdated
                | ProviderEventKind::RecordingEnded,
                _,
            ) => WebhookOutcome::Ignored,
        };
        self.finish(&txn, e, Some(conference.id), outcome).await?;
        txn.commit().await?;
        if matches!(
            outcome,
            WebhookOutcome::UnknownIdentityEvicted
                | WebhookOutcome::StaleSessionEvicted
                | WebhookOutcome::Applied
        ) {
            // Execute queued evictions / permission syncs immediately (relay retries).
            self.control.dispatch_for_conference(conference.id).await;
        }
        Ok(outcome)
    }

    /// Part G: recording (egress) evidence. Application is idempotent (forward-only
    /// transitions keyed by the internal provider reference and checked against the
    /// conference's room), so the dedupe row is written after a successful apply: a
    /// transient failure makes the provider retry instead of losing the evidence.
    async fn recording_event(
        &self,
        e: &ProviderEvent,
        recording: &crate::services::artifacts::recorder::ProviderRecording,
    ) -> ApiResult<WebhookOutcome> {
        use crate::services::artifacts::recording::{EvidenceOutcome, RecordingService};
        let seen = self
            .core
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM rtc_webhook_events WHERE event_id = $1",
                [e.id.clone().into()],
            ))
            .await?;
        if seen.is_some() {
            inc(&M.webhooks_duplicate);
            return Ok(WebhookOutcome::Duplicate);
        }
        let applied =
            RecordingService::apply_evidence(&self.core, recording, e.room_name.as_deref()).await?;
        let outcome = match applied {
            EvidenceOutcome::Applied(_) => WebhookOutcome::Applied,
            EvidenceOutcome::Ignored => WebhookOutcome::Ignored,
            EvidenceOutcome::Unknown => WebhookOutcome::UnknownRecording,
            EvidenceOutcome::Rejected => WebhookOutcome::Rejected,
        };
        let inserted = self
            .core
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO rtc_webhook_events (event_id, provider, event_type, room_name, outcome)
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (event_id) DO NOTHING",
                [
                    e.id.clone().into(),
                    self.core.rtc.name().into(),
                    e.event_type.clone().into(),
                    e.room_name.clone().into(),
                    outcome.as_str().into(),
                ],
            ))
            .await?
            .rows_affected();
        if inserted == 0 {
            inc(&M.webhooks_duplicate);
            return Ok(WebhookOutcome::Duplicate);
        }
        Ok(outcome)
    }

    async fn finish<C: ConnectionTrait>(
        &self,
        conn: &C,
        e: &ProviderEvent,
        conference_id: Option<Uuid>,
        outcome: WebhookOutcome,
    ) -> ApiResult<()> {
        conn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE rtc_webhook_events SET outcome = $2, conference_id = $3 WHERE event_id = $1",
            [
                e.id.clone().into(),
                outcome.as_str().into(),
                conference_id.into(),
            ],
        ))
        .await?;
        Ok(())
    }

    async fn room_finished(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        conference: conference_sessions::Model,
        e: &ProviderEvent,
    ) -> ApiResult<WebhookOutcome> {
        // A callback about an older incarnation of the room must not end the current one.
        if let (Some(known), Some(reported)) = (&conference.rtc_room_id, &e.room_id) {
            if known != reported {
                return Ok(WebhookOutcome::Ignored);
            }
        }
        match conference.status {
            ConferenceStatus::Ending | ConferenceStatus::Cancelled | ConferenceStatus::Failed => {
                record_room_terminated(
                    txn,
                    conference,
                    &AuditActor::system("rtc-webhook"),
                    "PROVIDER_WEBHOOK",
                )
                .await?;
                Ok(WebhookOutcome::Applied)
            }
            ConferenceStatus::Live | ConferenceStatus::Waiting => {
                // Unexpected provider-side room loss: business state is unchanged (still
                // LIVE); media sessions are gone. The next join or the reconciler
                // re-creates the room.
                let now = Utc::now();
                let sessions = participant_sessions::Entity::find()
                    .filter(participant_sessions::Column::ConferenceId.eq(conference.id))
                    .filter(participant_sessions::Column::State.eq(SessionState::Connected))
                    .all(txn)
                    .await?;
                for s in &sessions {
                    let mut am: participant_sessions::ActiveModel = s.clone().into();
                    am.state = Set(SessionState::Disconnected);
                    am.disconnected_at = Set(Some(now));
                    am.end_reason = Set(Some("PROVIDER_ROOM_FINISHED".into()));
                    am.updated_at = Set(now);
                    am.update(txn).await?;
                }
                let mut am: conference_sessions::ActiveModel = conference.clone().into();
                am.rtc_room_ready_at = Set(None);
                am.rtc_room_id = Set(None);
                am.updated_at = Set(now);
                am.update(txn).await?;
                inc(&M.drift_detected);
                tracing::warn!(conference_id = %conference.id, "provider room finished while conference is {}", conference.status);
                Ok(WebhookOutcome::Applied)
            }
            _ => Ok(WebhookOutcome::Ignored),
        }
    }

    async fn find_session(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        conference: &conference_sessions::Model,
        identity: &str,
    ) -> ApiResult<Option<participant_sessions::Model>> {
        // Scoped to THIS conference: an identity can never touch another conference.
        Ok(participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceId.eq(conference.id))
            .filter(participant_sessions::Column::MediaIdentity.eq(identity))
            .one(txn)
            .await?)
    }

    async fn evict(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        conference: &conference_sessions::Model,
        session: Option<&participant_sessions::Model>,
        identity: &str,
        reason: &str,
    ) -> ApiResult<()> {
        self.evict_from(txn, conference, session, identity, reason, None)
            .await
    }

    /// Evict a provider identity; `room` restricts the eviction to one room (Part F
    /// context enforcement), otherwise every room of the conference.
    async fn evict_from(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        conference: &conference_sessions::Model,
        session: Option<&participant_sessions::Model>,
        identity: &str,
        reason: &str,
        room: Option<&str>,
    ) -> ApiResult<()> {
        emit(
            txn,
            OutboxRecord {
                aggregate_type: "participant_session",
                aggregate_id: session.map(|s| s.id).unwrap_or(conference.id),
                event_type: event::RTC_PARTICIPANT_REMOVE_REQUESTED,
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference.id),
                payload: json!({
                    "conferenceId": conference.id,
                    "participantSessionId": session.map(|s| s.id),
                    "mediaIdentity": identity,
                    "reason": reason,
                    "roomName": room,
                }),
            },
        )
        .await?;
        audit(
            txn,
            &AuditActor::system("rtc-webhook"),
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference.id),
                action: action::PROVIDER_PARTICIPANT_EVICTED,
                target_type: Some("PARTICIPANT_SESSION"),
                target_ref: session.map(|s| s.id.to_string()),
                metadata: json!({ "reason": reason }),
            },
        )
        .await?;
        inc(&M.stale_sessions_evicted);
        Ok(())
    }

    async fn participant_joined(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        conference: &conference_sessions::Model,
        e: &ProviderEvent,
    ) -> ApiResult<WebhookOutcome> {
        let Some(identity) = e.media_identity.as_deref().filter(|i| !i.is_empty()) else {
            return Ok(WebhookOutcome::Ignored);
        };
        // The recorder (LiveKit Egress) is not a meeting participant: never evicted by
        // the session rules; unowned captures are escalated by recording reconciliation.
        if e.provider_service_participant {
            return Ok(WebhookOutcome::Ignored);
        }
        let session = self.find_session(txn, conference, identity).await?;
        let Some(session) = session else {
            if conference.status == ConferenceStatus::Live {
                self.evict(txn, conference, None, identity, "UNKNOWN_PROVIDER_IDENTITY")
                    .await?;
            }
            return Ok(WebhookOutcome::UnknownIdentityEvicted);
        };
        let participant =
            conference_participants::Entity::find_by_id(session.conference_participant_id)
                .filter(conference_participants::Column::ConferenceId.eq(conference.id))
                .one(txn)
                .await?;
        let allowed = conference.status == ConferenceStatus::Live
            && session.state.is_active()
            && participant
                .as_ref()
                .map(|p| p.admission_state == AdmissionState::Admitted)
                .unwrap_or(false);
        if !allowed {
            // Stale credential / removed / banned / replaced / ended: business
            // authorization wins over any still-valid provider token.
            self.evict(txn, conference, Some(&session), identity, "STALE_SESSION")
                .await?;
            return Ok(WebhookOutcome::StaleSessionEvicted);
        }
        let participant = participant.expect("checked above");
        // Part F: the provider room must be the participant's current media context
        // (main or assigned/visited breakout). Anything else is a stale credential.
        let expected = breakouts::media_context(txn, conference, &participant)
            .await?
            .room_name(conference);
        if e.room_name.as_deref() != Some(expected.as_str()) {
            self.evict_from(
                txn,
                conference,
                Some(&session),
                identity,
                "STALE_MEDIA_CONTEXT",
                e.room_name.as_deref(),
            )
            .await?;
            return Ok(WebhookOutcome::StaleSessionEvicted);
        }
        if session.state == SessionState::Connected {
            // Context switch (reroute) or duplicate: grants are re-asserted for the room.
            if self.control.participant_control() {
                queue_permission_sync(txn, conference, participant.id).await?;
            }
            return Ok(WebhookOutcome::Applied);
        }
        let now = Utc::now();
        let mut am: participant_sessions::ActiveModel = session.clone().into();
        am.state = Set(SessionState::Connected);
        am.connected_at = Set(Some(
            e.occurred_at.unwrap_or(now).max(session.established_at),
        ));
        am.updated_at = Set(now);
        let session = am.update(txn).await?;
        // Part I: join latency to usable media + reconnect accounting (once).
        crate::services::quality::service::record_media_connected(
            txn,
            &self.core,
            session.id,
            conference.id,
        )
        .await?;
        emit(
            txn,
            OutboxRecord {
                aggregate_type: "participant_session",
                aggregate_id: session.id,
                event_type: event::PARTICIPANT_JOINED,
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference.id),
                payload: session_payload(&participant, &session),
            },
        )
        .await?;
        // Re-assert current grants on connect (a role change may have happened after the
        // credential was minted).
        if self.control.participant_control() {
            emit(
                txn,
                OutboxRecord {
                    aggregate_type: "conference_participant",
                    aggregate_id: participant.id,
                    event_type: event::RTC_PERMISSIONS_UPDATE_REQUESTED,
                    meeting_space_id: Some(conference.meeting_space_id),
                    conference_id: Some(conference.id),
                    payload: json!({ "conferenceId": conference.id, "participantId": participant.id }),
                },
            )
            .await?;
        }
        Ok(WebhookOutcome::Applied)
    }

    async fn participant_left(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        conference: &conference_sessions::Model,
        e: &ProviderEvent,
    ) -> ApiResult<WebhookOutcome> {
        let Some(identity) = e.media_identity.as_deref().filter(|i| !i.is_empty()) else {
            return Ok(WebhookOutcome::Ignored);
        };
        let Some(session) = self.find_session(txn, conference, identity).await? else {
            return Ok(WebhookOutcome::Ignored);
        };
        if !session.state.is_active() {
            return Ok(WebhookOutcome::Ignored);
        }
        let participant =
            conference_participants::Entity::find_by_id(session.conference_participant_id)
                .one(txn)
                .await?;
        // Part F: leaving a room that is no longer the current context (rerouted into a
        // breakout or back to main) does not disconnect the session.
        if let Some(p) = &participant {
            let expected = breakouts::media_context(txn, conference, p)
                .await?
                .room_name(conference);
            if e.room_name
                .as_deref()
                .map(|r| r != expected)
                .unwrap_or(false)
            {
                return Ok(WebhookOutcome::Ignored);
            }
        }
        let now = Utc::now();
        let mut am: participant_sessions::ActiveModel = session.clone().into();
        am.state = Set(SessionState::Disconnected);
        let floor = session.connected_at.unwrap_or(session.established_at);
        am.disconnected_at = Set(Some(e.occurred_at.unwrap_or(now).max(floor)));
        am.end_reason = Set(Some("PROVIDER_LEFT".into()));
        am.updated_at = Set(now);
        let session = am.update(txn).await?;
        if let Some(p) = participant {
            let mut payload = session_payload(&p, &session);
            payload["sessionIds"] = json!([session.id]);
            emit(
                txn,
                OutboxRecord {
                    aggregate_type: "conference_participant",
                    aggregate_id: p.id,
                    event_type: event::PARTICIPANT_LEFT,
                    meeting_space_id: Some(conference.meeting_space_id),
                    conference_id: Some(conference.id),
                    payload,
                },
            )
            .await?;
        }
        Ok(WebhookOutcome::Applied)
    }

    /// Part F: screen-share track evidence (verified provider callback).
    async fn track_event(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        conference: &conference_sessions::Model,
        e: &ProviderEvent,
    ) -> ApiResult<WebhookOutcome> {
        if e.track_kind != Some(TrackKind::ScreenShare)
            || conference.status != ConferenceStatus::Live
        {
            return Ok(WebhookOutcome::Ignored);
        }
        let Some(identity) = e.media_identity.as_deref().filter(|i| !i.is_empty()) else {
            return Ok(WebhookOutcome::Ignored);
        };
        let Some(session) = self.find_session(txn, conference, identity).await? else {
            // Unknown identity in this conference: never creates state.
            return Ok(WebhookOutcome::Ignored);
        };
        let Some(participant) =
            conference_participants::Entity::find_by_id(session.conference_participant_id)
                .filter(conference_participants::Column::ConferenceId.eq(conference.id))
                .one(txn)
                .await?
        else {
            return Ok(WebhookOutcome::Ignored);
        };
        let published = e.kind == ProviderEventKind::TrackPublished;
        if published
            && (participant.admission_state != AdmissionState::Admitted
                || !session.state.is_active())
        {
            return Ok(WebhookOutcome::Ignored);
        }
        let result = media::on_screen_share_track(
            txn,
            conference,
            &participant,
            &session,
            published,
            e.track_id.as_deref(),
        )
        .await?;
        Ok(match result {
            "APPLIED" | "REVOKED" => WebhookOutcome::Applied,
            _ => WebhookOutcome::Ignored,
        })
    }
}
