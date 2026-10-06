//! Meeting-domain audit + transactional outbox writers (spec §B12, §B13, §3.5).
//!
//! Both are written on the caller's transaction so business state, audit and the
//! delivery event commit atomically. Events are delivery, never authority.
//! Never pass raw passcodes, invitation tokens or credentials in metadata/payloads.
//!
//! Part E: `rtc.*` rows are internal provider commands (`PROVIDER_COMMAND` channel) and
//! are never delivered to clients. Client events of a conference get a gap-free,
//! commit-ordered `conference_seq`, allocated by incrementing
//! `conference_sessions.event_seq` — that row update holds the conference row lock
//! until commit, so sequence order equals commit order for each conference. Callers
//! must therefore follow the lock order conference → participant → invitation.

use chrono::Utc;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DbBackend, Set, Statement};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    domain::{OutboxChannel, OutboxStatus},
    error::ApiResult,
    middleware::request_id::current_request_id,
    models::{conference_audit_events, outbox_events},
    services::actor::{ActorContext, ActorKind},
};

/// Event types compatible with the Part E realtime contract.
pub mod event {
    pub const SPACE_CREATED: &str = "meeting_space.created";
    pub const SPACE_UPDATED: &str = "meeting_space.updated";
    pub const SPACE_ARCHIVED: &str = "meeting_space.archived";
    pub const SPACE_OWNER_TRANSFERRED: &str = "meeting_space.owner_transferred";
    pub const CONFERENCE_CREATED: &str = "conference.created";
    pub const CONFERENCE_PREPARING: &str = "conference.preparing";
    /// PREPARING → WAITING once the provider confirmed the room.
    pub const CONFERENCE_READY: &str = "conference.ready";
    pub const CONFERENCE_STARTED: &str = "conference.started";
    pub const CONFERENCE_LOCKED: &str = "conference.locked";
    pub const CONFERENCE_UNLOCKED: &str = "conference.unlocked";
    pub const CONFERENCE_ENDING: &str = "conference.ending";
    /// ENDING → ENDED once the provider confirmed termination / room absence.
    pub const CONFERENCE_ENDED: &str = "conference.ended";
    pub const CONFERENCE_CANCELLED: &str = "conference.cancelled";
    pub const CONFERENCE_RESCHEDULED: &str = "conference.rescheduled";
    pub const CONFERENCE_FAILED: &str = "conference.failed";
    pub const PARTICIPANT_REQUESTED_JOIN: &str = "participant.requested_join";
    pub const PARTICIPANT_ADMITTED: &str = "participant.admitted";
    pub const PARTICIPANT_DENIED: &str = "participant.denied";
    pub const PARTICIPANT_RETURNED_TO_WAITING: &str = "participant.returned_to_waiting";
    pub const PARTICIPANT_ROLE_CHANGED: &str = "participant.role_changed";
    pub const PARTICIPANT_REMOVED: &str = "participant.removed";
    pub const PARTICIPANT_BANNED: &str = "participant.banned";
    pub const PARTICIPANT_LEFT: &str = "participant.left";
    /// The RTC/media session actually connected (provider-confirmed) — not admission.
    pub const PARTICIPANT_JOINED: &str = "participant.joined";
    pub const PARTICIPANT_MEDIA_PERMISSION_CHANGED: &str = "participant.media_permission_changed";
    /// Provider enforcement intents consumed by the Part E control plane.
    pub const RTC_ROOM_TERMINATE_REQUESTED: &str = "rtc.room_terminate_requested";
    pub const RTC_PARTICIPANT_REMOVE_REQUESTED: &str = "rtc.participant_remove_requested";
    pub const RTC_PERMISSIONS_UPDATE_REQUESTED: &str = "rtc.permissions_update_requested";
    pub const RTC_ROOM_PREPARE_REQUESTED: &str = "rtc.room_prepare_requested";
    /// Part F provider commands.
    pub const RTC_SCREENSHARE_STOP_REQUESTED: &str = "rtc.screenshare_stop_requested";
    pub const RTC_PARTICIPANT_REROUTE_REQUESTED: &str = "rtc.participant_reroute_requested";
    pub const RTC_BREAKOUT_PREPARE_REQUESTED: &str = "rtc.breakout_prepare_requested";
    pub const RTC_BREAKOUT_TERMINATE_REQUESTED: &str = "rtc.breakout_terminate_requested";

