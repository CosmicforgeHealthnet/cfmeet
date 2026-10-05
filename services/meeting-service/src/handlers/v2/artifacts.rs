//! Part G transport: recording, artefacts, consent, transcription, intelligence.
//! Every handler resolves identity server-side and delegates to the domain services;
//! no handler mutates artefact tables directly.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{
    dto::v2::*,
    error::{ApiError, ApiResult, ErrorCode},
    services::{actor::ActorContext, collaboration::policy::ConsentKind},
    state::AppState,
};

// ----------------------------- recordings ---------------------------------

/// Start the composite recording (provider called only after REQUESTED commits).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/recordings", tag = "Recordings (v2)", operation_id = "v2_start_recording",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = StartRecordingRequest,
    responses(
        (status = 202, description = "REQUESTED (capture evidence follows)", body = RecordingResponse),
        (status = 200, description = "Idempotent repeat", body = RecordingResponse),
        (status = 403, description = "PERMISSION_DENIED / RECORDING_NOT_ALLOWED / CONSENT_REQUIRED / CONSENT_DECLINED", body = ErrorEnvelope),
        (status = 404, description = "CONFERENCE_NOT_FOUND / RECORDING_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "RECORDING_ALREADY_ACTIVE / CONFERENCE_NOT_LIVE", body = ErrorEnvelope),
        (status = 503, description = "RECORDING_PROVIDER_UNAVAILABLE / ARTIFACT_STORAGE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn start_recording(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<StartRecordingRequest>,
) -> ApiResult<(StatusCode, Json<RecordingResponse>)> {
    let r = state.meeting.recordings.start(&actor, id, &req).await?;
    let status = if r.created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(r)))
}

/// Recordings of a conference.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/recordings", tag = "Recordings (v2)", operation_id = "v2_list_recordings",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Recordings", body = RecordingListResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn list_recordings(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<RecordingListResponse>> {
    Ok(Json(RecordingListResponse {
        recordings: state.meeting.recordings.list(&actor, id).await?,
    }))
}

