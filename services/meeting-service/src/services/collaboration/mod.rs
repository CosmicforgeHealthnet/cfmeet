//! Phase 2 Part F — media controls & collaboration (internal modules of the
//! meeting-service; none of these is a deployable service).
//!
//! Every action follows the same pipeline: server-derived identity (`ActorContext`)
//! → conference scope + state → role/permission (`MeetingPermissionService`) →
//! collaboration policy → authoritative commit with audit + same-transaction outbox →
//! provider enforcement through `RtcControl`/`RtcProvider` after commit → stable,
//! provider-neutral realtime events through the Part E pipeline.
//!
//! - [`policy`]: effective collaboration policy (defaults → space → conference).
//! - [`media`]: `MediaControlService` (locks, host mute, screen share, spotlight) and
//!   the single effective-media projection used for every credential / grant.
//! - [`messaging`]: `MeetingMessagingService` (storage-neutral meeting chat).
//! - [`engagement`]: reactions (transient) and hand raise (persistent).
//! - [`breakouts`]: `BreakoutService` (rooms, assignment, routing, closing).
//! - [`polls`]: `PollService` (polls, quizzes, anonymous ballots).
//! - [`questions`]: `QuestionService` (Q&A).
//! - [`prejoin`]: pre-join resolution contract and consent.

pub mod breakouts;
pub mod engagement;
pub mod media;
pub mod messaging;
pub mod metrics;
pub mod policy;
pub mod polls;
pub mod prejoin;
pub mod questions;

use std::{collections::HashMap, sync::Arc, time::Instant};

use serde::Serialize;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, ConferenceStatus, RecordOrigin, Role},
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_participants, conference_sessions, meeting_spaces},
    services::{
        actor::ActorContext,
        meeting_core::{
            conference_authority, find_conference, find_space, not_found_conference,
            permission_denied, Core,
        },
        meeting_events::{Audience, AuditActor},
        meeting_permissions::{Permission, PermissionContext},
        realtime::{conference_channel, RealtimeEnvelope, SCHEMA_VERSION},
        RedisService,
    },
};

use self::{
    metrics::{inc, COLLAB_METRICS as CM},
    policy::CollaborationPolicy,
};

/// Everything an action needs, resolved from authoritative state.
#[derive(Debug, Clone)]
pub struct Collab {
    pub conference: conference_sessions::Model,
    pub space: meeting_spaces::Model,
    pub pctx: PermissionContext,
    pub policy: CollaborationPolicy,
    /// Effective role (stronger of space role and ADMITTED participant role).
    pub role: Role,
    /// The actor's own ADMITTED participant row (sender / voter / raiser identity).
    pub participant: Option<conference_participants::Model>,
    pub audit: AuditActor,
}

pub fn effective_policy(
    space: &meeting_spaces::Model,
    conference: &conference_sessions::Model,
) -> CollaborationPolicy {
    CollaborationPolicy::effective(
        space.meeting_type,
        &space.collaboration_policy,
        &conference.collaboration_policy,
    )
}

pub fn collaboration_disabled() -> ApiError {
    ApiError::domain(ErrorCode::FeatureDisabled, "Collaboration is not enabled")
}

impl Collab {
    /// Resolve the actor's authority over a conference. No authority (stranger,
    /// waiting, removed, banned) is `PERMISSION_DENIED`; unknown / legacy conferences
    /// are `CONFERENCE_NOT_FOUND`.
    pub async fn load(core: &Core, ctx: &ActorContext, conference_id: Uuid) -> ApiResult<Self> {
        if !core.flags.collaboration_v2_enabled {
            return Err(collaboration_disabled());
        }
        if matches!(
            ctx.kind,
            crate::services::actor::ActorKind::Anonymous
                | crate::services::actor::ActorKind::LegacyGuest(_)
        ) {
            return Err(ApiError::domain(
                ErrorCode::AuthenticationRequired,
                "Sign in or present your participant credential",
            ));
        }
        let conference = find_conference(&core.db, conference_id).await?;
        if conference.origin == RecordOrigin::Legacy {
            return Err(not_found_conference());
        }
        let space = find_space(&core.db, conference.meeting_space_id).await?;
        let authority = conference_authority(&core.db, &space, conference_id, ctx)
            .await?
            .ok_or_else(|| permission_denied("You are not a participant of this conference"))?;
        let pctx = PermissionContext::for_space(&space);
        let policy = effective_policy(&space, &conference);
        Ok(Self {
            participant: authority
                .participant
                .filter(|p| p.admission_state == AdmissionState::Admitted),
            role: authority.role,
            audit: authority.audit,
            conference,
            space,
            pctx,
            policy,
        })
    }

    pub fn allows(&self, p: Permission) -> bool {
        self.pctx.allows(self.role, p)
    }

    pub fn require(&self, p: Permission, message: &str) -> ApiResult<()> {
        if self.allows(p) {
            Ok(())
        } else {
            Err(permission_denied(message))
        }
    }

    /// The actor's own participant (required for self actions: chat, votes, hand).
    pub fn me(&self) -> ApiResult<&conference_participants::Model> {
        self.participant.as_ref().ok_or_else(|| {
            ApiError::domain(
                ErrorCode::ParticipantNotFound,
                "Join the conference before collaborating",
            )
        })
    }

    pub fn is_live(&self) -> bool {
        self.conference.status == ConferenceStatus::Live
    }

