//! IntelligenceService — intelligence readiness (Part G §34–§36).
//!
//! Production AI summarization is not part of Phase 2: the default provider is
//! unconfigured and `FEATURE_INTELLIGENCE_V2_ENABLED` defaults to off. What exists is
//! the gate every future dispatch must pass, in this order, with ZERO provider calls
//! when any gate fails:
//!
//! 1. feature flag enabled;
//! 2. `intelligence.request` permission;
//! 3. profile / organization location policy permits the provider's location
//!    (`EXTERNAL_ALLOWED` / `SELF_HOSTED_ONLY` / `DISABLED`);
//! 4. AI-processing consent satisfied (explicit when policy requires it);
//! 5. source artefact authorized (`transcript.read`), READY, not pending deletion.
//!
//! Outputs are MEETING_SUMMARY artefacts (with source references) in object storage.

use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use super::{
    not_found_artifact,
    providers::{location_permitted, IntelligenceInput},
    service::ArtifactService,
    status_of, ArtifactCtx, ArtifactStatus, ArtifactType, ArtifactView,
};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    models::{meeting_artifacts, transcript_segments, transcription_jobs},
    services::{
        actor::ActorContext,
        collaboration::policy::ConsentKind,
        meeting_core::Core,
        meeting_events::{action, audit, AuditRecord},
        meeting_permissions::Permission,
        ops_metrics::{inc, OPS_METRICS as OM},
        profiles::ProcessingLocation,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IntelligenceKind {
    Summary,
    ActionItems,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntelligenceRequest {
    /// READY transcript artefact of this conference.
    pub source_artifact_id: Uuid,
    pub kind: IntelligenceKind,
}

pub struct IntelligenceService {
    core: Core,
}

fn not_allowed(message: &str) -> ApiError {
    inc(&OM.intelligence_blocked);
    ApiError::domain(ErrorCode::IntelligenceNotAllowed, message)
}

impl IntelligenceService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    pub async fn request(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: &IntelligenceRequest,
    ) -> ApiResult<ArtifactView> {
        if !self.core.flags.intelligence_v2_enabled {
            inc(&OM.intelligence_blocked);
            return Err(ApiError::domain(
                ErrorCode::IntelligenceDisabled,
                "Intelligence processing is not enabled",
            ));
        }
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::IntelligenceRequest,
            "You are not allowed to request intelligence outputs",
        )?;
        let provider = self.core.artifacts.intelligence.clone();
        let caps = provider.capabilities();
        let policy = c.profile.policy.intelligence.location;
        if policy == ProcessingLocation::Disabled {
            return Err(not_allowed("Intelligence processing is disabled by policy"));
        }
        if caps.location.is_some() && !location_permitted(policy, caps.location) {
            return Err(not_allowed(
                "The configured intelligence provider is not permitted by policy",
            ));
        }
        let supported = match req.kind {
            IntelligenceKind::Summary => caps.summarize,
            IntelligenceKind::ActionItems => caps.action_items,
        };
        if caps.location.is_none() || !supported {
            return Err(ApiError::domain(
                ErrorCode::IntelligenceProviderUnavailable,
                "Intelligence processing is unavailable",
            ));
        }
        super::consent::ConsentService::new(self.core.clone())
            .require_for_post_meeting(&c, ConsentKind::AiProcessing)
            .await?;
        c.require(
            Permission::TranscriptRead,
            "You are not allowed to read the source transcript",
        )?;
        let source = meeting_artifacts::Entity::find_by_id(req.source_artifact_id)
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
            .filter(meeting_artifacts::Column::ArtifactType.eq("TRANSCRIPT"))
            .one(&self.core.db)
            .await?
            .ok_or_else(not_found_artifact)?;
        if status_of(&source) != ArtifactStatus::Ready || source.delete_requested_at.is_some() {
            return Err(ApiError::domain(
                ErrorCode::ArtifactNotReady,
                "The source transcript is not ready",
            ));
        }
        let job = transcription_jobs::Entity::find()
            .filter(transcription_jobs::Column::ArtifactId.eq(source.id))
            .filter(transcription_jobs::Column::ConferenceId.eq(conference_id))
            .one(&self.core.db)
            .await?
            .ok_or_else(not_found_artifact)?;
        let segments = transcript_segments::Entity::find()
            .filter(transcript_segments::Column::JobId.eq(job.id))
            .order_by_asc(transcript_segments::Column::Sequence)
            .all(&self.core.db)
            .await?;
        // Speakers are opaque labels — never names or contact details.
        let mut labels: Vec<Uuid> = vec![];
        let lines = segments
            .iter()
            .map(|s| {
                let label = match s.speaker_participant_id {
                    Some(p) => {
                        let idx = labels.iter().position(|x| *x == p).unwrap_or_else(|| {
                            labels.push(p);
                            labels.len() - 1
                        });
                        format!("Speaker {}", idx + 1)
                    }
                    None => "Speaker".to_string(),
                };
                (label, s.text.clone())
            })
            .collect();
        let input = IntelligenceInput {
            conference_id,
            language: job.language.clone(),
            lines,
        };
        audit(
            &self.core.db,
            &c.audit,
            AuditRecord {
                meeting_space_id: c.space.id,
                conference_id: Some(conference_id),
                action: action::INTELLIGENCE_REQUEST,
                target_type: Some("ARTIFACT"),
                target_ref: Some(source.id.to_string()),
                metadata: json!({ "kind": req.kind, "provider": provider.name() }),
            },
        )
        .await?;
        inc(&OM.intelligence_dispatched);
        let output = match req.kind {
            IntelligenceKind::Summary => provider
                .summarize(&input)
                .await
                .map(|s| json!({ "summary": s })),
            IntelligenceKind::ActionItems => provider
                .extract_action_items(&input)
                .await
                .map(|items| json!({ "actionItems": items })),
        }
        .map_err(|e| {
            inc(&OM.intelligence_failed);
            tracing::warn!(conference_id = %conference_id, "intelligence provider failed: {e}");
            ApiError::domain(
                ErrorCode::IntelligenceProviderUnavailable,
                "Intelligence processing is unavailable",
            )
        })?;
        let body = json!({
            "conferenceId": conference_id,
            "kind": req.kind,
            "sourceArtifactId": source.id,
            "output": output,
        });
        let artifact = ArtifactService::new(self.core.clone())
            .store_generated(
                &c.conference,
                ArtifactType::MeetingSummary,
                serde_json::to_vec(&body).map_err(|e| ApiError::InternalError(e.to_string()))?,
                "application/json",
                json!({
                    "kind": req.kind,
                    "sourceTranscriptId": source.id,
                    "sourceRecordingId": source.source_artifact_id,
                    "provider": provider.name(),
                    "location": caps.location,
                }),
                Some(source.id),
                c.audit.actor_ref.clone(),
                None,
            )
            .await?;
        Ok(ArtifactView::of(&artifact))
    }
}
