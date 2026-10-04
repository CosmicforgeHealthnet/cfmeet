//! MediaControlService (Part F §6–§15).
//!
//! The single source of media publication rights is [`effective_media`]:
//!
//! ```text
//! role grant (MeetingPermissionService)
//!   ∧ screen-share policy tier (HOST_ONLY / PRESENTERS_ONLY / ALL_INTERACTIVE)
//!   ∧ ¬ conference-wide audio/video lock (roles without participant.mute)
//!   ∧ ¬ participant-specific audio / video / screen-share lock
//! ```
//!
//! Every credential (join, refresh, breakout routing), every provider permission
//! update and the reconciler use it, so a restriction can only reduce rights (§55)
//! and a modified client can never publish beyond it: the provider enforces the grant.
//!
//! Distinctions kept explicit: MUTE NOW (`participant.muted`, provider mute of current
//! tracks) ≠ LOCK (persistent restriction); STOP SHARE NOW (current track only) ≠
//! DISABLE FUTURE SHARING (screen-share lock); SPOTLIGHT (shared, persisted) ≠ personal
//! PIN (client-local, never stored).

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, ConferenceStatus, Role, SessionState},
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_participants, conference_sessions, participant_sessions},
    services::{
        actor::ActorContext,
        meeting_core::{
            find_scoped_participant, find_space, lock_conference, permission_denied, Core,
        },
        meeting_events::{action, audit, emit, event, AuditRecord, OutboxRecord},
        meeting_permissions::{can_moderate, MediaPermissions, Permission, PermissionContext},
        rtc_control::{Enforcement, RtcControl},
        rtc_provider::{RtcError, RtcParticipantRef, TrackKind},
    },
};

use super::{
    breakouts::media_context,
    effective_policy,
    metrics::{inc, COLLAB_METRICS as CM},
    policy::{CollaborationPolicy, CollaborationPolicyPatch, ScreenSharePolicy},
    Collab,
};

/// Participant-specific restrictions (authoritative columns).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaRestrictions {
    pub audio_locked: bool,
    pub video_locked: bool,
    pub screen_share_locked: bool,
}

impl MediaRestrictions {
    pub fn of(p: &conference_participants::Model) -> Self {
        Self {
            audio_locked: p.audio_locked_at.is_some(),
            video_locked: p.video_locked_at.is_some(),
            screen_share_locked: p.screen_share_locked_at.is_some(),
        }
    }
}

/// Whether the screen-share policy tier admits `role` (the role grant is still required).
pub fn screen_share_tier_allows(policy: ScreenSharePolicy, role: Role) -> bool {
    match policy {
        ScreenSharePolicy::HostOnly => role.rank() >= Role::CoHost.rank(),
        ScreenSharePolicy::PresentersOnly => role.rank() >= Role::Presenter.rank(),
        ScreenSharePolicy::AllInteractive => true,
    }
}

/// The effective media projection (pure; see module docs).
pub fn effective_media(
    pctx: &PermissionContext,
    policy: &CollaborationPolicy,
    role: Role,
    restrictions: MediaRestrictions,
) -> MediaPermissions {
    let mut m = pctx.media(role);
    let exempt = pctx.allows(role, Permission::ParticipantMute);
    if restrictions.audio_locked || (policy.audio_locked && !exempt) {
        m.audio_publish = false;
    }
    if restrictions.video_locked || (policy.video_locked && !exempt) {
        m.video_publish = false;
    }
    m.screen_share_publish = m.screen_share_publish
        && screen_share_tier_allows(policy.screen_share_policy, role)
        && !restrictions.screen_share_locked;
    m
}

