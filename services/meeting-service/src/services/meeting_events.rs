//! Meeting-domain audit + transactional outbox writers (spec §B12, §B13, §3.5).
//!
//! Both are written on the caller's transaction so business state, audit and the
//! delivery event commit atomically. Events are delivery, never authority.
//! Never pass raw passcodes, invitation tokens or credentials in metadata/payloads.

use chrono::Utc;
use sea_orm::{ActiveModelTrait, ConnectionTrait, Set};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    domain::OutboxStatus,
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
    pub const CONFERENCE_STARTED: &str = "conference.started";
    pub const CONFERENCE_LOCKED: &str = "conference.locked";
    pub const CONFERENCE_UNLOCKED: &str = "conference.unlocked";
    pub const CONFERENCE_ENDING: &str = "conference.ending";
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
    /// Provider enforcement intents consumed by the Part E control plane.
    pub const RTC_ROOM_TERMINATE_REQUESTED: &str = "rtc.room_terminate_requested";
    pub const RTC_PARTICIPANT_REMOVE_REQUESTED: &str = "rtc.participant_remove_requested";
    pub const RTC_PERMISSIONS_UPDATE_REQUESTED: &str = "rtc.permissions_update_requested";
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

pub async fn emit<C: ConnectionTrait>(conn: &C, record: OutboxRecord<'_>) -> ApiResult<()> {
    let now = Utc::now();
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
    }
    .insert(conn)
    .await?;
    Ok(())
}
