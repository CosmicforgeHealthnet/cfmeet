//! Part I transport: participant quality samples and scoped conference quality.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// Submit a batch of technical quality samples for the caller's own session.
/// Identity, conference and session are server-derived; bodies are size- and
/// rate-limited and contain no free text.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/quality/samples", tag = "Quality (v2)", operation_id = "v2_submit_quality_samples",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = QualitySampleBatch,
    responses(
        (status = 202, description = "Accepted", body = QualityIngestResponse),
        (status = 400, description = "QUALITY_SAMPLE_INVALID / VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED (no active session)", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 413, description = "Body too large", body = ErrorEnvelope),
        (status = 429, description = "QUALITY_RATE_LIMITED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn submit_samples(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(batch): V2Json<QualitySampleBatch>,
) -> ApiResult<(StatusCode, Json<QualityIngestResponse>)> {
    let r = state.meeting.quality.ingest(&actor, id, &batch).await?;
    Ok((StatusCode::ACCEPTED, Json(r)))
}

/// Conference quality summary for holders of `quality.view` (no infrastructure
/// identifiers).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/quality", tag = "Quality (v2)", operation_id = "v2_conference_quality",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "Quality summary", body = ConferenceQualityView),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn conference_quality(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ConferenceQualityView>> {
    Ok(Json(
        state.meeting.quality.conference_quality(&actor, id).await?,
    ))
}