    // Part F — media
    pub const PARTICIPANT_AUDIO_LOCKED: &str = "participant.audio_locked";
    pub const PARTICIPANT_AUDIO_UNLOCKED: &str = "participant.audio_unlocked";
    pub const PARTICIPANT_VIDEO_LOCKED: &str = "participant.video_locked";
    pub const PARTICIPANT_VIDEO_UNLOCKED: &str = "participant.video_unlocked";
    pub const PARTICIPANT_SCREEN_SHARE_LOCKED: &str = "participant.screen_share_locked";
    pub const PARTICIPANT_SCREEN_SHARE_UNLOCKED: &str = "participant.screen_share_unlocked";
    pub const PARTICIPANT_MUTED: &str = "participant.muted";
    pub const SCREENSHARE_STARTED: &str = "screenshare.started";
    pub const SCREENSHARE_ENDED: &str = "screenshare.ended";
    pub const PARTICIPANT_SPOTLIGHTED: &str = "participant.spotlighted";
    pub const PARTICIPANT_SPOTLIGHT_REMOVED: &str = "participant.spotlight_removed";
    pub const COLLABORATION_POLICY_CHANGED: &str = "collaboration.policy_changed";
    // Part F — chat
    pub const CHAT_MESSAGE_CREATED: &str = "chat.message_created";
    pub const CHAT_MESSAGE_DELETED: &str = "chat.message_deleted";
    pub const CHAT_PARTICIPANT_RESTRICTED: &str = "chat.participant_restricted";
    pub const CHAT_PARTICIPANT_UNRESTRICTED: &str = "chat.participant_unrestricted";
    pub const CHAT_POLICY_CHANGED: &str = "chat.policy_changed";
    // Part F — reactions / hand (reactions are transient: never written to the outbox)
    pub const REACTION_SENT: &str = "reaction.sent";
    pub const HAND_RAISED: &str = "hand.raised";
    pub const HAND_LOWERED: &str = "hand.lowered";
    // Part F — breakouts
    pub const BREAKOUT_CREATED: &str = "breakout.created";
    pub const BREAKOUT_OPENED: &str = "breakout.opened";
    pub const BREAKOUT_ASSIGNMENT_CHANGED: &str = "breakout.assignment_changed";
    pub const BREAKOUT_PARTICIPANT_MOVED: &str = "breakout.participant_moved";
    pub const BREAKOUT_ANNOUNCEMENT: &str = "breakout.announcement";
    pub const BREAKOUT_CLOSING: &str = "breakout.closing";
    pub const BREAKOUT_CLOSED: &str = "breakout.closed";
    // Part F — polls
    pub const POLL_CREATED: &str = "poll.created";
    pub const POLL_STARTED: &str = "poll.started";
    pub const POLL_RESPONSE_RECORDED: &str = "poll.response_recorded";
    pub const POLL_CLOSED: &str = "poll.closed";
    pub const POLL_ARCHIVED: &str = "poll.archived";
    pub const POLL_RESULTS_UPDATED: &str = "poll.results_updated";
    // Part F — Q&A
    pub const QNA_QUESTION_CREATED: &str = "qna.question_created";
    pub const QNA_QUESTION_UPVOTED: &str = "qna.question_upvoted";
    pub const QNA_QUESTION_ANSWERED: &str = "qna.question_answered";
    pub const QNA_QUESTION_DISMISSED: &str = "qna.question_dismissed";
    pub const QNA_QUESTION_ARCHIVED: &str = "qna.question_archived";
    pub const QNA_QUESTION_PINNED: &str = "qna.question_pinned";
    pub const QNA_QUESTION_UNPINNED: &str = "qna.question_unpinned";