/// Effective media of a participant from authoritative state.
pub async fn effective_media_for<C: ConnectionTrait>(
    conn: &C,
    conference: &conference_sessions::Model,
    participant: &conference_participants::Model,
) -> ApiResult<MediaPermissions> {
    if participant.admission_state != AdmissionState::Admitted {
        return Ok(MediaPermissions::none());
    }
    let space = find_space(conn, conference.meeting_space_id).await?;
    Ok(effective_media(
        &PermissionContext::for_space(&space),
        &effective_policy(&space, conference),
        participant.role,
        MediaRestrictions::of(participant),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    Audio,
    Video,
    ScreenShare,
}

impl LockKind {
    fn permission(&self) -> Permission {
        match self {
            LockKind::Audio => Permission::ParticipantAudioLock,
            LockKind::Video => Permission::ParticipantVideoLock,
            LockKind::ScreenShare => Permission::ParticipantScreenShareStop,
        }
    }

    fn events(&self) -> (&'static str, &'static str) {
        match self {
            LockKind::Audio => (
                event::PARTICIPANT_AUDIO_LOCKED,
                event::PARTICIPANT_AUDIO_UNLOCKED,
            ),
            LockKind::Video => (
                event::PARTICIPANT_VIDEO_LOCKED,
                event::PARTICIPANT_VIDEO_UNLOCKED,
            ),
            LockKind::ScreenShare => (
                event::PARTICIPANT_SCREEN_SHARE_LOCKED,
                event::PARTICIPANT_SCREEN_SHARE_UNLOCKED,
            ),
        }
    }

    fn actions(&self) -> (&'static str, &'static str) {
        match self {
            LockKind::Audio => (action::MEDIA_AUDIO_LOCK, action::MEDIA_AUDIO_UNLOCK),
            LockKind::Video => (action::MEDIA_VIDEO_LOCK, action::MEDIA_VIDEO_UNLOCK),
            LockKind::ScreenShare => (
                action::MEDIA_SCREEN_SHARE_LOCK,
                action::MEDIA_SCREEN_SHARE_UNLOCK,
            ),
        }
    }

    fn track(&self) -> TrackKind {
        match self {
            LockKind::Audio => TrackKind::Audio,
            LockKind::Video => TrackKind::Video,
            LockKind::ScreenShare => TrackKind::ScreenShare,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaOutcome {
    pub participant: conference_participants::Model,
    pub changed: bool,
    pub enforcement: Enforcement,
    pub permissions: MediaPermissions,
}

#[derive(Debug, Clone)]
pub struct SpotlightEntry {
    pub participant_id: Uuid,
    pub display_name: String,
    pub spotlighted_at: chrono::DateTime<Utc>,
}

pub fn media_payload(p: &conference_participants::Model) -> serde_json::Value {
    json!({
        "conferenceId": p.conference_id,
        "participantId": p.id,
        "audioLocked": p.audio_locked_at.is_some(),
        "videoLocked": p.video_locked_at.is_some(),
        "screenShareLocked": p.screen_share_locked_at.is_some(),
        "collabVersion": p.collab_version,
    })
}

fn participant_record<'a>(
    conference: &conference_sessions::Model,
    p: &conference_participants::Model,
    event_type: &'a str,
    payload: serde_json::Value,
) -> OutboxRecord<'a> {
    OutboxRecord {
        aggregate_type: "conference_participant",
        aggregate_id: p.id,
        event_type,
        meeting_space_id: Some(conference.meeting_space_id),
        conference_id: Some(conference.id),
        payload,
    }
}

/// Queue the provider grant refresh for a participant (executed after commit).
pub async fn queue_permission_sync<C: ConnectionTrait>(
    conn: &C,
    conference: &conference_sessions::Model,
    participant_id: Uuid,
) -> ApiResult<()> {
    emit(
        conn,
        OutboxRecord {
            aggregate_type: "conference_participant",
            aggregate_id: participant_id,
            event_type: event::RTC_PERMISSIONS_UPDATE_REQUESTED,
            meeting_space_id: Some(conference.meeting_space_id),
            conference_id: Some(conference.id),
            payload: json!({ "conferenceId": conference.id, "participantId": participant_id }),
        },
    )
    .await
}

pub struct MediaControlService {
    core: Core,
    rtc: RtcControl,
}

impl MediaControlService {
    pub fn new(core: Core) -> Self {
        Self {
            rtc: RtcControl::new(core.clone()),
            core,
        }
    }

    pub fn rtc(&self) -> &RtcControl {
        &self.rtc
    }

    fn require_moderation(
        c: &Collab,
        target: &conference_participants::Model,
        permission: Permission,
        message: &str,
    ) -> ApiResult<()> {
        c.require(permission, message)?;
        if c.participant.as_ref().map(|p| p.id) == Some(target.id) {
            return Err(permission_denied(
                "You cannot apply this control to yourself",
            ));
        }
        if !can_moderate(c.role, target.role) {
            return Err(permission_denied(
                "You can only control less privileged participants",
            ));
        }
        Ok(())
    }

    /// Persistent audio / video / screen-share lock (idempotent).
    pub async fn set_lock(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
        kind: LockKind,
        locked: bool,
    ) -> ApiResult<MediaOutcome> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        // Scope first: a foreign participant is indistinguishable from an unknown one.
        let target =
            find_scoped_participant(&self.core.db, conference_id, participant_id, false).await?;
        Self::require_moderation(
            &c,
            &target,
            kind.permission(),
            "Your role does not allow this media control",
        )?;
        c.require_configurable()?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let target = find_scoped_participant(&txn, conference_id, participant_id, true).await?;
        if target.admission_state != AdmissionState::Admitted {
            return Err(ApiError::domain(
                ErrorCode::ParticipantStateConflict,
                "Only admitted participants have media to control",
            ));
        }
        let current = match kind {
            LockKind::Audio => target.audio_locked_at.is_some(),
            LockKind::Video => target.video_locked_at.is_some(),
            LockKind::ScreenShare => target.screen_share_locked_at.is_some(),
        };
        if current == locked {
            txn.commit().await?;
            let permissions = effective_media_for(&self.core.db, &conference, &target).await?;
            return Ok(MediaOutcome {
                participant: target,
                changed: false,
                enforcement: Enforcement::NotRequired,
                permissions,
            });
        }
        let now = Utc::now();
        let (at, by) = if locked {
            (Some(now), Some(c.audit.reference()))
        } else {
            (None, None)
        };
        let mut am: conference_participants::ActiveModel = target.clone().into();
        match kind {
            LockKind::Audio => {
                am.audio_locked_at = Set(at);
                am.audio_locked_by = Set(by);
            }
            LockKind::Video => {
                am.video_locked_at = Set(at);
                am.video_locked_by = Set(by);
            }
            LockKind::ScreenShare => {
                am.screen_share_locked_at = Set(at);
                am.screen_share_locked_by = Set(by);
            }
        }
        am.collab_version = Set(target.collab_version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        let (lock_event, unlock_event) = kind.events();
        let (lock_action, unlock_action) = kind.actions();
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference.id),
                action: if locked { lock_action } else { unlock_action },
                target_type: Some("PARTICIPANT"),
                target_ref: Some(updated.id.to_string()),
                metadata: json!({}),
            },
        )
        .await?;
        emit(
            &txn,
            participant_record(
                &conference,
                &updated,
                if locked { lock_event } else { unlock_event },
                media_payload(&updated),
            ),
        )
        .await?;
        let permissions = effective_media(
            &c.pctx,
            &c.policy,
            updated.role,
            MediaRestrictions::of(&updated),
        );
        emit(
            &txn,
            participant_record(
                &conference,
                &updated,
                event::PARTICIPANT_MEDIA_PERMISSION_CHANGED,
                json!({ "participantId": updated.id, "permissions": permissions }),
            ),
        )
        .await?;
        let provider =
            self.rtc.participant_control() && conference.status == ConferenceStatus::Live;
        if provider {
            queue_permission_sync(&txn, &conference, updated.id).await?;
        }
        txn.commit().await?;
        match kind {
            LockKind::Audio => inc(&CM.audio_locks),
            LockKind::Video => inc(&CM.video_locks),
            LockKind::ScreenShare => inc(&CM.screen_share_locks),
        }
        let mut enforcement = if provider {
            self.rtc.dispatch_for_conference(conference_id).await
        } else {
            Enforcement::NotRequired
        };
        // Revoking the grant stops future publication; also mute what is live now.
        if locked && provider {
            if let Err(e) = self.mute_tracks(&conference, &updated, kind.track()).await {
                tracing::warn!(conference_id = %conference_id, "lock: provider mute failed: {e}");
                inc(&CM.provider_media_failures);
                enforcement = Enforcement::Pending;
            }
        }
        Ok(MediaOutcome {
            participant: updated,
            changed: true,
            enforcement,
            permissions,
        })
    }

    /// Mute every live track of `kind` on all CONNECTED sessions (current context room).
    async fn mute_tracks(
        &self,
        conference: &conference_sessions::Model,
        participant: &conference_participants::Model,
        kind: TrackKind,
    ) -> Result<u32, RtcError> {
        let sessions = participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceParticipantId.eq(participant.id))
            .filter(participant_sessions::Column::ConferenceId.eq(conference.id))
            .filter(participant_sessions::Column::State.eq(SessionState::Connected))
            .all(&self.core.db)
            .await
            .map_err(|e| RtcError::Unavailable(e.to_string()))?;
        let room = media_context(&self.core.db, conference, participant)
            .await
            .map_err(|e| RtcError::Unavailable(e.to_string()))?
            .room_name(conference);
        let mut muted = 0;
        for s in &sessions {
            let target = RtcParticipantRef {
                conference_id: conference.id,
                room_name: room.clone(),
                participant_session_id: s.id,
                media_identity: s.media_identity.clone(),
            };
            match self.core.rtc.mute_track(&target, kind).await {
                Ok(n) => muted += n,
                Err(RtcError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(muted)
    }

    /// STOP SHARE NOW: terminate the participant's active screen share without locking
    /// future sharing. The provider grant is withdrawn (the provider unpublishes the
    /// track), the track is muted, then the grant is restored to the effective value.
    /// `screenshare.ended` is emitted only after the provider confirmed.
    pub async fn stop_screen_share(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
    ) -> ApiResult<u32> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let target =
            find_scoped_participant(&self.core.db, conference_id, participant_id, false).await?;
        let is_self = c.participant.as_ref().map(|p| p.id) == Some(target.id);
        if !is_self {
            Self::require_moderation(
                &c,
                &target,
                Permission::ParticipantScreenShareStop,
                "Your role does not allow stopping screen shares",
            )?;
        }
        c.require_live()?;
        let sharing = participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceParticipantId.eq(target.id))
            .filter(participant_sessions::Column::ConferenceId.eq(conference_id))
            .filter(participant_sessions::Column::ScreenShareStartedAt.is_not_null())
            .all(&self.core.db)
            .await?;
        if sharing.is_empty() {
            return Err(ApiError::domain(
                ErrorCode::ScreenShareNotActive,
                "This participant is not sharing their screen",
            ));
        }
        if !self.rtc.participant_control() {
            return Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Provider media control is not enabled",
            ));
        }
        let conference = c.conference.clone();
        let effective = effective_media(
            &c.pctx,
            &c.policy,
            target.role,
            MediaRestrictions::of(&target),
        );
        let withdrawn = MediaPermissions {
            screen_share_publish: false,
            ..effective
        };
        let room = media_context(&self.core.db, &conference, &target)
            .await?
            .room_name(&conference);
        let mut provider_error = None;
        for s in &sharing {
            let target_ref = RtcParticipantRef {
                conference_id,
                room_name: room.clone(),
                participant_session_id: s.id,
                media_identity: s.media_identity.clone(),
            };
            let result = async {
                match self
                    .core
                    .rtc
                    .update_permissions(&target_ref, &withdrawn)
                    .await
                {
                    Ok(()) | Err(RtcError::NotFound) => {}
                    Err(e) => return Err(e),
                }
                match self
                    .core
                    .rtc
                    .mute_track(&target_ref, TrackKind::ScreenShare)
                    .await
                {
                    Ok(_) | Err(RtcError::NotFound) => {}
                    Err(e) => return Err(e),
                }
                match self
                    .core
                    .rtc
                    .update_permissions(&target_ref, &effective)
                    .await
                {
                    Ok(()) | Err(RtcError::NotFound) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            .await;
            if let Err(e) = result {
                provider_error = Some(e);
                break;
            }
        }
        if let Some(e) = provider_error {
            inc(&CM.provider_media_failures);
            // Never claim a stop the provider did not confirm; the grant is re-asserted
            // from authoritative state by the queued sync and the reconciler.
            let txn = self.core.db.begin().await?;
            audit(
                &txn,
                &c.audit,
                AuditRecord {
                    meeting_space_id: conference.meeting_space_id,
                    conference_id: Some(conference_id),
                    action: action::SCREEN_SHARE_STOP,
                    target_type: Some("PARTICIPANT"),
                    target_ref: Some(target.id.to_string()),
                    metadata: json!({ "result": "PROVIDER_FAILED" }),
                },
            )
            .await?;
            queue_permission_sync(&txn, &conference, target.id).await?;
            txn.commit().await?;
            return Err(e.into());
        }
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let stopped = end_screen_share(&txn, &conference, &target, None, "HOST_STOPPED").await?;
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::SCREEN_SHARE_STOP,
                target_type: Some("PARTICIPANT"),
                target_ref: Some(target.id.to_string()),
                metadata: json!({ "result": "PROVIDER_CONFIRMED", "sessions": stopped }),
            },
        )
        .await?;
        txn.commit().await?;
        inc(&CM.screen_share_stops);
        Ok(stopped)
    }

    pub async fn list_spotlights(&self, conference_id: Uuid) -> ApiResult<Vec<SpotlightEntry>> {
        Ok(conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference_id))
            .filter(conference_participants::Column::SpotlightedAt.is_not_null())
            .order_by_asc(conference_participants::Column::SpotlightedAt)
            .all(&self.core.db)
            .await?
            .into_iter()
            .map(|p| SpotlightEntry {
                participant_id: p.id,
                display_name: p.display_name,
                spotlighted_at: p.spotlighted_at.unwrap_or_default(),
            })
            .collect())
    }

    /// Shared spotlight (presentation metadata only — never media rights or identity).
    pub async fn spotlight(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
        on: bool,
    ) -> ApiResult<bool> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let _ =
            find_scoped_participant(&self.core.db, conference_id, participant_id, false).await?;
        c.require(
            Permission::ParticipantSpotlight,
            "Your role does not allow spotlighting",
        )?;
        c.require_live()?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let target = find_scoped_participant(&txn, conference_id, participant_id, true).await?;
        if on && target.admission_state != AdmissionState::Admitted {
            return Err(ApiError::domain(
                ErrorCode::ParticipantStateConflict,
                "Only admitted participants can be spotlighted",
            ));
        }
        if target.spotlighted_at.is_some() == on {
            txn.commit().await?;
            return Ok(false);
        }
        let now = Utc::now();
        if on {
            let current = conference_participants::Entity::find()
                .filter(conference_participants::Column::ConferenceId.eq(conference_id))
                .filter(conference_participants::Column::SpotlightedAt.is_not_null())
                .order_by_asc(conference_participants::Column::SpotlightedAt)
                .all(&txn)
                .await?;
            let max = c.policy.max_spotlights as usize;
            if current.len() >= max {
                if max == 1 {
                    // Single spotlight: the new one replaces the old one.
                    for old in current {
                        self.clear_spotlight(&txn, &conference, old, &c, now)
                            .await?;
                    }
                } else {
                    return Err(ApiError::domain(
                        ErrorCode::ValidationError,
                        format!("At most {max} participants can be spotlighted"),
                    ));
                }
            }
            let mut am: conference_participants::ActiveModel = target.clone().into();
            am.spotlighted_at = Set(Some(now));
            am.spotlighted_by = Set(Some(c.audit.reference()));
            am.collab_version = Set(target.collab_version + 1);
            am.updated_at = Set(now);
            let updated = am.update(&txn).await?;
            audit(
                &txn,
                &c.audit,
                AuditRecord {
                    meeting_space_id: conference.meeting_space_id,
                    conference_id: Some(conference_id),
                    action: action::SPOTLIGHT_SET,
                    target_type: Some("PARTICIPANT"),
                    target_ref: Some(updated.id.to_string()),
                    metadata: json!({}),
                },
            )
            .await?;
            emit(
                &txn,
                participant_record(
                    &conference,
                    &updated,
                    event::PARTICIPANT_SPOTLIGHTED,
                    json!({ "participantId": updated.id, "spotlightedAt": now }),
                ),
            )
            .await?;
        } else {
            self.clear_spotlight(&txn, &conference, target, &c, now)
                .await?;
        }
        txn.commit().await?;
        inc(&CM.spotlight_changes);
        Ok(true)
    }

    async fn clear_spotlight(
        &self,
        txn: &DatabaseTransaction,
        conference: &conference_sessions::Model,
        p: conference_participants::Model,
        c: &Collab,
        now: chrono::DateTime<Utc>,
    ) -> ApiResult<()> {
        let mut am: conference_participants::ActiveModel = p.clone().into();
        am.spotlighted_at = Set(None);
        am.spotlighted_by = Set(None);
        am.collab_version = Set(p.collab_version + 1);
        am.updated_at = Set(now);
        let updated = am.update(txn).await?;
        audit(
            txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference.id),
                action: action::SPOTLIGHT_REMOVE,
                target_type: Some("PARTICIPANT"),
                target_ref: Some(updated.id.to_string()),
                metadata: json!({}),
            },
        )
        .await?;
        emit(
            txn,
            participant_record(
                conference,
                &updated,
                event::PARTICIPANT_SPOTLIGHT_REMOVED,
                json!({ "participantId": updated.id }),
            ),
        )
        .await
    }

    /// Conference-level collaboration policy (optimistic `collaborationVersion`).
    pub async fn update_policy(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        expected_version: i32,
        patch: CollaborationPolicyPatch,
    ) -> ApiResult<(conference_sessions::Model, CollaborationPolicy, Enforcement)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::ConferenceConfigureLive,
            "Your role does not allow changing collaboration policy",
        )?;
        c.require_configurable()?;
        patch
            .validate()
            .map_err(|m| ApiError::domain(ErrorCode::ValidationError, m))?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        if conference.collaboration_version != expected_version {
            return Err(ApiError::domain(
                ErrorCode::MeetingVersionConflict,
                "Collaboration policy changed; reload and retry",
            ));
        }
        let before = effective_policy(&c.space, &conference);
        let merged =
            CollaborationPolicyPatch::from_json(&conference.collaboration_policy).merged(&patch);
        let mut am: conference_sessions::ActiveModel = conference.clone().into();
        am.collaboration_policy = Set(merged.to_json());
        am.collaboration_version = Set(conference.collaboration_version + 1);
        am.updated_at = Set(Utc::now());
        let updated = am.update(&txn).await?;
        let after = effective_policy(&c.space, &updated);
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: updated.meeting_space_id,
                conference_id: Some(updated.id),
                action: action::COLLABORATION_POLICY_UPDATE,
                target_type: Some("CONFERENCE"),
                target_ref: Some(updated.id.to_string()),
                metadata: json!({ "changes": patch.to_json() }),
            },
        )
        .await?;
        let record = |event_type| OutboxRecord {
            aggregate_type: "conference",
            aggregate_id: updated.id,
            event_type,
            meeting_space_id: Some(updated.meeting_space_id),
            conference_id: Some(updated.id),
            payload: json!({
                "conferenceId": updated.id,
                "collaborationVersion": updated.collaboration_version,
                "policy": after,
            }),
        };
        emit(&txn, record(event::COLLABORATION_POLICY_CHANGED)).await?;
        let chat_changed = before.chat_enabled != after.chat_enabled
            || before.chat_send_mode != after.chat_send_mode
            || before.private_chat_mode != after.private_chat_mode
            || before.breakout_chat_enabled != after.breakout_chat_enabled
            || before.main_chat_visible_in_breakouts != after.main_chat_visible_in_breakouts;
        if chat_changed {
            emit(&txn, record(event::CHAT_POLICY_CHANGED)).await?;
        }
        // Deterministic semantics: disabling hand raise clears raised hands (§30).
        if before.hand_raise_enabled && !after.hand_raise_enabled {
            super::engagement::clear_all_hands(&txn, &updated, &c.audit, "POLICY_DISABLED").await?;
        }
        let media_changed = before.audio_locked != after.audio_locked
            || before.video_locked != after.video_locked
            || before.screen_share_policy != after.screen_share_policy;
        let provider = media_changed
            && self.rtc.participant_control()
            && updated.status == ConferenceStatus::Live;
        let admitted = if provider {
            conference_participants::Entity::find()
                .filter(conference_participants::Column::ConferenceId.eq(conference_id))
                .filter(
                    conference_participants::Column::AdmissionState.eq(AdmissionState::Admitted),
                )
                .all(&txn)
                .await?
        } else {
            vec![]
        };
        for p in &admitted {
            queue_permission_sync(&txn, &updated, p.id).await?;
        }
        txn.commit().await?;
        let mut enforcement = Enforcement::NotRequired;
        if provider {
            enforcement = self.rtc.dispatch_for_conference(conference_id).await;
            let newly_locked: Vec<TrackKind> = [
                (after.audio_locked && !before.audio_locked).then_some(TrackKind::Audio),
                (after.video_locked && !before.video_locked).then_some(TrackKind::Video),
            ]
            .into_iter()
            .flatten()
            .collect();
            for p in admitted
                .iter()
                .filter(|p| !c.pctx.allows(p.role, Permission::ParticipantMute))
            {
                for kind in &newly_locked {
                    if self.mute_tracks(&updated, p, *kind).await.is_err() {
                        inc(&CM.provider_media_failures);
                        enforcement = Enforcement::Pending;
                    }
                }
            }
        }
        Ok((updated, after, enforcement))
    }
}

