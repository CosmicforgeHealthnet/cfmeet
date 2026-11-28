//! Polls in MANAGED conversations (WhatsApp polls).
//!
//! A poll is a MANAGED message with the presentation `POLL`: its (sealed) text is the
//! question, so previews, search, replies, deletion, retention and moderation treat it like
//! any message. `POST /v1/conversations/{id}/messages` creates one with `poll`
//! (`options`: 2-12 distinct answers of 1-100 characters; `allowsMultipleAnswers`). The
//! options are sealed under the environment's MANAGED key like message text, bound to the
//! message; when the message's content is purged its poll goes with it (database trigger).
//!
//! | Route (under `/v1/conversations`) | Scope |
//! |---|---|
//! | `PUT /{id}/messages/{messageId}/poll/vote` | `messages.send` (ACTIVE members) |
//! | `POST /{id}/messages/{messageId}/poll/close` | `messages.send` (the poll's sender or an OWNER / ADMIN) |
//!
//! A vote replaces the member's previous choices (an empty `optionIndexes` retracts it);
//! a closed poll accepts no vote. Members see who voted for what (like WhatsApp): the
//! rendered message carries `managed.poll` with each option's voters. Every change is one
//! transaction with its durable conversation event (`messaging.poll.voted` /
//! `messaging.poll.closed`, ids only), whose hydrated `message` carries the current tally,
//! so realtime clients update live. In PRIVATE_E2EE conversations a poll and its votes are
//! encrypted application messages (the server must not learn them): `POLL_MANAGED_ONLY`.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::{post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{load_message, MessageRow};
use crate::{
    conversations::{
        access,
        service::{request_context, rfc3339, scope_of},
    },
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    events::outbox::{self, NewEvent},
    http::{
        error::ApiError,
        middleware::{Acting, CustomerCaller},
    },
    identity::TrustedContext,
    membership::http::MESSAGES_SEND,
    messages::{
        http::acting_with,
        keys::{DecryptPurpose, KeyScope, ManagedKeyring},
    },
    state::AppState,
};

/// The presentation of a poll message.
pub const POLL: &str = "POLL";
pub const MIN_POLL_OPTIONS: usize = 2;
pub const MAX_POLL_OPTIONS: usize = 12;
/// Longest answer (characters).
pub const MAX_POLL_OPTION_CHARS: usize = 100;
/// Longest question (characters).
pub const MAX_POLL_QUESTION_CHARS: usize = 300;

const OPTIONS_AAD: &str = "cfmeet-messaging-poll-options:v1";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/:conversation_id/messages/:message_id/poll/vote",
            put(vote_handler),
        )
        .route(
            "/:conversation_id/messages/:message_id/poll/close",
            post(close_handler),
        )
}

/// `poll` of a MANAGED send.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PollRequest {
    pub options: Vec<String>,
    #[serde(default)]
    pub allows_multiple_answers: bool,
}

/// A validated poll (trimmed, distinct options).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPoll {
    pub options: Vec<String>,
    pub allows_multiple: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoteRequest {
    pub option_indexes: Vec<i64>,
}

/// Validates a poll's question (the message text) and options.
pub fn validate(question: &str, request: &PollRequest) -> Result<NewPoll> {
    let q = question.trim();
    if q.is_empty() || q.chars().count() > MAX_POLL_QUESTION_CHARS {
        return Err(Error::validation(
            "a poll's text (its question) must be 1-300 characters",
        ));
    }
    if !(MIN_POLL_OPTIONS..=MAX_POLL_OPTIONS).contains(&request.options.len()) {
        return Err(Error::validation("a poll has 2-12 options"));
    }
    let mut options = Vec::with_capacity(request.options.len());
    for o in &request.options {
        let o = o.trim();
        if o.is_empty()
            || o.chars().count() > MAX_POLL_OPTION_CHARS
            || o.chars().any(char::is_control)
        {
            return Err(Error::validation(
                "each poll option must be 1-100 characters without control characters",
            ));
        }
        if options
            .iter()
            .any(|p: &String| p.to_lowercase() == o.to_lowercase())
        {
            return Err(Error::validation("poll options must be distinct"));
        }
        options.push(o.to_string());
    }
    Ok(NewPoll {
        options,
        allows_multiple: request.allows_multiple_answers,
    })
}