    // Part G — recording / artefacts / consent / transcription
    pub const RTC_RECORDING_START_REQUESTED: &str = "rtc.recording_start_requested";
    pub const RTC_RECORDING_STOP_REQUESTED: &str = "rtc.recording_stop_requested";
    pub const RECORDING_REQUESTED: &str = "recording.requested";
    pub const RECORDING_STARTED: &str = "recording.started";
    pub const RECORDING_STOPPING: &str = "recording.stopping";
    pub const RECORDING_PROCESSING: &str = "recording.processing";
    pub const RECORDING_READY: &str = "recording.ready";
    pub const RECORDING_FAILED: &str = "recording.failed";
    pub const RECORDING_DELETED: &str = "recording.deleted";
    pub const ARTIFACT_READY: &str = "artifact.ready";
    pub const ARTIFACT_DELETED: &str = "artifact.deleted";
    pub const ARTIFACT_FAILED: &str = "artifact.failed";
    pub const CONSENT_REQUIRED: &str = "consent.required";
    pub const CONSENT_RECORDED: &str = "consent.recorded";
    pub const CONSENT_WITHDRAWN: &str = "consent.withdrawn";
    pub const TRANSCRIPTION_STARTED: &str = "transcription.started";
    pub const TRANSCRIPTION_SEGMENT_PARTIAL: &str = "transcription.segment_partial";
    pub const TRANSCRIPTION_SEGMENT_FINAL: &str = "transcription.segment_final";
    pub const TRANSCRIPTION_PROCESSING: &str = "transcription.processing";
    pub const TRANSCRIPTION_READY: &str = "transcription.ready";
    pub const TRANSCRIPTION_FAILED: &str = "transcription.failed";
    pub const CAPTION_SEGMENT: &str = "caption.segment";
    // Part H — profiles / webinar / backstage
    pub const RTC_BACKSTAGE_PREPARE_REQUESTED: &str = "rtc.backstage_prepare_requested";
    pub const REGISTRATION_CREATED: &str = "registration.created";
    pub const PARTICIPANT_PROMOTED: &str = "participant.promoted";
    pub const PARTICIPANT_DEMOTED: &str = "participant.demoted";
    pub const BACKSTAGE_PARTICIPANT_JOINED: &str = "backstage.participant_joined";
    pub const BACKSTAGE_PARTICIPANT_LEFT: &str = "backstage.participant_left";
    pub const PROFILE_POLICY_CHANGED: &str = "profile.policy_changed";

    /// Internal provider-control command (never delivered to clients).
    pub fn is_provider_command(event_type: &str) -> bool {
        event_type.starts_with("rtc.")
    }
}