/// Canonical collaboration snapshot of a conference for one viewer (REST recovery
/// source after realtime gaps).
#[derive(Debug, Clone)]
pub struct CollaborationState {
    pub conference: conference_sessions::Model,
    pub policy: CollaborationPolicy,
    pub me: Option<(
        conference_participants::Model,
        MediaPermissions,
        super::breakouts::MediaContext,
    )>,
    pub spotlights: Vec<SpotlightEntry>,
    pub raised_hands: Vec<super::engagement::RaisedHand>,
    pub active_screen_shares: Vec<Uuid>,
}

/// Issued media credential for the caller's current context.
#[derive(Debug, Clone)]
pub struct MediaCredential {
    pub credentials: crate::services::rtc_provider::JoinCredentials,
    pub context: super::breakouts::MediaContext,
    pub permissions: MediaPermissions,
}

impl MediaControlService {
    pub async fn state(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<CollaborationState> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let me = match &c.participant {
            Some(p) => {
                let perms = effective_media(&c.pctx, &c.policy, p.role, MediaRestrictions::of(p));
                let mc = media_context(&self.core.db, &c.conference, p).await?;
                Some((p.clone(), perms, mc))
            }
            None => None,
        };
        let sharing: Vec<Uuid> = participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceId.eq(conference_id))
            .filter(participant_sessions::Column::ScreenShareStartedAt.is_not_null())
            .filter(
                participant_sessions::Column::State
                    .is_in([SessionState::Pending, SessionState::Connected]),
            )
            .all(&self.core.db)
            .await?
            .into_iter()
            .map(|s| s.conference_participant_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(CollaborationState {
            spotlights: self.list_spotlights(conference_id).await?,
            raised_hands: super::engagement::EngagementService::new(self.core.clone())
                .raised_hands(conference_id)
                .await?,
            active_screen_shares: sharing,
            policy: c.policy.clone(),
            conference: c.conference.clone(),
            me,
        })
    }

