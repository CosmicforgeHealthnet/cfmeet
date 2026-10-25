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
///
/// Creates a poll (or a scored quiz when `quizMode` is true) in DRAFT state; open it with
/// `POST .../open`. Requires the poll-create permission (host, co-host or moderator by default),
/// polls enabled for the meeting and a conference that is not ended or cancelled. Title is 1–300
/// characters; `resultsVisibility` defaults to AFTER_CLOSE. Accepts a bearer user token or a
/// participant token.
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
///
/// Returns up to 200 polls of the conference, oldest first, each with its response count and
/// whether the caller has responded. DRAFT polls are included only for callers who can create or
/// manage polls, and quiz correct answers are hidden from attendees until the results state allows
/// them. The caller must be a participant or member of the conference (bearer user token or
/// participant token).
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
///
/// Returns a single poll of the conference with its questions, status, response count and whether
/// the caller has responded. A DRAFT poll is reported as POLL_NOT_FOUND to callers who cannot
/// create or manage polls, and quiz correct answers are hidden until the results state allows them.
/// Accepts a bearer user token or a participant token.
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
    ($name:ident, $path:literal, $op:literal, $to:expr, $doc:literal, $description:literal) => {
        #[doc = $doc]
        #[doc = ""]
        #[doc = $description]
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
    "Open a draft poll for responses (DRAFT → OPEN).",
    "Starts collecting responses. Allowed only from DRAFT, on a LIVE conference with polls \
     enabled, for a caller with the poll-open permission. Repeating the call on an open poll \
     returns `changed: false`; any other transition returns 409."
);
poll_transition!(
    close_poll,
    "/api/v2/conferences/{id}/polls/{pollId}/close",
    "v2_close_poll",
    PollStatus::Closed,
    "Close an open poll (OPEN → CLOSED; never reopened).",
    "Stops accepting responses. A closed poll is never reopened. Requires the poll-close \
     permission. Repeating the call on a closed poll returns `changed: false`; any other \
     transition returns 409."
);
poll_transition!(
    archive_poll,
    "/api/v2/conferences/{id}/polls/{pollId}/archive",
    "v2_archive_poll",
    PollStatus::Archived,
    "Archive a closed poll or cancel a draft (CLOSED or DRAFT → ARCHIVED).",
    "Removes the poll from active use. Requires the poll-close permission. Repeating the call \
     on an archived poll returns `changed: false`; any other transition returns 409."
);

/// Submit the caller's own response (one ballot per participant).
///
/// Records the caller's answers to an OPEN poll while the conference is LIVE. The caller must be an
/// admitted participant with permission to answer polls; each participant gets exactly one ballot
/// and a second ballot fails with POLL_ALREADY_RESPONDED. Retrying with the same `idempotencyKey`
/// returns 200 with `idempotentReplay: true` instead; a new ballot returns 201. Quizzes are scored
/// server-side, and the score is returned only for non-anonymous AFTER_VOTE quizzes; anonymous
/// ballots are stored without the participant's identity. Rate-limited per participant.
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
///
/// Returns aggregated results of a poll. Poll managers can read them at any time; other callers
/// only as the visibility mode allows (HOST_ONLY: never; AFTER_VOTE: once they have voted or the
/// poll is closed; AFTER_CLOSE: once closed), otherwise POLL_RESULTS_NOT_VISIBLE. Results of
/// anonymous polls are available to everyone, managers included, only once the poll is closed.
/// Accepts a bearer user token or a participant token.
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
///
/// Submits a new OPEN question to the conference Q&A. The caller must be an admitted participant
/// allowed to ask questions, the conference must be LIVE and Q&A enabled; the body is 1–1000
/// characters. `anonymous: true` is accepted only when the meeting allows anonymous questions, and
/// the author's identity is then never returned to hosts or other participants. Rate-limited per
/// participant.
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
///
/// Returns up to 1000 questions of the conference, ordered pinned first, then by upvote count, then
/// oldest first, with `votedByMe` and `isMine` for the caller. Attendees see OPEN and ANSWERED
/// questions; Q&A moderators also see DISMISSED and ARCHIVED ones. Anonymous questions never
/// include the author. Accepts a bearer user token or a participant token.
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
///
/// Adds the caller's upvote to an OPEN or ANSWERED question while the conference is LIVE and Q&A is
/// enabled; the caller must be an admitted participant and cannot upvote their own question. Voting
/// again is a no-op that returns `changed: false`. The upvote count is recalculated from the stored
/// votes. Rate-limited per participant.
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
///
/// Removes the caller's upvote from a question while the conference is LIVE and Q&A is enabled; the
/// caller must be an admitted participant. Returns `changed: false` when the caller had not voted.
/// Rate-limited per participant.
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
///
/// Marks an OPEN question as ANSWERED, with an optional written `answer` (omit it when answered
/// live). Requires the Q&A answer permission (host, co-host or moderator by default) and a
/// conference that is not ended or cancelled. Answering an already-answered question keeps the
/// first answer and returns `changed: false`; dismissed or archived questions return
/// CONFERENCE_STATE_CONFLICT.
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
///
/// Moves an OPEN or ANSWERED question to DISMISSED and unpins it, hiding it from attendees.
/// Requires the Q&A dismiss permission (host, co-host or moderator by default) and a conference
/// that is not ended or cancelled. Dismissing an already-dismissed question returns
/// `changed: false`; an archived question returns CONFERENCE_STATE_CONFLICT.
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
///
/// Moves a question in any state to ARCHIVED and unpins it, hiding it from attendees. Requires the
/// Q&A dismiss permission (host, co-host or moderator by default) and a conference that is not
/// ended or cancelled. Archiving an already-archived question returns `changed: false`.
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
///
/// Pins an OPEN or ANSWERED question so it is listed first. Requires the Q&A pin permission (host,
/// co-host or moderator by default) and a conference that is not ended or cancelled. Pinning an
/// already-pinned question returns `changed: false`; dismissed or archived questions cannot be
/// pinned (CONFERENCE_STATE_CONFLICT).
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
///
/// Removes the pin from a question. Requires the Q&A pin permission (host, co-host or moderator by
/// default) and a conference that is not ended or cancelled. Unpinning a question that is not
/// pinned returns `changed: false`.
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