/// Audit actions.
pub mod action {
    pub const SPACE_CREATE: &str = "SPACE_CREATE";
    pub const SPACE_UPDATE: &str = "SPACE_UPDATE";
    pub const SPACE_POLICY_UPDATE: &str = "SPACE_POLICY_UPDATE";
    pub const SPACE_ARCHIVE: &str = "SPACE_ARCHIVE";
    pub const SPACE_DUPLICATE: &str = "SPACE_DUPLICATE";
    pub const SPACE_OWNER_TRANSFER: &str = "SPACE_OWNER_TRANSFER";
    pub const RECURRENCE_SET: &str = "RECURRENCE_SET";
    pub const RECURRENCE_END: &str = "RECURRENCE_END";
    pub const RECURRENCE_EXCEPTION_SET: &str = "RECURRENCE_EXCEPTION_SET";
    pub const MEMBER_ADD: &str = "MEMBER_ADD";
    pub const MEMBER_UPDATE: &str = "MEMBER_UPDATE";
    pub const MEMBER_REVOKE: &str = "MEMBER_REVOKE";
    pub const INVITATION_CREATE: &str = "INVITATION_CREATE";
    pub const INVITATION_REVOKE: &str = "INVITATION_REVOKE";
    pub const INVITATION_USE: &str = "INVITATION_USE";
    pub const CONFERENCE_CREATE: &str = "CONFERENCE_CREATE";
    pub const CONFERENCE_PREPARE: &str = "CONFERENCE_PREPARE";
    pub const CONFERENCE_READY: &str = "CONFERENCE_READY";
    pub const CONFERENCE_END_COMPLETE: &str = "CONFERENCE_END_COMPLETE";
    pub const CONFERENCE_START: &str = "CONFERENCE_START";
    pub const CONFERENCE_LOCK: &str = "CONFERENCE_LOCK";
    pub const CONFERENCE_UNLOCK: &str = "CONFERENCE_UNLOCK";
    pub const CONFERENCE_END_REQUEST: &str = "CONFERENCE_END_REQUEST";
    pub const CONFERENCE_CANCEL: &str = "CONFERENCE_CANCEL";
    pub const CONFERENCE_RESCHEDULE: &str = "CONFERENCE_RESCHEDULE";
    pub const CONFERENCE_FAIL: &str = "CONFERENCE_FAIL";
    pub const JOIN_REQUEST: &str = "JOIN_REQUEST";
    pub const JOIN_DENIED: &str = "JOIN_DENIED";
    pub const PASSCODE_FAILED: &str = "PASSCODE_FAILED";
    pub const ADMIT: &str = "ADMIT";
    pub const DENY: &str = "DENY";
    pub const RETURN_TO_WAITING: &str = "RETURN_TO_WAITING";
    pub const ROLE_CHANGE: &str = "ROLE_CHANGE";
    pub const REMOVE: &str = "REMOVE";
    pub const BAN: &str = "BAN";
    pub const MEDIA_MUTE: &str = "MEDIA_MUTE";
    pub const PROVIDER_PARTICIPANT_EVICTED: &str = "PROVIDER_PARTICIPANT_EVICTED";
    // Part F
    pub const MEDIA_AUDIO_LOCK: &str = "MEDIA_AUDIO_LOCK";
    pub const MEDIA_AUDIO_UNLOCK: &str = "MEDIA_AUDIO_UNLOCK";
    pub const MEDIA_VIDEO_LOCK: &str = "MEDIA_VIDEO_LOCK";
    pub const MEDIA_VIDEO_UNLOCK: &str = "MEDIA_VIDEO_UNLOCK";
    pub const MEDIA_SCREEN_SHARE_LOCK: &str = "MEDIA_SCREEN_SHARE_LOCK";
    pub const MEDIA_SCREEN_SHARE_UNLOCK: &str = "MEDIA_SCREEN_SHARE_UNLOCK";
    pub const SCREEN_SHARE_STOP: &str = "SCREEN_SHARE_STOP";
    pub const SPOTLIGHT_SET: &str = "SPOTLIGHT_SET";
    pub const SPOTLIGHT_REMOVE: &str = "SPOTLIGHT_REMOVE";
    pub const COLLABORATION_POLICY_UPDATE: &str = "COLLABORATION_POLICY_UPDATE";
    pub const CHAT_MESSAGE_DELETE: &str = "CHAT_MESSAGE_DELETE";
    pub const CHAT_PARTICIPANT_RESTRICT: &str = "CHAT_PARTICIPANT_RESTRICT";
    pub const CHAT_PARTICIPANT_UNRESTRICT: &str = "CHAT_PARTICIPANT_UNRESTRICT";
    pub const CHAT_ANNOUNCEMENT: &str = "CHAT_ANNOUNCEMENT";
    pub const HAND_LOWER_OTHER: &str = "HAND_LOWER_OTHER";
    pub const BREAKOUT_CREATE: &str = "BREAKOUT_CREATE";
    pub const BREAKOUT_ASSIGN: &str = "BREAKOUT_ASSIGN";
    pub const BREAKOUT_MOVE: &str = "BREAKOUT_MOVE";
    pub const BREAKOUT_OPEN: &str = "BREAKOUT_OPEN";
    pub const BREAKOUT_CLOSE: &str = "BREAKOUT_CLOSE";
    pub const BREAKOUT_CLOSE_COMPLETE: &str = "BREAKOUT_CLOSE_COMPLETE";
    pub const BREAKOUT_VISIT: &str = "BREAKOUT_VISIT";
    pub const BREAKOUT_ANNOUNCE: &str = "BREAKOUT_ANNOUNCE";
    pub const POLL_CREATE: &str = "POLL_CREATE";
    pub const POLL_OPEN: &str = "POLL_OPEN";
    pub const POLL_CLOSE: &str = "POLL_CLOSE";
    pub const POLL_ARCHIVE: &str = "POLL_ARCHIVE";
    pub const QNA_ANSWER: &str = "QNA_ANSWER";
    pub const QNA_DISMISS: &str = "QNA_DISMISS";
    pub const QNA_PIN: &str = "QNA_PIN";
    pub const QNA_UNPIN: &str = "QNA_UNPIN";
    pub const QNA_ARCHIVE: &str = "QNA_ARCHIVE";
    pub const CONSENT_RECORDED: &str = "CONSENT_RECORDED";
    // Parts G–I
    pub const RECORDING_START: &str = "RECORDING_START";
    pub const RECORDING_STOP: &str = "RECORDING_STOP";
    pub const RECORDING_PROVIDER_STARTED: &str = "RECORDING_PROVIDER_STARTED";
    pub const RECORDING_FINALIZED: &str = "RECORDING_FINALIZED";
    pub const RECORDING_FAILED: &str = "RECORDING_FAILED";
    pub const ARTIFACT_ACCESS: &str = "ARTIFACT_ACCESS";
    pub const ARTIFACT_DELETE_REQUEST: &str = "ARTIFACT_DELETE_REQUEST";
    pub const ARTIFACT_DELETED: &str = "ARTIFACT_DELETED";
    pub const ARTIFACT_GENERATE: &str = "ARTIFACT_GENERATE";
    pub const CONSENT_DECLINED: &str = "CONSENT_DECLINED";
    pub const CONSENT_WITHDRAWN: &str = "CONSENT_WITHDRAWN";
    pub const TRANSCRIPTION_START: &str = "TRANSCRIPTION_START";
    pub const TRANSCRIPTION_STOP: &str = "TRANSCRIPTION_STOP";
    pub const TRANSLATION_REQUEST: &str = "TRANSLATION_REQUEST";
    pub const INTELLIGENCE_REQUEST: &str = "INTELLIGENCE_REQUEST";
    pub const PROFILE_POLICY_UPDATE: &str = "PROFILE_POLICY_UPDATE";
    pub const REGISTRATION_CREATE: &str = "REGISTRATION_CREATE";
    pub const REGISTRATION_CANCEL: &str = "REGISTRATION_CANCEL";
    pub const PARTICIPANT_PROMOTE: &str = "PARTICIPANT_PROMOTE";
    pub const PARTICIPANT_DEMOTE: &str = "PARTICIPANT_DEMOTE";
    pub const BACKSTAGE_ENTER: &str = "BACKSTAGE_ENTER";
    pub const BACKSTAGE_LEAVE: &str = "BACKSTAGE_LEAVE";
    /// Enhanced (regulated-profile) semantic audit.
    pub const PARTICIPANT_JOIN: &str = "PARTICIPANT_JOIN";
    pub const EXTERNAL_AUTHORIZATION: &str = "EXTERNAL_AUTHORIZATION";
    pub const OPERATIONS_ACCESS: &str = "OPERATIONS_ACCESS";
}