    /// Credential refresh / re-route: a fresh, bounded credential for the caller's own
    /// session in its CURRENT authorized context, with CURRENT effective grants (a
    /// lock or reassignment committed earlier can never be bypassed by refreshing).
    pub async fn issue_media_credential(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_session_id: Uuid,
    ) -> ApiResult<MediaCredential> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.clone();
        c.require_live()?;
        if !self.rtc.room_control() {
            return Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Provider media control is not enabled",
            ));
        }
        let session = participant_sessions::Entity::find_by_id(participant_session_id)
            .filter(participant_sessions::Column::ConferenceId.eq(conference_id))
            .filter(participant_sessions::Column::ConferenceParticipantId.eq(me.id))
            .one(&self.core.db)
            .await?
            .filter(|s| s.state.is_active())
            .ok_or_else(|| {
                ApiError::domain(
                    ErrorCode::ParticipantNotFound,
                    "Session not found for this participant",
                )
            })?;
        let context = media_context(&self.core.db, &c.conference, &me).await?;
        super::breakouts::ensure_context_room(&self.core, &self.rtc, &c.conference, &context)
            .await?;
        let permissions = effective_media(&c.pctx, &c.policy, me.role, MediaRestrictions::of(&me));
        let credentials = self
            .core
            .rtc
            .issue_join_credentials(&crate::services::rtc_provider::JoinCredentialRequest {
                participant: RtcParticipantRef {
                    conference_id,
                    room_name: context.room_name(&c.conference),
                    participant_session_id: session.id,
                    media_identity: session.media_identity.clone(),
                },
                display_name: me.display_name.clone(),
                permissions,
                ttl: self.rtc.credential_ttl(),
            })
            .await?;
        // Fail closed if authority changed while minting (lock, move, removal, end).
        let conference =
            crate::services::meeting_core::find_conference(&self.core.db, conference_id).await?;
        let current = find_scoped_participant(&self.core.db, conference_id, me.id, false).await?;
        let still_active = participant_sessions::Entity::find_by_id(session.id)
            .one(&self.core.db)
            .await?
            .map(|s| s.state.is_active())
            .unwrap_or(false);
        let now_context = media_context(&self.core.db, &conference, &current).await?;
        let now_permissions = effective_media_for(&self.core.db, &conference, &current).await?;
        if conference.status != ConferenceStatus::Live
            || current.admission_state != AdmissionState::Admitted
            || !still_active
            || now_context != context
            || now_permissions != permissions
        {
            return Err(ApiError::domain(
                ErrorCode::ParticipantStateConflict,
                "Your media access changed while the credential was issued; request it again",
            ));
        }
        Ok(MediaCredential {
            credentials,
            context,
            permissions,
        })
    }
}

