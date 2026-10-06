//! Part F transport: polls / quizzes and Q&A.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{
    dto::v2::*,
    error::ApiResult,
    services::{actor::ActorContext, collaboration::polls::PollStatus},
    state::AppState,
};

/// Create a poll or quiz (DRAFT).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/polls", tag = "Polls (v2)", operation_id = "v2_create_poll",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = CreatePollRequest,
    responses(
        (status = 201, description = "Created", body = PollView),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / POLLS_DISABLED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn create_poll(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<CreatePollRequest>,
) -> ApiResult<(StatusCode, Json<PollView>)> {
    Ok((
        StatusCode::CREATED,
        Json(state.meeting.polls.create(&actor, id, req).await?),
    ))
}

/// Polls of a conference (drafts for poll managers only; quiz answers protected).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/polls", tag = "Polls (v2)", operation_id = "v2_list_polls",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Polls", body = PollListResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn list_polls(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<PollListResponse>> {
    Ok(Json(PollListResponse {
        polls: state.meeting.polls.list(&actor, id).await?,
    }))
}

/// One poll.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/polls/{pollId}", tag = "Polls (v2)", operation_id = "v2_get_poll",
    params(("id" = Uuid, Path, description = "Conference id"), ("pollId" = Uuid, Path, description = "Poll id")),
    responses((status = 200, description = "Poll", body = PollView), (status = 404, description = "POLL_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn get_poll(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<PollView>> {
    Ok(Json(state.meeting.polls.get(&actor, id, pid).await?))
}

macro_rules! poll_transition {
    ($name:ident, $path:literal, $op:literal, $to:expr, $doc:literal) => {
        #[doc = $doc]
        #[utoipa::path(
            post, path = $path, tag = "Polls (v2)", operation_id = $op,
            params(("id" = Uuid, Path, description = "Conference id"), ("pollId" = Uuid, Path, description = "Poll id")),
            responses(
                (status = 200, description = "Transitioned (idempotent)", body = PollTransitionResponse),
                (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
                (status = 404, description = "POLL_NOT_FOUND", body = ErrorEnvelope),
                (status = 409, description = "POLL_NOT_OPEN / CONFERENCE_STATE_CONFLICT", body = ErrorEnvelope),
            ),
            security(("bearer_auth" = []), ("participant_token" = []))
        )]
        pub async fn $name(
            State(state): State<AppState>,
            actor: ActorContext,
            V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
        ) -> ApiResult<Json<PollTransitionResponse>> {
            let (poll, changed) = state.meeting.polls.transition(&actor, id, pid, $to).await?;
            Ok(Json(PollTransitionResponse { poll, changed }))
        }
    };
}

poll_transition!(
    open_poll,
    "/api/v2/conferences/{id}/polls/{pollId}/open",
    "v2_open_poll",
    PollStatus::Open,
    "DRAFT → OPEN."
);
poll_transition!(
    close_poll,
    "/api/v2/conferences/{id}/polls/{pollId}/close",
    "v2_close_poll",
    PollStatus::Closed,
    "OPEN → CLOSED (never reopened)."
);
poll_transition!(
    archive_poll,
    "/api/v2/conferences/{id}/polls/{pollId}/archive",
    "v2_archive_poll",
    PollStatus::Archived,
    "CLOSED → ARCHIVED (or cancel a DRAFT)."
);