/// Server-side realtime audience of a client event (Part F §57). Stored in
/// `outbox_events.audience`, evaluated per subscriber, never sent to clients.
/// `None` (no audience) = every authorized conference subscriber.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Audience {
    /// Listed participants, plus subscribers holding `moderator_permission` (if any).
    Participants {
        ids: Vec<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        moderator_permission: Option<String>,
    },
    /// Only subscribers whose role holds this permission (e.g. `poll.view_results`).
    Permission { permission: String },
    /// Members / visitors of one breakout room, plus `breakout.enter_any` holders.
    Breakout { breakout_id: Uuid },
    /// Everyone not inside a breakout (unless policy shows main chat in breakouts),
    /// plus `chat.moderate` holders.
    MainRoom,
    /// Everyone inside any open breakout, plus `breakout.enter_any` holders.
    Breakouts,
}

/// Who performed an audited action.
#[derive(Debug, Clone)]
pub struct AuditActor {
    pub actor_type: String,
    pub actor_ref: Option<String>,
}

impl AuditActor {
    pub fn from_ctx(ctx: &ActorContext) -> Self {
        let (actor_type, actor_ref) = ctx.audit_ref();
        Self {
            actor_type,
            actor_ref,
        }
    }

    pub fn participant(id: Uuid) -> Self {
        Self {
            actor_type: "PARTICIPANT".into(),
            actor_ref: Some(format!("participant:{id}")),
        }
    }

