//! Reactions and hand raise (Part F §28–§30).
//!
//! - Reactions are TRANSIENT: authorized, policy-bound and rate-limited, then fanned out
//!   over Redis only (`reaction.sent`, no sequence, no replay). No reaction row and no
//!   per-emoji audit is written.
//! - Hand raise is PERSISTENT authoritative state on the participant row
//!   (`hand_raised_at`, ordered oldest first) so it survives reconnects; changes commit
//!   with `hand.raised` / `hand.lowered` in the same transaction.
//! - Disabling hand raise clears every raised hand transactionally (deterministic
//!   semantics, §30).

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde::Serialize;
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::AdmissionState,
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_participants, conference_sessions},
    services::{
        actor::ActorContext,
        meeting_core::{find_scoped_participant, lock_conference, permission_denied, Core},
        meeting_events::{
            action, audit, emit, event, Audience, AuditActor, AuditRecord, OutboxRecord,
        },
        meeting_permissions::{can_moderate, Permission},
    },
};

use super::{
    breakouts::media_context,
    metrics::{inc, COLLAB_METRICS as CM},
    publish_transient, Collab, RateAction,
};

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RaisedHand {
    pub participant_id: Uuid,
    pub display_name: String,
    pub raised_at: chrono::DateTime<Utc>,
}

fn hand_record<'a>(
    conference: &conference_sessions::Model,
    p: &conference_participants::Model,
    event_type: &'a str,
    reason: Option<&str>,
) -> OutboxRecord<'a> {
    OutboxRecord {
        aggregate_type: "conference_participant",
        aggregate_id: p.id,
        event_type,
        meeting_space_id: Some(conference.meeting_space_id),
        conference_id: Some(conference.id),
        payload: json!({
            "participantId": p.id,
            "raisedAt": p.hand_raised_at,
            "reason": reason,
            "collabVersion": p.collab_version,
        }),
    }
}

async fn lower(
    txn: &DatabaseTransaction,
    conference: &conference_sessions::Model,
    p: conference_participants::Model,
    reason: &str,
) -> ApiResult<conference_participants::Model> {
    let now = Utc::now();
    let mut am: conference_participants::ActiveModel = p.clone().into();
    am.hand_raised_at = Set(None);
    am.hand_lowered_at = Set(Some(now));
    am.collab_version = Set(p.collab_version + 1);
    am.updated_at = Set(now);
    let updated = am.update(txn).await?;
    emit(
        txn,
        hand_record(conference, &updated, event::HAND_LOWERED, Some(reason)),
    )
    .await?;
    Ok(updated)
}

/// Clear every raised hand (policy disabled). Caller holds the conference row lock.
pub async fn clear_all_hands(
    txn: &DatabaseTransaction,
    conference: &conference_sessions::Model,
    _actor: &AuditActor,
    reason: &str,
) -> ApiResult<u32> {
    let raised = conference_participants::Entity::find()
        .filter(conference_participants::Column::ConferenceId.eq(conference.id))
        .filter(conference_participants::Column::HandRaisedAt.is_not_null())
        .order_by_asc(conference_participants::Column::Id)
        .all(txn)
        .await?;
    let n = raised.len() as u32;
    for p in raised {
        lower(txn, conference, p, reason).await?;
    }
    Ok(n)
}

pub struct EngagementService {
    core: Core,
}