/// Distinct, in-range choices (at most one unless the poll allows several).
pub fn valid_choices(
    indexes: &[i64],
    option_count: i16,
    allows_multiple: bool,
) -> Result<Vec<i16>> {
    let mut out: Vec<i16> = Vec::with_capacity(indexes.len());
    for &i in indexes {
        if i < 0 || i >= i64::from(option_count) {
            return Err(Error::validation(
                "optionIndexes must name options of this poll",
            ));
        }
        let i = i as i16;
        if !out.contains(&i) {
            out.push(i);
        }
    }
    if out.len() > 1 && !allows_multiple {
        return Err(Error::validation("this poll allows one answer"));
    }
    out.sort_unstable();
    Ok(out)
}

fn options_aad(row: &MessageRow) -> Vec<u8> {
    format!(
        "{OPTIONS_AAD}:{}:{}:{}:{}:{}",
        row.tenant_id, row.application_id, row.environment_id, row.conversation_id, row.id
    )
    .into_bytes()
}

/// Records the poll of a just-inserted poll message (same transaction).
pub(crate) async fn insert<C: ConnectionTrait>(
    txn: &C,
    ring: &ManagedKeyring,
    pool: &sea_orm::DatabaseConnection,
    row: &MessageRow,
    poll: &NewPoll,
    now: DateTime<Utc>,
) -> Result<()> {
    let plain =
        serde_json::to_vec(&poll.options).map_err(|_| Error::code(ErrorCode::InternalError))?;
    let (sealed, nonce, key_id) = ring
        .seal_record(
            pool,
            &KeyScope {
                tenant_id: row.tenant_id,
                application_id: row.application_id,
                environment_id: row.environment_id,
            },
            &options_aad(row),
            &plain,
        )
        .await?;
    db::exec(
        txn,
        "INSERT INTO messaging_polls (message_id, conversation_id, allows_multiple, option_count, options_sealed, \
           options_nonce, options_key_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            row.id.into(),
            row.conversation_id.into(),
            poll.allows_multiple.into(),
            (poll.options.len() as i16).into(),
            sealed.into(),
            nonce.into(),
            key_id.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone, FromQueryResult)]
struct PollRow {
    allows_multiple: bool,
    option_count: i16,
    options_sealed: Vec<u8>,
    options_nonce: Vec<u8>,
    options_key_id: String,
    closed_at: Option<DateTime<Utc>>,
    closed_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone, FromQueryResult)]
struct VoteRow {
    principal_id: Uuid,
    option_index: i16,
}

async fn poll_row<C: ConnectionTrait>(
    db: &C,
    message_id: Uuid,
    lock: bool,
) -> Result<Option<PollRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT allows_multiple, option_count, options_sealed, options_nonce, options_key_id, closed_at, \
               closed_by_principal_id FROM messaging_polls WHERE message_id = $1{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![message_id.into()],
    )
    .await?)
}

/// The `managed.poll` of an ACTIVE poll message (`None`: not a poll, purged, or the key
/// ring cannot open it).
pub async fn view(state: &AppState, row: &MessageRow) -> Option<Value> {
    if row.managed_presentation.as_deref() != Some(POLL) || !row.is_active() {
        return None;
    }
    let poll = poll_row(&state.db, row.id, false).await.ok()??;
    let plain = state
        .managed_keys
        .as_ref()?
        .open_record(
            &state.db,
            &options_aad(row),
            &poll.options_sealed,
            &poll.options_nonce,
            &poll.options_key_id,
            DecryptPurpose::MemberRead,
        )
        .await?;
    let options: Vec<String> = serde_json::from_slice(&plain).ok()?;
    let votes: Vec<VoteRow> = db::all(
        &state.db,
        "SELECT principal_id, option_index FROM messaging_poll_votes WHERE message_id = $1 \
          ORDER BY voted_at, principal_id",
        vec![row.id.into()],
    )
    .await
    .ok()?;
    Some(render_poll(&options, &poll, &votes))
}