/// Clear the provider-evidenced screen share of a participant (all sessions, or one).
/// Emits `screenshare.ended` once; idempotent (duplicate webhooks / host stop races).
pub async fn end_screen_share(
    txn: &DatabaseTransaction,
    conference: &conference_sessions::Model,
    participant: &conference_participants::Model,
    session_id: Option<Uuid>,
    reason: &str,
) -> ApiResult<u32> {
    let mut q = participant_sessions::Entity::find()
        .filter(participant_sessions::Column::ConferenceParticipantId.eq(participant.id))
        .filter(participant_sessions::Column::ConferenceId.eq(conference.id))
        .filter(participant_sessions::Column::ScreenShareStartedAt.is_not_null());
    if let Some(s) = session_id {
        q = q.filter(participant_sessions::Column::Id.eq(s));
    }
    let rows = q.all(txn).await?;
    let now = Utc::now();
    for s in &rows {
        let mut am: participant_sessions::ActiveModel = s.clone().into();
        am.screen_share_started_at = Set(None);
        am.screen_share_track_ref = Set(None);
        am.updated_at = Set(now);
        am.update(txn).await?;
        emit(
            txn,
            participant_record(
                conference,
                participant,
                event::SCREENSHARE_ENDED,
                json!({
                    "participantId": participant.id,
                    "participantSessionId": s.id,
                    "reason": reason,
                    "endedAt": now,
                }),
            ),
        )
        .await?;
    }
    Ok(rows.len() as u32)
}