impl EngagementService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    /// Send a transient reaction. Returns whether it reached the realtime fanout.
    pub async fn react(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        reaction: &str,
    ) -> ApiResult<bool> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.clone();
        c.require(
            Permission::ReactionSend,
            "Your role does not allow reactions",
        )?;
        c.require_live()?;
        if !c.policy.reactions_enabled {
            return Err(ApiError::domain(
                ErrorCode::ReactionsDisabled,
                "Reactions are disabled in this meeting",
            ));
        }
        if !c.policy.reaction_allowed(reaction) {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "This reaction is not available in this meeting",
            ));
        }
        self.core
            .collab_limiter
            .check(RateAction::Reaction, &me.id.to_string())
            .await?;
        // Reactions stay inside the sender's media context (a breakout reaction is
        // never shown in another room).
        let audience = media_context(&self.core.db, &c.conference, &me)
            .await?
            .breakout_id()
            .map(|b| Audience::Breakout { breakout_id: b })
            .unwrap_or(Audience::MainRoom);
        let delivered = publish_transient(
            self.core.redis.as_ref(),
            &c.conference,
            event::REACTION_SENT,
            json!({ "participantId": me.id, "reaction": reaction, "occurredAt": Utc::now() }),
            Some(audience),
        )
        .await;
        if delivered {
            inc(&CM.reactions_sent);
        } else {
            inc(&CM.reactions_dropped);
        }
        Ok(delivered)
    }

    /// Raised hands, oldest first.
    pub async fn raised_hands(&self, conference_id: Uuid) -> ApiResult<Vec<RaisedHand>> {
        Ok(conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference_id))
            .filter(conference_participants::Column::HandRaisedAt.is_not_null())
            .filter(conference_participants::Column::AdmissionState.eq(AdmissionState::Admitted))
            .order_by_asc(conference_participants::Column::HandRaisedAt)
            .order_by_asc(conference_participants::Column::Id)
            .all(&self.core.db)
            .await?
            .into_iter()
            .map(|p| RaisedHand {
                participant_id: p.id,
                display_name: p.display_name,
                raised_at: p.hand_raised_at.unwrap_or_default(),
            })
            .collect())
    }

    /// Raise (true) / lower (false) the caller's own hand. Idempotent.
    pub async fn set_own_hand(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        raised: bool,
    ) -> ApiResult<(conference_participants::Model, bool)> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.id;
        if raised {
            c.require(
                Permission::HandRaise,
                "Your role does not allow raising your hand",
            )?;
            c.require_live()?;
            if !c.policy.hand_raise_enabled {
                return Err(ApiError::domain(
                    ErrorCode::HandRaiseDisabled,
                    "Hand raise is disabled in this meeting",
                ));
            }
        }
        self.core
            .collab_limiter
            .check(RateAction::HandToggle, &me.to_string())
            .await?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let p = find_scoped_participant(&txn, conference_id, me, true).await?;
        if p.hand_raised_at.is_some() == raised {
            txn.commit().await?;
            return Ok((p, false));
        }
        let updated = if raised {
            // Re-check under the lock: policy may have been disabled concurrently.
            let policy = super::effective_policy(
                &crate::services::meeting_core::find_space(&txn, conference.meeting_space_id)
                    .await?,
                &conference,
            );
            if !policy.hand_raise_enabled {
                return Err(ApiError::domain(
                    ErrorCode::HandRaiseDisabled,
                    "Hand raise is disabled in this meeting",
                ));
            }
            let now = Utc::now();
            let mut am: conference_participants::ActiveModel = p.clone().into();
            am.hand_raised_at = Set(Some(now));
            am.hand_lowered_at = Set(None);
            am.collab_version = Set(p.collab_version + 1);
            am.updated_at = Set(now);
            let updated = am.update(&txn).await?;
            emit(
                &txn,
                hand_record(&conference, &updated, event::HAND_RAISED, None),
            )
            .await?;
            inc(&CM.hand_raises);
            updated
        } else {
            lower(&txn, &conference, p, "SELF").await?
        };
        txn.commit().await?;
        Ok((updated, true))
    }

    /// Host/moderator lowers another participant's hand. Idempotent.
    pub async fn lower_other(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
    ) -> ApiResult<bool> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let target =
            find_scoped_participant(&self.core.db, conference_id, participant_id, false).await?;
        if c.participant.as_ref().map(|p| p.id) == Some(target.id) {
            return self
                .set_own_hand(ctx, conference_id, false)
                .await
                .map(|(_, ch)| ch);
        }
        c.require(
            Permission::HandLowerOther,
            "Your role does not allow lowering hands",
        )?;
        if !can_moderate(c.role, target.role) {
            return Err(permission_denied(
                "You can only lower hands of less privileged participants",
            ));
        }
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let p = find_scoped_participant(&txn, conference_id, participant_id, true).await?;
        if p.hand_raised_at.is_none() {
            txn.commit().await?;
            return Ok(false);
        }
        let updated = lower(&txn, &conference, p, "LOWERED_BY_HOST").await?;
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::HAND_LOWER_OTHER,
                target_type: Some("PARTICIPANT"),
                target_ref: Some(updated.id.to_string()),
                metadata: json!({}),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::services::collaboration::policy::{valid_reaction_key, CollaborationPolicy};

    #[test]
    fn reaction_keys_are_bounded_tokens() {
        assert!(valid_reaction_key("thumbs_up"));
        assert!(!valid_reaction_key("thumbs up"));
        assert!(!valid_reaction_key(&"a".repeat(33)));
        let p = CollaborationPolicy::defaults(crate::domain::MeetingType::Standard);
        for r in [
            "thumbs_up",
            "clap",
            "heart",
            "laugh",
            "surprised",
            "celebrate",
        ] {
            assert!(p.reaction_allowed(r), "{r}");
        }
    }
}