fn render_poll(options: &[String], poll: &PollRow, votes: &[VoteRow]) -> Value {
    let mut voters: Vec<Uuid> = votes.iter().map(|v| v.principal_id).collect();
    voters.sort_unstable();
    voters.dedup();
    json!({
        "options": options.iter().enumerate().take(poll.option_count as usize).map(|(i, text)| {
            let by: Vec<Uuid> = votes.iter().filter(|v| v.option_index as usize == i).map(|v| v.principal_id).collect();
            json!({ "index": i, "text": text, "voteCount": by.len(), "voterPrincipalIds": by })
        }).collect::<Vec<_>>(),
        "allowsMultipleAnswers": poll.allows_multiple,
        "voterCount": voters.len(),
        "closed": poll.closed_at.is_some(),
        "closedAt": poll.closed_at.map(rfc3339),
        "closedByPrincipalId": poll.closed_by_principal_id,
    })
}

/// The poll message `message_id` of a conversation the acting principal is an ACTIVE
/// member of, within its history window; `NOT_FOUND` otherwise.
async fn poll_message<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
) -> Result<(
    crate::conversations::service::ConversationRow,
    access::Membership,
    MessageRow,
)> {
    let (conversation, member) =
        access::as_member(db, ctx, conversation_id, acting.principal_id).await?;
    if conversation.profile() != SecurityProfile::Managed {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "POLL_MANAGED_ONLY",
        ));
    }
    let message = load_message(db, conversation.id, message_id, false)
        .await?
        .filter(|m| {
            m.sequence >= member.visible_from_sequence
                && m.message_kind == "APPLICATION"
                && m.managed_presentation.as_deref() == Some(POLL)
        })
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if !message.is_active() {
        return Err(Error::reason(
            ErrorCode::MessageNotModifiable,
            "MESSAGE_NOT_ACTIVE",
        ));
    }
    Ok((conversation, member, message))
}

async fn event<C: ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    conversation_id: Uuid,
    event_type: &'static str,
    message_id: Uuid,
    principal_id: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    outbox::enqueue_conversation(
        txn,
        conversation_id,
        NewEvent {
            event_type,
            aggregate_type: "CONVERSATION",
            aggregate_id: conversation_id,
            scope: scope_of(ctx),
            payload: json!({ "conversation_id": conversation_id, "message_id": message_id, "principal_id": principal_id }),
        },
        &request_context(),
        now,
    )
    .await?;
    Ok(())
}

async fn answer(
    state: &AppState,
    conversation_id: Uuid,
    message_id: Uuid,
    changed: bool,
) -> Result<Value> {
    let row = load_message(&state.db, conversation_id, message_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    Ok(json!({ "messageId": message_id, "changed": changed, "poll": view(state, &row).await }))
}

/// `PUT /v1/conversations/{id}/messages/{messageId}/poll/vote`.
pub async fn vote(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
    request: VoteRequest,
) -> Result<Value> {
    if request.option_indexes.len() > MAX_POLL_OPTIONS {
        return Err(Error::validation("at most 12 optionIndexes"));
    }
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.clone();
    let changed = state
        .db
        .transaction::<_, bool, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (conversation, _, message) =
                    poll_message(txn, &ctx, &acting, conversation_id, message_id).await?;
                let poll = poll_row(txn, message.id, true).await?.ok_or(Error::code(ErrorCode::NotFound))?;
                if poll.closed_at.is_some() {
                    return Err(Error::reason(ErrorCode::Conflict, "POLL_CLOSED"));
                }
                let choices = valid_choices(&request.option_indexes, poll.option_count, poll.allows_multiple)?;
                let mut current: Vec<i16> = db::all::<VoteRow, _>(
                    txn,
                    "SELECT principal_id, option_index FROM messaging_poll_votes WHERE message_id = $1 AND principal_id = $2",
                    vec![message.id.into(), acting.principal_id.into()],
                )
                .await?
                .into_iter()
                .map(|v| v.option_index)
                .collect();
                current.sort_unstable();
                if current == choices {
                    return Ok(false);
                }
                db::exec(
                    txn,
                    "DELETE FROM messaging_poll_votes WHERE message_id = $1 AND principal_id = $2",
                    vec![message.id.into(), acting.principal_id.into()],
                )
                .await?;
                if !choices.is_empty() {
                    db::exec(
                        txn,
                        "INSERT INTO messaging_poll_votes (message_id, principal_id, option_index, voted_at) \
                         SELECT $1, $2, o, $4 FROM unnest($3::smallint[]) AS o",
                        vec![message.id.into(), acting.principal_id.into(), choices.into(), now.into()],
                    )
                    .await?;
                }
                event(txn, &ctx, conversation.id, "messaging.poll.voted", message.id, acting.principal_id, now).await?;
                Ok(true)
            })
        })
        .await
        .map_err(Error::from)?;
    if changed {
        crate::metrics::inc("messaging_poll_votes_total", &[]);
    }
    answer(state, conversation_id, message_id, changed).await
}

