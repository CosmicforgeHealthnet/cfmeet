//! Migration/compatibility operations (platform administrators only).

use axum::{extract::State, Json};
use uuid::Uuid;

use super::V2Path;
use crate::{
    dto::v2::{BackfillReportResponse, ShadowComparisonResponse},
    error::ApiResult,
    services::{actor::ActorContext, compat::CompatService},
    state::AppState,
};

/// Compare legacy and v2 state for one legacy meeting (no mutation).
#[utoipa::path(
    get, path = "/api/v2/admin/compat/legacy-meetings/{id}/shadow", tag = "Compatibility (v2)", operation_id = "v2_shadow_compare",
    params(("id" = Uuid, Path, description = "Legacy meeting id")),
    responses((status = 200, description = "Comparison", body = ShadowComparisonResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn shadow_compare(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ShadowComparisonResponse>> {
    CompatService::require_platform_admin(&actor)?;
    let (space, differences) = state.meeting.compat.shadow_compare(id).await?;
    Ok(Json(ShadowComparisonResponse {
        legacy_meeting_id: id,
        meeting_space_id: space,
        consistent: differences.is_empty(),
        differences,
    }))
}

/// Run the idempotent legacy → v2 backfill and return migration evidence.
#[utoipa::path(
    post, path = "/api/v2/admin/compat/backfill", tag = "Compatibility (v2)", operation_id = "v2_run_backfill",
    responses((status = 200, description = "Backfill report", body = BackfillReportResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn run_backfill(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<BackfillReportResponse>> {
    CompatService::require_platform_admin(&actor)?;
    let r = state.meeting.compat.backfill(None).await?;
    Ok(Json(BackfillReportResponse {
        legacy_meeting_rows: r.legacy_meeting_rows,
        meeting_space_rows: r.meeting_space_rows,
        mapped_spaces: r.mapped_spaces,
        mapping_failures: r.mapping_failures,
        legacy_conference_sessions: r.legacy_conference_sessions,
        legacy_participant_rows: r.legacy_participant_rows,
        mapped_legacy_participant_rows: r.mapped_legacy_participant_rows,
        conference_participant_rows_from_legacy: r.conference_participant_rows_from_legacy,
        participant_session_rows_from_legacy: r.participant_session_rows_from_legacy,
        unmapped_meeting_ids: r.unmapped_meeting_ids,
        unmapped_participant_ids: r.unmapped_participant_ids,
        inserted_spaces: r.inserted_spaces,
        inserted_conferences: r.inserted_conferences,
        inserted_participants: r.inserted_participants,
        inserted_sessions: r.inserted_sessions,
    }))
}