    pub fn system(name: &str) -> Self {
        Self {
            actor_type: "SYSTEM".into(),
            actor_ref: Some(format!("system:{name}")),
        }
    }

    /// Compact reference stored in `*_by` columns.
    pub fn reference(&self) -> String {
        self.actor_ref
            .clone()
            .unwrap_or_else(|| self.actor_type.to_ascii_lowercase())
    }
}

impl From<&ActorContext> for AuditActor {
    fn from(ctx: &ActorContext) -> Self {
        match ctx.kind {
            ActorKind::System(name) => AuditActor::system(name),
            _ => AuditActor::from_ctx(ctx),
        }
    }
}

pub struct AuditRecord<'a> {
    pub meeting_space_id: Uuid,
    pub conference_id: Option<Uuid>,
    pub action: &'a str,
    pub target_type: Option<&'a str>,
    pub target_ref: Option<String>,
    pub metadata: Value,
}

pub async fn audit<C: ConnectionTrait>(
    conn: &C,
    actor: &AuditActor,
    record: AuditRecord<'_>,
) -> ApiResult<()> {
    conference_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_space_id: Set(record.meeting_space_id),
        conference_id: Set(record.conference_id),
        actor_type: Set(actor.actor_type.clone()),
        actor_ref: Set(actor.actor_ref.clone()),
        action: Set(record.action.to_string()),
        target_type: Set(record.target_type.map(str::to_string)),
        target_ref: Set(record.target_ref),
        metadata: Set(record.metadata),
        request_id: Set(current_request_id()),
        created_at: Set(Utc::now()),
    }
    .insert(conn)
    .await?;
    Ok(())
}

pub struct OutboxRecord<'a> {
    pub aggregate_type: &'a str,
    pub aggregate_id: Uuid,
    pub event_type: &'a str,
    pub meeting_space_id: Option<Uuid>,
    pub conference_id: Option<Uuid>,
    pub payload: Value,
}

/// Next gap-free sequence number for a conference's client events. The row update
/// keeps the conference row locked until the caller's transaction commits.
async fn next_conference_seq<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
) -> ApiResult<Option<i64>> {
    let row = conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE conference_sessions SET event_seq = event_seq + 1 WHERE id = $1 RETURNING event_seq",
            [conference_id.into()],
        ))
        .await?;
    Ok(row.and_then(|r| r.try_get_by_index::<i64>(0).ok()))
}

pub async fn emit<C: ConnectionTrait>(conn: &C, record: OutboxRecord<'_>) -> ApiResult<()> {
    emit_to(conn, record, None).await
}

/// Emit a client event restricted to `audience` (server-side filtered delivery).
pub async fn emit_to<C: ConnectionTrait>(
    conn: &C,
    record: OutboxRecord<'_>,
    audience: Option<Audience>,
) -> ApiResult<()> {
    let audience = audience.map(|a| serde_json::to_value(a).unwrap_or(Value::Null));
    let now = Utc::now();
    let channel = if event::is_provider_command(record.event_type) {
        OutboxChannel::ProviderCommand
    } else {
        OutboxChannel::Realtime
    };
    let conference_seq = match (channel, record.conference_id) {
        (OutboxChannel::Realtime, Some(cid)) => next_conference_seq(conn, cid).await?,
        _ => None,
    };
    outbox_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        aggregate_type: Set(record.aggregate_type.to_string()),
        aggregate_id: Set(record.aggregate_id),
        event_type: Set(record.event_type.to_string()),
        payload: Set(record.payload),
        meeting_space_id: Set(record.meeting_space_id),
        conference_id: Set(record.conference_id),
        status: Set(OutboxStatus::Pending),
        attempts: Set(0),
        available_at: Set(now),
        published_at: Set(None),
        last_error: Set(None),
        request_id: Set(current_request_id()),
        created_at: Set(now),
        channel: Set(channel),
        conference_seq: Set(conference_seq),
        locked_by: Set(None),
        audience: Set(audience),
    }
    .insert(conn)
    .await?;
    Ok(())
}
