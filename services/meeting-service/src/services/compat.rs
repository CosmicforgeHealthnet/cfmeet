//! Compatibility layer (spec §A9, §B15 steps 5–6).
//!
//! - **Resolver**: legacy meeting id / legacy code / v2 UUID / v2 meeting code →
//!   canonical MeetingSpace (+ canonical ConferenceSession). Resolution never implies
//!   authorization, never exposes provider room identifiers, and is deterministic:
//!   v2 ids win over legacy ids, and v2/legacy code formats are disjoint.
//! - **Lazy backfill**: a legacy meeting created after the migration is mapped on first
//!   resolution (same idempotent SQL as the migration).
//! - **Shadow comparison**: compares legacy and v2 state without affecting consumers.

use chrono::Utc;
use migration::phase2_backfill::{self, BackfillReport};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{
    domain::{AccessPolicy, AdmissionState, ConferenceStatus, RecordOrigin, Role},
    error::ApiResult,
    models::{
        conference_participants, conference_sessions, meeting_spaces,
        meetings::{self, MeetingStatus},
        participants::{self, ParticipantRole, ParticipantStatus},
    },
    services::{
        actor::ActorContext,
        meeting_core::{not_found_space, permission_denied, space_is_visible, Core},
        secrets,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedBy {
    V2Id,
    V2Code,
    LegacyId,
    LegacyCode,
}

impl MatchedBy {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchedBy::V2Id => "V2_ID",
            MatchedBy::V2Code => "V2_CODE",
            MatchedBy::LegacyId => "LEGACY_ID",
            MatchedBy::LegacyCode => "LEGACY_CODE",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub space: meeting_spaces::Model,
    pub matched_by: MatchedBy,
}

pub struct CompatService {
    core: Core,
}

impl CompatService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    fn visible(space: Option<meeting_spaces::Model>) -> Option<meeting_spaces::Model> {
        space.filter(space_is_visible)
    }

    async fn by_legacy_id(&self, legacy_id: Uuid) -> ApiResult<Option<meeting_spaces::Model>> {
        let mapped = meeting_spaces::Entity::find()
            .filter(meeting_spaces::Column::LegacyMeetingId.eq(legacy_id))
            .one(&self.core.db)
            .await?;
        if mapped.is_some() {
            return Ok(mapped);
        }
        // Legacy meeting created after the migration: map it now (idempotent).
        if meetings::Entity::find_by_id(legacy_id)
            .one(&self.core.db)
            .await?
            .is_none()
        {
            return Ok(None);
        }
        self.backfill(Some(legacy_id)).await?;
        Ok(meeting_spaces::Entity::find()
            .filter(meeting_spaces::Column::LegacyMeetingId.eq(legacy_id))
            .one(&self.core.db)
            .await?)
    }

    /// Resolve any supported identifier to the canonical MeetingSpace.
    pub async fn resolve(&self, identifier: &str) -> ApiResult<Resolution> {
        let identifier = identifier.trim();
        if let Ok(id) = Uuid::parse_str(identifier) {
            if let Some(space) = Self::visible(
                meeting_spaces::Entity::find_by_id(id)
                    .one(&self.core.db)
                    .await?,
            ) {
                return Ok(Resolution {
                    space,
                    matched_by: MatchedBy::V2Id,
                });
            }
            if let Some(space) = Self::visible(self.by_legacy_id(id).await?) {
                return Ok(Resolution {
                    space,
                    matched_by: MatchedBy::LegacyId,
                });
            }
            return Err(not_found_space());
        }
        let lower = identifier.to_ascii_lowercase();
        if secrets::is_v2_meeting_code(&lower) {
            let space = meeting_spaces::Entity::find()
                .filter(meeting_spaces::Column::MeetingCode.eq(lower))
                .one(&self.core.db)
                .await?;
            return Self::visible(space)
                .map(|space| Resolution {
                    space,
                    matched_by: MatchedBy::V2Code,
                })
                .ok_or_else(not_found_space);
        }
        let upper = identifier.to_ascii_uppercase();
        if secrets::is_legacy_meeting_code(&upper) {
            let legacy = meetings::Entity::find()
                .filter(meetings::Column::MeetingIdentifier.eq(upper))
                .one(&self.core.db)
                .await?
                .ok_or_else(not_found_space)?;
            return Self::visible(self.by_legacy_id(legacy.id).await?)
                .map(|space| Resolution {
                    space,
                    matched_by: MatchedBy::LegacyCode,
                })
                .ok_or_else(not_found_space);
        }
        Err(not_found_space())
    }

    /// Canonical conference: the LIVE one, else the next upcoming unstarted one, else the latest.
    pub async fn canonical_conference(
        &self,
        space_id: Uuid,
    ) -> ApiResult<Option<conference_sessions::Model>> {
        let all = conference_sessions::Entity::find()
            .filter(conference_sessions::Column::MeetingSpaceId.eq(space_id))
            .order_by_asc(conference_sessions::Column::ScheduledStartAt)
            .all(&self.core.db)
            .await?;
        if let Some(live) = all
            .iter()
            .filter(|c| c.status == ConferenceStatus::Live)
            .max_by_key(|c| (c.origin == RecordOrigin::V2, c.started_at))
        {
            return Ok(Some(live.clone()));
        }
        let now = Utc::now();
        if let Some(next) = all.iter().find(|c| {
            matches!(
                c.status,
                ConferenceStatus::Created | ConferenceStatus::Preparing | ConferenceStatus::Waiting
            ) && c.scheduled_end_at.map(|e| e > now).unwrap_or(true)
        }) {
            return Ok(Some(next.clone()));
        }
        Ok(all.into_iter().max_by_key(|c| c.created_at))
    }

    /// Run the idempotent backfill (all legacy meetings, or one).
    pub async fn backfill(&self, legacy_meeting_id: Option<Uuid>) -> ApiResult<BackfillReport> {
        let txn = self.core.db.begin().await?;
        let report = phase2_backfill::run(&txn, legacy_meeting_id).await?;
        txn.commit().await?;
        Ok(report)
    }

    pub fn require_platform_admin(ctx: &ActorContext) -> ApiResult<()> {
        match ctx.registered() {
            Some(r) if r.platform_admin => Ok(()),
            _ => Err(permission_denied("Platform administrator role required")),
        }
    }

    /// Compare legacy and v2 state for one legacy meeting. Never mutates anything.
    pub async fn shadow_compare(
        &self,
        legacy_meeting_id: Uuid,
    ) -> ApiResult<(Option<Uuid>, Vec<String>)> {
        let mut diffs = Vec::new();
        let legacy = meetings::Entity::find_by_id(legacy_meeting_id)
            .one(&self.core.db)
            .await?;
        let space = meeting_spaces::Entity::find()
            .filter(meeting_spaces::Column::LegacyMeetingId.eq(legacy_meeting_id))
            .one(&self.core.db)
            .await?;
        let Some(legacy) = legacy else {
            if space.is_some() {
                diffs.push("legacy meeting missing but v2 mapping exists".into());
            }
            return Ok((space.map(|s| s.id), diffs));
        };
        let Some(space) = space else {
            diffs.push("no v2 meeting space mapped".into());
            return Ok((None, diffs));
        };
        if space.meeting_code != legacy.meeting_identifier {
            diffs.push(format!(
                "meeting_code {} != legacy {}",
                space.meeting_code, legacy.meeting_identifier
            ));
        }
        if space.owner_user_id != Some(legacy.host_id) {
            diffs.push("owner differs from legacy host".into());
        }
        if space.title != legacy.title {
            diffs.push("title differs".into());
        }
        let expected_access = if legacy.is_private {
            AccessPolicy::Authenticated
        } else {
            AccessPolicy::Public
        };
        if space.access_policy != expected_access {
            diffs.push(format!(
                "access_policy {} != legacy-derived {}",
                space.access_policy, expected_access
            ));
        }
        let conference = conference_sessions::Entity::find()
            .filter(conference_sessions::Column::LegacyMeetingId.eq(legacy_meeting_id))
            .one(&self.core.db)
            .await?;
        let Some(conference) = conference else {
            diffs.push("no legacy conference session mapped".into());
            return Ok((Some(space.id), diffs));
        };
        let expected_status = match legacy.status {
            MeetingStatus::Scheduled => ConferenceStatus::Created,
            MeetingStatus::Ongoing => ConferenceStatus::Live,
            MeetingStatus::Ended => ConferenceStatus::Ended,
            MeetingStatus::Cancelled => ConferenceStatus::Cancelled,
        };
        if conference.status != expected_status {
            diffs.push(format!(
                "conference status {} != legacy-derived {}",
                conference.status, expected_status
            ));
        }
        let legacy_participants = participants::Entity::find()
            .filter(participants::Column::MeetingId.eq(legacy_meeting_id))
            .order_by_asc(participants::Column::JoinTime)
            .all(&self.core.db)
            .await?;
        let v2_participants = conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference.id))
            .all(&self.core.db)
            .await?;
        // Latest legacy row per registered user decides expected state.
        let mut latest: HashMap<Uuid, &participants::Model> = HashMap::new();
        for p in &legacy_participants {
            if let Some(u) = p.user_id {
                latest.insert(u, p);
            }
        }
        for (user, p) in &latest {
            let expected = match p.status {
                ParticipantStatus::Waiting => AdmissionState::Waiting,
                ParticipantStatus::Joined => AdmissionState::Admitted,
                ParticipantStatus::Kicked => AdmissionState::Removed,
            };
            // Same mapping as the backfill (`PARTICIPANT_ROLE`).
            let expected_role = match p.role {
                ParticipantRole::Host if Some(legacy.host_id) == p.user_id => Role::Owner,
                ParticipantRole::Host => Role::Host,
                ParticipantRole::Viewer => Role::Viewer,
                ParticipantRole::Participant => Role::Participant,
            };
            match v2_participants.iter().find(|v| v.user_id == Some(*user)) {
                None => diffs.push(format!("registered user {user} missing in v2")),
                Some(v) => {
                    if v.admission_state != expected {
                        diffs.push(format!(
                            "user {user}: v2 {} != legacy-derived {}",
                            v.admission_state, expected
                        ));
                    }
                    if v.role != expected_role {
                        diffs.push(format!(
                            "user {user}: v2 role {} != legacy-derived role {}",
                            v.role, expected_role
                        ));
                    }
                }
            }
        }
        let legacy_guests = legacy_participants
            .iter()
            .filter(|p| p.user_id.is_none())
            .count();
        let v2_guests = v2_participants
            .iter()
            .filter(|p| p.legacy_participant_id.is_some())
            .count();
        if legacy_guests != v2_guests {
            diffs.push(format!(
                "guest participants: legacy {legacy_guests} != v2 {v2_guests}"
            ));
        }
        Ok((Some(space.id), diffs))
    }

    /// Shadow-read hook for legacy reads: logs drift and counts it (operations metrics
    /// `shadow_reads` / `shadow_mismatches` / `shadow_failures`); never changes the
    /// response and never reaches the client.
    pub async fn shadow_log(&self, legacy_meeting_id: Uuid) {
        use crate::services::ops_metrics::{inc, OPS_METRICS as OM};
        inc(&OM.shadow_reads);
        match self.shadow_compare(legacy_meeting_id).await {
            Ok((_, diffs)) if diffs.is_empty() => {
                tracing::debug!(%legacy_meeting_id, "meeting v2 shadow read consistent")
            }
            Ok((space, diffs)) => {
                inc(&OM.shadow_mismatches);
                tracing::warn!(%legacy_meeting_id, ?space, ?diffs, "meeting v2 shadow read drift")
            }
            Err(e) => {
                inc(&OM.shadow_failures);
                tracing::warn!(%legacy_meeting_id, "meeting v2 shadow read failed: {e}")
            }
        }
    }
}