/// Submit the caller's own response (one ballot per participant).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/polls/{pollId}/responses", tag = "Polls (v2)", operation_id = "v2_submit_poll_response",
    params(("id" = Uuid, Path, description = "Conference id"), ("pollId" = Uuid, Path, description = "Poll id")),
    request_body = SubmitPollResponseRequest,
    responses(
        (status = 201, description = "Recorded", body = PollResponseReceipt),
        (status = 400, description = "POLL_RESPONSE_INVALID", body = ErrorEnvelope),
        (status = 404, description = "POLL_NOT_FOUND", body = ErrorEnvelope),
        (status = 409, description = "POLL_NOT_OPEN / POLL_ALREADY_RESPONDED", body = ErrorEnvelope),
        (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn respond(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<SubmitPollResponseRequest>,
) -> ApiResult<(StatusCode, Json<PollResponseReceipt>)> {
    let r = state.meeting.polls.respond(&actor, id, pid, req).await?;
    let status = if r.idempotent_replay {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(r)))
}

/// Aggregate results, subject to the poll's visibility mode.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/polls/{pollId}/results", tag = "Polls (v2)", operation_id = "v2_poll_results",
    params(("id" = Uuid, Path, description = "Conference id"), ("pollId" = Uuid, Path, description = "Poll id")),
    responses((status = 200, description = "Results", body = PollResults), (status = 403, description = "POLL_RESULTS_NOT_VISIBLE", body = ErrorEnvelope), (status = 404, description = "POLL_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn results(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<PollResults>> {
    Ok(Json(state.meeting.polls.results(&actor, id, pid).await?))
}

// --------------------------------- Q&A ------------------------------------

/// Ask a question (optionally anonymous).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/questions", tag = "Q&A (v2)", operation_id = "v2_submit_question",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = SubmitQuestionRequest,
    responses((status = 201, description = "Created", body = QuestionView), (status = 403, description = "QNA_DISABLED / PERMISSION_DENIED", body = ErrorEnvelope), (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn submit_question(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<SubmitQuestionRequest>,
) -> ApiResult<(StatusCode, Json<QuestionView>)> {
    Ok((
        StatusCode::CREATED,
        Json(state.meeting.questions.submit(&actor, id, req).await?),
    ))
}

/// Questions (pinned first, then by upvotes).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/questions", tag = "Q&A (v2)", operation_id = "v2_list_questions",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Questions", body = QuestionListResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn list_questions(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<QuestionListResponse>> {
    Ok(Json(QuestionListResponse {
        questions: state.meeting.questions.list(&actor, id).await?,
    }))
}

fn qa((question, changed): (QuestionView, bool)) -> Json<QuestionActionResponse> {
    Json(QuestionActionResponse { question, changed })
}

/// Upvote a question (idempotent).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/questions/{questionId}/vote", tag = "Q&A (v2)", operation_id = "v2_vote_question",
    params(("id" = Uuid, Path, description = "Conference id"), ("questionId" = Uuid, Path, description = "Question id")),
    responses((status = 200, description = "Voted", body = QuestionActionResponse), (status = 404, description = "QUESTION_NOT_FOUND", body = ErrorEnvelope), (status = 409, description = "CONFERENCE_STATE_CONFLICT", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn vote(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, qid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<QuestionActionResponse>> {
    Ok(qa(state
        .meeting
        .questions
        .vote(&actor, id, qid, true)
        .await?))
}

/// Remove the caller's upvote (idempotent).
#[utoipa::path(
    delete, path = "/api/v2/conferences/{id}/questions/{questionId}/vote", tag = "Q&A (v2)", operation_id = "v2_unvote_question",
    params(("id" = Uuid, Path, description = "Conference id"), ("questionId" = Uuid, Path, description = "Question id")),
    responses((status = 200, description = "Vote removed", body = QuestionActionResponse), (status = 404, description = "QUESTION_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn unvote(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, qid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<QuestionActionResponse>> {
    Ok(qa(state
        .meeting
        .questions
        .vote(&actor, id, qid, false)
        .await?))
}

/// Answer a question (first committed answer wins).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/questions/{questionId}/answer", tag = "Q&A (v2)", operation_id = "v2_answer_question",
    params(("id" = Uuid, Path, description = "Conference id"), ("questionId" = Uuid, Path, description = "Question id")),
    request_body = AnswerQuestionRequest,
    responses((status = 200, description = "Answered", body = QuestionActionResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "QUESTION_NOT_FOUND", body = ErrorEnvelope), (status = 409, description = "CONFERENCE_STATE_CONFLICT", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn answer(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, qid)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<AnswerQuestionRequest>,
) -> ApiResult<Json<QuestionActionResponse>> {
    Ok(qa(state
        .meeting
        .questions
        .answer(&actor, id, qid, req)
        .await?))
}

/// Dismiss a question.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/questions/{questionId}/dismiss", tag = "Q&A (v2)", operation_id = "v2_dismiss_question",
    params(("id" = Uuid, Path, description = "Conference id"), ("questionId" = Uuid, Path, description = "Question id")),
    responses((status = 200, description = "Dismissed", body = QuestionActionResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "QUESTION_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn dismiss(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, qid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<QuestionActionResponse>> {
    Ok(qa(state.meeting.questions.dismiss(&actor, id, qid).await?))
}

/// Archive a question.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/questions/{questionId}/archive", tag = "Q&A (v2)", operation_id = "v2_archive_question",
    params(("id" = Uuid, Path, description = "Conference id"), ("questionId" = Uuid, Path, description = "Question id")),
    responses((status = 200, description = "Archived", body = QuestionActionResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "QUESTION_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn archive(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, qid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<QuestionActionResponse>> {
    Ok(qa(state.meeting.questions.archive(&actor, id, qid).await?))
}

/// Pin a question.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/questions/{questionId}/pin", tag = "Q&A (v2)", operation_id = "v2_pin_question",
    params(("id" = Uuid, Path, description = "Conference id"), ("questionId" = Uuid, Path, description = "Question id")),
    responses((status = 200, description = "Pinned", body = QuestionActionResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 409, description = "CONFERENCE_STATE_CONFLICT", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn pin(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, qid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<QuestionActionResponse>> {
    Ok(qa(state
        .meeting
        .questions
        .pin(&actor, id, qid, true)
        .await?))
}

/// Unpin a question.
#[utoipa::path(
    delete, path = "/api/v2/conferences/{id}/questions/{questionId}/pin", tag = "Q&A (v2)", operation_id = "v2_unpin_question",
    params(("id" = Uuid, Path, description = "Conference id"), ("questionId" = Uuid, Path, description = "Question id")),
    responses((status = 200, description = "Unpinned", body = QuestionActionResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn unpin(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, qid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<QuestionActionResponse>> {
    Ok(qa(state
        .meeting
        .questions
        .pin(&actor, id, qid, false)
        .await?))
}