/// Provider evidence of a screen-share track (verified webhook). Operational state
/// only: never changes role, admission or access. A share the participant is not
/// allowed to publish (policy changed in flight) is stopped instead of recorded.
pub async fn on_screen_share_track(
    txn: &DatabaseTransaction,
    conference: &conference_sessions::Model,
    participant: &conference_participants::Model,
    session: &participant_sessions::Model,
    published: bool,
    track_ref: Option<&str>,
) -> ApiResult<&'static str> {
    if !published {
        let ended = end_screen_share(
            txn,
            conference,
            participant,
            Some(session.id),
            "PROVIDER_ENDED",
        )
        .await?;
        return Ok(if ended > 0 { "APPLIED" } else { "DUPLICATE" });
    }
    if session.screen_share_started_at.is_some() {
        return Ok("DUPLICATE");
    }
    let allowed = effective_media_for(txn, conference, participant)
        .await?
        .screen_share_publish;
    if !allowed {
        emit(
            txn,
            OutboxRecord {
                aggregate_type: "participant_session",
                aggregate_id: session.id,
                event_type: event::RTC_SCREENSHARE_STOP_REQUESTED,
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference.id),
                payload: json!({
                    "conferenceId": conference.id,
                    "participantId": participant.id,
                    "participantSessionId": session.id,
                }),
            },
        )
        .await?;
        return Ok("REVOKED");
    }
    let now = Utc::now();
    let mut am: participant_sessions::ActiveModel = session.clone().into();
    am.screen_share_started_at = Set(Some(now));
    am.screen_share_track_ref = Set(track_ref.map(|t| t.chars().take(160).collect()));
    am.updated_at = Set(now);
    am.update(txn).await?;
    emit(
        txn,
        participant_record(
            conference,
            participant,
            event::SCREENSHARE_STARTED,
            json!({
                "participantId": participant.id,
                "participantSessionId": session.id,
                "startedAt": now,
            }),
        ),
    )
    .await?;
    inc(&CM.screen_share_starts);
    Ok("APPLIED")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MeetingType;

    fn policy() -> CollaborationPolicy {
        CollaborationPolicy::defaults(MeetingType::Standard)
    }

    #[test]
    fn locks_only_reduce_rights() {
        let pctx = PermissionContext::default();
        let free = effective_media(
            &pctx,
            &policy(),
            Role::Participant,
            MediaRestrictions::default(),
        );
        assert!(free.audio_publish && free.video_publish && free.screen_share_publish);
        let locked = effective_media(
            &pctx,
            &policy(),
            Role::Participant,
            MediaRestrictions {
                audio_locked: true,
                video_locked: true,
                screen_share_locked: true,
            },
        );
        assert!(!locked.audio_publish && !locked.video_publish && !locked.screen_share_publish);
        assert!(locked.subscribe, "a lock never removes subscription");
        // A viewer is never elevated by an absent lock.
        let viewer = effective_media(&pctx, &policy(), Role::Viewer, MediaRestrictions::default());
        assert!(!viewer.can_publish_anything());
    }

    #[test]
    fn conference_wide_lock_exempts_moderators() {
        let pctx = PermissionContext::default();
        let mut p = policy();
        p.audio_locked = true;
        p.video_locked = true;
        let attendee = effective_media(&pctx, &p, Role::Participant, MediaRestrictions::default());
        assert!(!attendee.audio_publish && !attendee.video_publish);
        let host = effective_media(&pctx, &p, Role::Host, MediaRestrictions::default());
        assert!(host.audio_publish && host.video_publish);
        // ...but a participant-specific lock applies even to a co-host.
        let cohost = effective_media(
            &pctx,
            &p,
            Role::CoHost,
            MediaRestrictions {
                audio_locked: true,
                ..Default::default()
            },
        );
        assert!(!cohost.audio_publish);
    }

    #[test]
    fn screen_share_policy_tiers() {
        let pctx = PermissionContext::default();
        let mut p = policy();
        let share = |p: &CollaborationPolicy, r| {
            effective_media(&pctx, p, r, MediaRestrictions::default()).screen_share_publish
        };
        p.screen_share_policy = ScreenSharePolicy::HostOnly;
        assert!(share(&p, Role::Host) && share(&p, Role::CoHost));
        assert!(!share(&p, Role::Presenter) && !share(&p, Role::Participant));
        p.screen_share_policy = ScreenSharePolicy::PresentersOnly;
        assert!(share(&p, Role::Presenter) && share(&p, Role::Moderator));
        assert!(!share(&p, Role::Participant));
        p.screen_share_policy = ScreenSharePolicy::AllInteractive;
        assert!(share(&p, Role::Participant));
        assert!(!share(&p, Role::Viewer), "view-only never shares");
        // Participant-specific lock beats a presenter role grant.
        assert!(
            !effective_media(
                &pctx,
                &p,
                Role::Presenter,
                MediaRestrictions {
                    screen_share_locked: true,
                    ..Default::default()
                }
            )
            .screen_share_publish
        );
    }
}