/// One recording (scoped to the conference).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/recordings/{recordingId}", tag = "Recordings (v2)", operation_id = "v2_get_recording",
    params(("id" = Uuid, Path, description = "Conference id"), ("recordingId" = Uuid, Path, description = "Recording id")),
    responses((status = 200, description = "Recording", body = ArtifactView), (status = 404, description = "RECORDING_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn get_recording(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, rid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ArtifactView>> {
    Ok(Json(state.meeting.recordings.get(&actor, id, rid).await?))
}

/// Stop a recording (idempotent; READY only after provider finalization + storage
/// verification).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/recordings/{recordingId}/stop", tag = "Recordings (v2)", operation_id = "v2_stop_recording",
    params(("id" = Uuid, Path, description = "Conference id"), ("recordingId" = Uuid, Path, description = "Recording id")),
    responses(
        (status = 200, description = "STOPPING (or already stopping / processing / ready)", body = RecordingResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "RECORDING_NOT_FOUND", body = ErrorEnvelope),
        (status = 409, description = "RECORDING_NOT_ACTIVE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn stop_recording(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, rid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<RecordingResponse>> {
    Ok(Json(state.meeting.recordings.stop(&actor, id, rid).await?))
}

// ------------------------------ artefacts ---------------------------------

/// Artefacts of a conference (historical access still requires authorization).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/artifacts", tag = "Artifacts (v2)", operation_id = "v2_list_artifacts",
    params(("id" = Uuid, Path, description = "Conference id"), ArtifactListQuery),
    responses((status = 200, description = "Artifacts", body = ArtifactListResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn list_artifacts(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    Query(q): Query<ArtifactListQuery>,
) -> ApiResult<Json<ArtifactListResponse>> {
    Ok(Json(ArtifactListResponse {
        artifacts: state
            .meeting
            .artifacts
            .list(&actor, id, q.artifact_type)
            .await?,
    }))
}

/// One artefact (conference id + artefact id; knowing an id is not authorization).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/artifacts/{artifactId}", tag = "Artifacts (v2)", operation_id = "v2_get_artifact",
    params(("id" = Uuid, Path, description = "Conference id"), ("artifactId" = Uuid, Path, description = "Artifact id")),
    responses((status = 200, description = "Artifact", body = ArtifactView), (status = 403, description = "ARTIFACT_ACCESS_DENIED", body = ErrorEnvelope), (status = 404, description = "ARTIFACT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn get_artifact(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, aid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ArtifactView>> {
    Ok(Json(state.meeting.artifacts.get(&actor, id, aid).await?))
}

/// Mint a short-lived, read-only, single-object signed URL (never stored).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/artifacts/{artifactId}/access", tag = "Artifacts (v2)", operation_id = "v2_artifact_access",
    params(("id" = Uuid, Path, description = "Conference id"), ("artifactId" = Uuid, Path, description = "Artifact id")),
    responses(
        (status = 200, description = "Signed access", body = ArtifactAccess),
        (status = 403, description = "ARTIFACT_ACCESS_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ARTIFACT_NOT_FOUND", body = ErrorEnvelope),
        (status = 409, description = "ARTIFACT_NOT_READY", body = ErrorEnvelope),
        (status = 410, description = "ARTIFACT_DELETED", body = ErrorEnvelope),
        (status = 503, description = "ARTIFACT_STORAGE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn artifact_access(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, aid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ArtifactAccess>> {
    Ok(Json(state.meeting.artifacts.access(&actor, id, aid).await?))
}

/// Delete an artefact (DELETED only after the object's absence is confirmed).
#[utoipa::path(
    delete, path = "/api/v2/conferences/{id}/artifacts/{artifactId}", tag = "Artifacts (v2)", operation_id = "v2_delete_artifact",
    params(("id" = Uuid, Path, description = "Conference id"), ("artifactId" = Uuid, Path, description = "Artifact id")),
    responses(
        (status = 200, description = "DELETED or deletion pending (retried)", body = ArtifactView),
        (status = 403, description = "PERMISSION_DENIED / PROFILE_POLICY_DENIED", body = ErrorEnvelope),
        (status = 409, description = "RECORDING_STATE_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn delete_artifact(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, aid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ArtifactView>> {
    Ok(Json(state.meeting.artifacts.delete(&actor, id, aid).await?))
}

/// Generate an attendance report, poll report or chat export (policy-gated).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/artifacts/reports", tag = "Artifacts (v2)", operation_id = "v2_generate_report",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = GenerateReportRequest,
    responses(
        (status = 201, description = "READY report", body = ArtifactView),
        (status = 403, description = "PERMISSION_DENIED / PROFILE_POLICY_DENIED", body = ErrorEnvelope),
        (status = 503, description = "ARTIFACT_STORAGE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn generate_report(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<GenerateReportRequest>,
) -> ApiResult<(StatusCode, Json<ArtifactView>)> {
    Ok((
        StatusCode::CREATED,
        Json(
            state
                .meeting
                .artifacts
                .generate_report(&actor, id, &req)
                .await?,
        ),
    ))
}

// ------------------------------- consent ----------------------------------

/// The caller's consent requirements (independent types) and current decisions.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/consents/requirements", tag = "Consent (v2)", operation_id = "v2_consent_requirements",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Requirements", body = ConsentRequirementsResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn consent_requirements(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ConsentRequirementsResponse>> {
    Ok(Json(state.meeting.consents.requirements(&actor, id).await?))
}

/// Record the caller's OWN consent decisions (no participant field exists).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/consents", tag = "Consent (v2)", operation_id = "v2_record_consents",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = RecordConsentRequest,
    responses(
        (status = 200, description = "Recorded", body = ConsentRequirementsResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 409, description = "CONSENT_POLICY_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn record_consents(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<RecordConsentRequest>,
) -> ApiResult<Json<ConsentRequirementsResponse>> {
    Ok(Json(state.meeting.consents.record(&actor, id, &req).await?))
}

fn consent_kind(raw: &str) -> ApiResult<ConsentKind> {
    serde_json::from_value(serde_json::Value::String(raw.to_ascii_uppercase()))
        .map_err(|_| ApiError::domain(ErrorCode::ValidationError, "unknown consent type"))
}

/// Withdraw one consent type (effect follows the profile's withdrawal mode).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/consents/{consentType}/withdraw", tag = "Consent (v2)", operation_id = "v2_withdraw_consent",
    params(("id" = Uuid, Path, description = "Conference id"), ("consentType" = String, Path, description = "RECORDING / TRANSCRIPTION / AI_PROCESSING / DATA_SHARING")),
    responses((status = 200, description = "Withdrawn", body = WithdrawalResponse), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn withdraw_consent(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, kind)): V2Path<(Uuid, String)>,
) -> ApiResult<Json<WithdrawalResponse>> {
    let kind = consent_kind(&kind)?;
    Ok(Json(
        state.meeting.consents.withdraw(&actor, id, kind).await?,
    ))
}

// ---------------------------- transcription -------------------------------

/// Start LIVE or POST_MEETING transcription (policy, location and consent gated).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/transcription", tag = "Transcription (v2)", operation_id = "v2_start_transcription",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = StartTranscriptionRequest,
    responses(
        (status = 202, description = "Job created", body = TranscriptionJobView),
        (status = 403, description = "TRANSCRIPTION_NOT_ALLOWED / CONSENT_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "TRANSCRIPTION_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "TRANSCRIPTION_PROVIDER_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn start_transcription(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<StartTranscriptionRequest>,
) -> ApiResult<(StatusCode, Json<TranscriptionJobView>)> {
    Ok((
        StatusCode::ACCEPTED,
        Json(state.meeting.transcription.start(&actor, id, &req).await?),
    ))
}

/// Stop a transcription job.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/transcription/{jobId}/stop", tag = "Transcription (v2)", operation_id = "v2_stop_transcription",
    params(("id" = Uuid, Path, description = "Conference id"), ("jobId" = Uuid, Path, description = "Job id")),
    responses((status = 200, description = "Stopping", body = TranscriptionJobView), (status = 404, description = "TRANSCRIPTION_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn stop_transcription(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, jid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<TranscriptionJobView>> {
    Ok(Json(
        state.meeting.transcription.stop(&actor, id, jid).await?,
    ))
}

/// Transcription jobs / transcripts of a conference.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/transcripts", tag = "Transcription (v2)", operation_id = "v2_list_transcripts",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Jobs", body = TranscriptionJobListResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn list_transcripts(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<TranscriptionJobListResponse>> {
    Ok(Json(TranscriptionJobListResponse {
        jobs: state.meeting.transcription.list(&actor, id).await?,
    }))
}

/// Transcript FINAL segments (cursor paginated).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/transcripts/{artifactId}", tag = "Transcription (v2)", operation_id = "v2_get_transcript",
    params(("id" = Uuid, Path, description = "Conference id"), ("artifactId" = Uuid, Path, description = "Transcript artifact id"), TranscriptQuery),
    responses((status = 200, description = "Transcript", body = TranscriptView), (status = 404, description = "ARTIFACT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn get_transcript(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, aid)): V2Path<(Uuid, Uuid)>,
    Query(q): Query<TranscriptQuery>,
) -> ApiResult<Json<TranscriptView>> {
    Ok(Json(
        state
            .meeting
            .transcription
            .get(&actor, id, aid, q.after, q.limit)
            .await?,
    ))
}

/// Translate a READY transcript (downstream; the original is never overwritten).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/transcripts/{artifactId}/translations", tag = "Transcription (v2)", operation_id = "v2_translate_transcript",
    params(("id" = Uuid, Path, description = "Conference id"), ("artifactId" = Uuid, Path, description = "Transcript artifact id")),
    request_body = TranslateRequest,
    responses((status = 200, description = "Translated", body = TranslationResponse), (status = 503, description = "TRANSCRIPTION_PROVIDER_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn translate_transcript(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, aid)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<TranslateRequest>,
) -> ApiResult<Json<TranslationResponse>> {
    Ok(Json(
        state
            .meeting
            .transcription
            .translate(&actor, id, aid, &req)
            .await?,
    ))
}

// ----------------------------- intelligence -------------------------------

/// Request an intelligence output (disabled by default; policy / consent gated).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/intelligence/requests", tag = "Intelligence (v2)", operation_id = "v2_request_intelligence",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = IntelligenceRequest,
    responses(
        (status = 201, description = "MEETING_SUMMARY artifact", body = ArtifactView),
        (status = 403, description = "INTELLIGENCE_NOT_ALLOWED / CONSENT_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "INTELLIGENCE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "INTELLIGENCE_PROVIDER_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn request_intelligence(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<IntelligenceRequest>,
) -> ApiResult<(StatusCode, Json<ArtifactView>)> {
    Ok((
        StatusCode::CREATED,
        Json(state.meeting.intelligence.request(&actor, id, &req).await?),
    ))
}