/// `POST /v1/conversations/{id}/messages/{messageId}/poll/close` (idempotent).
pub async fn close(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
) -> Result<Value> {
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.clone();
    let changed = state
        .db
        .transaction::<_, bool, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (conversation, member, message) =
                    poll_message(txn, &ctx, &acting, conversation_id, message_id).await?;
                if message.sender_principal_id != acting.principal_id && !member.is_admin() {
                    return Err(Error::reason(ErrorCode::PrincipalNotAuthorized, "NOT_POLL_CREATOR"));
                }
                let poll = poll_row(txn, message.id, true).await?.ok_or(Error::code(ErrorCode::NotFound))?;
                if poll.closed_at.is_some() {
                    return Ok(false);
                }
                db::exec(
                    txn,
                    "UPDATE messaging_polls SET closed_at = $2, closed_by_principal_id = $3 WHERE message_id = $1",
                    vec![message.id.into(), now.into(), acting.principal_id.into()],
                )
                .await?;
                event(txn, &ctx, conversation.id, "messaging.poll.closed", message.id, acting.principal_id, now).await?;
                Ok(true)
            })
        })
        .await
        .map_err(Error::from)?;
    answer(state, conversation_id, message_id, changed).await
}

async fn vote_handler(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
    Json(body): Json<VoteRequest>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        vote(&state, caller.context(), &acting, id, message, body).await?,
    ))
}

async fn close_handler(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        close(&state, caller.context(), &acting, id, message).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(options: &[&str], multiple: bool) -> PollRequest {
        PollRequest {
            options: options.iter().map(|o| o.to_string()).collect(),
            allows_multiple_answers: multiple,
        }
    }

    #[test]
    fn polls_have_two_to_twelve_distinct_bounded_options() {
        let p = validate(" Lunch? ", &req(&[" Pizza ", "Sushi"], false)).unwrap();
        assert_eq!(p.options, vec!["Pizza", "Sushi"]);
        assert!(!p.allows_multiple);
        assert!(validate("Q", &req(&["only"], false)).is_err());
        assert!(validate("Q", &req(&["a"; 13], false)).is_err());
        assert!(
            validate("Q", &req(&["Yes", "yes"], false)).is_err(),
            "case-insensitively distinct"
        );
        assert!(validate("Q", &req(&["a", ""], false)).is_err());
        assert!(validate("Q", &req(&["a", &"x".repeat(101)], false)).is_err());
        assert!(validate("", &req(&["a", "b"], false)).is_err());
        assert!(validate(&"q".repeat(301), &req(&["a", "b"], false)).is_err());
        let twelve: Vec<String> = (0..12).map(|i| format!("o{i}")).collect();
        let refs: Vec<&str> = twelve.iter().map(String::as_str).collect();
        assert_eq!(validate("Q", &req(&refs, true)).unwrap().options.len(), 12);
    }

    #[test]
    fn choices_are_in_range_distinct_and_single_unless_multiple() {
        assert_eq!(valid_choices(&[2, 0, 2], 3, true).unwrap(), vec![0, 2]);
        assert_eq!(valid_choices(&[], 3, false).unwrap(), Vec::<i16>::new());
        assert_eq!(valid_choices(&[1], 3, false).unwrap(), vec![1]);
        assert!(valid_choices(&[0, 1], 3, false).is_err());
        assert!(valid_choices(&[3], 3, true).is_err());
        assert!(valid_choices(&[-1], 3, true).is_err());
    }
}