    /// Interactive participant collaboration happens while LIVE only (§63).
    pub fn require_live(&self) -> ApiResult<()> {
        if self.is_live() {
            Ok(())
        } else {
            Err(ApiError::domain(
                ErrorCode::ConferenceNotLive,
                format!(
                    "Collaboration is not available while the conference is {}",
                    self.conference.status
                ),
            ))
        }
    }

    /// Configuration (drafts, policy) is allowed before and during LIVE, never after.
    pub fn require_configurable(&self) -> ApiResult<()> {
        if matches!(
            self.conference.status,
            ConferenceStatus::Created
                | ConferenceStatus::Preparing
                | ConferenceStatus::Waiting
                | ConferenceStatus::Live
        ) {
            Ok(())
        } else {
            Err(ApiError::domain(
                ErrorCode::ConferenceStateConflict,
                format!(
                    "A {} conference can no longer be configured",
                    self.conference.status
                ),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Participant-scoped rate limiting (§26, §67)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateAction {
    Chat,
    Reaction,
    HandToggle,
    PollResponse,
    QnaSubmit,
    QnaVote,
    Announcement,
}

impl RateAction {
    /// (max events, window seconds)
    pub fn limit(&self) -> (i64, u64) {
        match self {
            RateAction::Chat => (20, 10),
            RateAction::Reaction => (10, 10),
            RateAction::HandToggle => (10, 60),
            RateAction::PollResponse => (20, 60),
            RateAction::QnaSubmit => (5, 60),
            RateAction::QnaVote => (30, 60),
            RateAction::Announcement => (5, 60),
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            RateAction::Chat => "chat",
            RateAction::Reaction => "reaction",
            RateAction::HandToggle => "hand",
            RateAction::PollResponse => "poll",
            RateAction::QnaSubmit => "qna_submit",
            RateAction::QnaVote => "qna_vote",
            RateAction::Announcement => "announce",
        }
    }
}

/// Fixed-window limiter keyed by the server-authoritative participant id (never by
/// IP). Redis is used when reachable; on Redis failure an in-process window keeps
/// enforcing (fail safe: limits never silently disappear).
pub struct CollabLimiter {
    redis: Option<Arc<RedisService>>,
    local: Mutex<HashMap<String, (i64, Instant)>>,
}

impl CollabLimiter {
    pub fn new(redis: Option<Arc<RedisService>>) -> Self {
        Self {
            redis,
            local: Mutex::new(HashMap::new()),
        }
    }

    async fn local_hit(&self, key: &str, window: u64) -> (i64, u64) {
        let mut map = self.local.lock().await;
        if map.len() > 50_000 {
            map.retain(|_, (_, t)| t.elapsed().as_secs() < 3600);
        }
        let now = Instant::now();
        let entry = map.entry(key.to_string()).or_insert((0, now));
        if now.duration_since(entry.1).as_secs() >= window {
            *entry = (0, now);
        }
        entry.0 += 1;
        (
            entry.0,
            window.saturating_sub(now.duration_since(entry.1).as_secs()),
        )
    }

    /// Count one action for a server-authoritative subject (participant id, or the
    /// audit reference of a host acting without a participant row); `RATE_LIMITED`
    /// (with retry) once over the window budget.
    pub async fn check(&self, action: RateAction, subject: &str) -> ApiResult<()> {
        let (max, window) = action.limit();
        let key = format!("rl:collab:{}:{subject}", action.key());
        let mut outcome = None;
        if let Some(redis) = &self.redis {
            if let Ok(count) = redis.incr_with_ttl(&key, window).await {
                let retry = if count > max {
                    redis
                        .ttl(&key)
                        .await
                        .ok()
                        .filter(|t| *t > 0)
                        .unwrap_or(window as i64) as u64
                } else {
                    0
                };
                outcome = Some((count, retry));
            }
        }
        let (count, retry) = match outcome {
            Some(o) => o,
            None => self.local_hit(&key, window).await,
        };
        if count > max {
            inc(&CM.rate_limited);
            return Err(ApiError::rate_limited(
                "Too many requests; slow down and retry shortly",
                retry.max(1),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Transient realtime (reactions) — Redis fanout only, never the outbox (§28).
// ---------------------------------------------------------------------------

/// Redis message carrying a transient event (no sequence, no replay).
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransientMessage {
    pub transient: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<Audience>,
    pub event: RealtimeEnvelope,
}

/// Publish a transient event. Returns whether it reached the fanout (Redis down →
/// dropped: transient events carry no durable state).
pub async fn publish_transient(
    redis: Option<&Arc<RedisService>>,
    conference: &conference_sessions::Model,
    event_type: &str,
    payload: serde_json::Value,
    audience: Option<Audience>,
) -> bool {
    let Some(redis) = redis else { return false };
    let msg = TransientMessage {
        transient: true,
        audience,
        event: RealtimeEnvelope {
            event_id: Uuid::new_v4(),
            event_type: event_type.to_string(),
            schema_version: SCHEMA_VERSION,
            conference_id: Some(conference.id),
            meeting_space_id: Some(conference.meeting_space_id),
            sequence: None,
            occurred_at: chrono::Utc::now(),
            correlation_id: crate::middleware::request_id::current_request_id(),
            payload,
        },
    };
    let Ok(body) = serde_json::to_string(&msg) else {
        return false;
    };
    redis
        .publish(&conference_channel(conference.id), &body)
        .await
        .is_ok()
}
