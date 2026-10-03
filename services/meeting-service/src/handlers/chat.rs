//! Chat Handlers
//!
//! HTTP handlers for meeting chat functionality (volatile per session).
//!
//! ## Architecture: Hybrid Real-Time + REST
//!
//! This chat system uses a hybrid approach:
//! - **LiveKit Data Channels**: For instant real-time message delivery
//! - **REST API**: For message persistence and history loading
//!
//! ## Frontend Implementation Flow
//!
//! ### Sending a Message
//! 1. Call `POST /chat` to persist the message and get the saved message with ID
//! 2. Use LiveKit `room.localParticipant.publishData()` to broadcast to other participants
//! 3. Display the message locally
//!
//! ### Receiving Messages (Real-Time)
//! - Listen to LiveKit `RoomEvent.DataReceived` for instant message delivery
//! - Parse the payload and display the message
//!
//! ### Late Joiner / Loading History
//! - Call `GET /chat` to load all previous messages when joining
//! - Use `?after=<timestamp>` to load only new messages since last fetch

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set,
};
use uuid::Uuid;
use validator::Validate;

use crate::{
    dto::{
        ApiResponse, ChatMessageResponse, ChatMessagesListResponse,
        ChatMessagesQuery, SendChatMessageRequest,
    },
    utils::{format_utc, now_utc},
    error::{ApiError, ApiResult},
    models::{
        chat_messages::{self, Entity as ChatMessages},
        meetings::{Entity as Meetings, MeetingStatus},
        participants::{self, Entity as Participants, ParticipantStatus},
    },
    services::auth::Claims,
    state::AppState,
};

/// Send a chat message in a meeting
///
/// ## Real-Time Chat Implementation
///
/// This endpoint is part of a **hybrid real-time architecture**:
///
/// ### Step 1: Persist Message (This Endpoint)
/// ```javascript
/// const response = await fetch(`/api/v1/meetings/${meetingId}/chat`, {
///   method: 'POST',
///   headers: {
///     'Content-Type': 'application/json',
///     'Authorization': `Bearer ${token}`
///   },
///   body: JSON.stringify({
///     participant_id: myParticipantId,
///     message: "Hello everyone!"
///   })
/// });
/// const { data: savedMessage } = await response.json();
/// ```
///
/// ### Step 2: Broadcast via LiveKit Data Channel
/// ```javascript
/// const payload = JSON.stringify({
///   type: 'chat_message',
///   id: savedMessage.id,
///   participant_id: savedMessage.participant_id,
///   display_name: savedMessage.display_name,
///   message: savedMessage.message,
///   created_at: savedMessage.created_at
/// });
/// await room.localParticipant.publishData(
///   new TextEncoder().encode(payload),
///   { reliable: true }
/// );
/// ```
///
/// ### Step 3: Receive Messages (Other Participants)
/// ```javascript
/// room.on(RoomEvent.DataReceived, (payload, participant) => {
///   const data = JSON.parse(new TextDecoder().decode(payload));
///   if (data.type === 'chat_message') {
///     // Display the message in UI
///     addMessageToChat(data);
///   }
/// });
/// ```
///
/// ## Notes
/// - Messages are **volatile** and deleted when meeting ends
/// - Only participants with status "joined" can send messages
/// - Message length: 1-2000 characters
#[utoipa::path(
    post,
    path = "/api/v1/meetings/{id}/chat",
    tag = "Chat",
    params(
        ("id" = Uuid, Path, description = "Meeting UUID")
    ),
    request_body = SendChatMessageRequest,
    responses(
        (status = 201, description = "Message persisted successfully. Now broadcast via LiveKit publishData() for real-time delivery.", body = SendChatMessageApiResponse),
        (status = 400, description = "Validation error - message must be 1-2000 characters"),
        (status = 401, description = "Unauthorized - valid JWT token required"),
        (status = 403, description = "Forbidden - not a joined participant of this meeting"),
        (status = 404, description = "Meeting or participant not found"),
        (status = 409, description = "Conflict - meeting is not live (must be 'ongoing' status)")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn send_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(meeting_id): Path<Uuid>,
    Json(payload): Json<SendChatMessageRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<ChatMessageResponse>>)> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Find meeting
    let meeting = Meetings::find_by_id(meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Check if meeting is live (ongoing)
    if meeting.status != MeetingStatus::Ongoing {
        return Err(ApiError::Conflict(
            "Chat is only available during live meetings".to_string(),
        ));
    }

    // Verify participant exists and is joined
    let participant = Participants::find_by_id(payload.participant_id)
        .filter(participants::Column::MeetingId.eq(meeting_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Participant not found in this meeting".to_string()))?;

    // Check participant is actually joined (not waiting or kicked)
    if participant.status != ParticipantStatus::Joined {
        return Err(ApiError::Forbidden(
            "Only joined participants can send messages".to_string(),
        ));
    }

    // Verify the authenticated user owns this participant record
    // (either they have the same user_id or they are a guest participant created by them)
    if let Some(participant_user_id) = participant.user_id {
        if participant_user_id != user_id {
            return Err(ApiError::Forbidden(
                "You can only send messages as yourself".to_string(),
            ));
        }
    }

    let now = now_utc();
    let message_id = Uuid::new_v4();

    // Create chat message
    let chat_message = chat_messages::ActiveModel {
        id: Set(message_id),
        meeting_id: Set(meeting_id),
        participant_id: Set(payload.participant_id),
        message: Set(payload.message.clone()),
        created_at: Set(now),
    };

    let _message = chat_message.insert(&state.db).await?;

    let response = ChatMessageResponse {
        id: message_id,
        participant_id: payload.participant_id,
        display_name: participant.display_name,
        message: payload.message,
        created_at: format_utc(now),
    };

    Ok((
        StatusCode::CREATED,
        Json(ApiResponse {
            success: true,
            data: response,
        }),
    ))
}

/// Get chat messages for a meeting
///
/// ## Use Case: Late Joiners & Chat History
///
/// This endpoint loads chat history from the database. Use it when:
/// - A participant joins a meeting that's already in progress
/// - You need to refresh/reload the chat history
/// - Reconnecting after a network disconnection
///
/// ### Load Full History (Late Joiner)
/// ```javascript
/// // When joining a meeting, load all previous messages
/// const response = await fetch(
///   `/api/v1/meetings/${meetingId}/chat?limit=500`,
///   { headers: { 'Authorization': `Bearer ${token}` } }
/// );
/// const { data: messages } = await response.json();
/// messages.forEach(msg => addMessageToChat(msg));
/// ```
///
/// ### Load New Messages Only (Reconnection)
/// ```javascript
/// // After reconnecting, load only messages since last received
/// const lastMessageTime = messages[messages.length - 1]?.created_at;
/// const response = await fetch(
///   `/api/v1/meetings/${meetingId}/chat?after=${lastMessageTime}`,
///   { headers: { 'Authorization': `Bearer ${token}` } }
/// );
/// const { data: newMessages } = await response.json();
/// newMessages.forEach(msg => addMessageToChat(msg));
/// ```
///
/// ## Complete Frontend Setup
///
/// ```javascript
/// // 1. Join meeting and get participant info
/// const joinResponse = await joinMeeting(meetingId, displayName);
/// const { participant_id, join_token, room_name } = joinResponse.data;
///
/// // 2. Connect to LiveKit room
/// const room = new Room();
/// await room.connect(LIVEKIT_URL, join_token);
///
/// // 3. Load chat history
/// const history = await fetch(`/api/v1/meetings/${meetingId}/chat`);
/// const { data: messages } = await history.json();
///
/// // 4. Listen for real-time messages
/// room.on(RoomEvent.DataReceived, (payload) => {
///   const data = JSON.parse(new TextDecoder().decode(payload));
///   if (data.type === 'chat_message') {
///     addMessageToChat(data);
///   }
/// });
///
/// // 5. Send messages
/// async function sendMessage(text) {
///   // Persist to DB
///   const res = await fetch(`/api/v1/meetings/${meetingId}/chat`, {
///     method: 'POST',
///     headers: { 'Authorization': `Bearer ${token}`, 'Content-Type': 'application/json' },
///     body: JSON.stringify({ participant_id, message: text })
///   });
///   const { data: saved } = await res.json();
///
///   // Broadcast via LiveKit
///   await room.localParticipant.publishData(
///     new TextEncoder().encode(JSON.stringify({ type: 'chat_message', ...saved })),
///     { reliable: true }
///   );
/// }
/// ```
///
/// ## Notes
/// - Messages are **volatile** - deleted when meeting ends
/// - Messages ordered by `created_at` ascending (oldest first)
/// - Use `after` param to avoid duplicate messages when polling
#[utoipa::path(
    get,
    path = "/api/v1/meetings/{id}/chat",
    tag = "Chat",
    params(
        ("id" = Uuid, Path, description = "Meeting UUID"),
        ("after" = Option<String>, Query, description = "ISO 8601 timestamp - only return messages created after this time. Use for incremental loading."),
        ("limit" = Option<u64>, Query, description = "Max messages to return (default: 100, max: 500)")
    ),
    responses(
        (status = 200, description = "Chat messages ordered by created_at ascending", body = ChatMessagesListResponse),
        (status = 401, description = "Unauthorized - valid JWT token required"),
        (status = 403, description = "Forbidden - must be a participant or host of this meeting"),
        (status = 404, description = "Meeting not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_messages(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(meeting_id): Path<Uuid>,
    Query(query): Query<ChatMessagesQuery>,
) -> ApiResult<Json<ChatMessagesListResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Find meeting
    let meeting = Meetings::find_by_id(meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Verify user is a participant of this meeting
    let is_participant = Participants::find()
        .filter(participants::Column::MeetingId.eq(meeting_id))
        .filter(participants::Column::UserId.eq(user_id))
        .filter(participants::Column::Status.eq(ParticipantStatus::Joined))
        .one(&state.db)
        .await?
        .is_some();

    // Also check if user is the host
    let is_host = meeting.host_id == user_id;

    if !is_participant && !is_host {
        return Err(ApiError::Forbidden(
            "Only meeting participants can view chat messages".to_string(),
        ));
    }

    let limit = query.limit.unwrap_or(100).min(500);

    // Build query for messages
    let mut messages_query = ChatMessages::find()
        .filter(chat_messages::Column::MeetingId.eq(meeting_id))
        .order_by_asc(chat_messages::Column::CreatedAt);

    // Filter by timestamp if provided (input is already in Nigeria time)
    if let Some(after) = query.after {
        messages_query =
            messages_query.filter(chat_messages::Column::CreatedAt.gt(after));
    }

    // Get messages with participant info
    let messages = messages_query.all(&state.db).await?;

    // Get participant display names
    let participant_ids: Vec<Uuid> = messages.iter().map(|m| m.participant_id).collect();
    let participants_map: std::collections::HashMap<Uuid, String> = Participants::find()
        .filter(participants::Column::Id.is_in(participant_ids))
        .all(&state.db)
        .await?
        .into_iter()
        .map(|p| (p.id, p.display_name))
        .collect();

    // Build response with display names
    let data: Vec<ChatMessageResponse> = messages
        .into_iter()
        .take(limit as usize)
        .map(|m| ChatMessageResponse {
            id: m.id,
            participant_id: m.participant_id,
            display_name: participants_map
                .get(&m.participant_id)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string()),
            message: m.message,
            created_at: format_utc(m.created_at),
        })
        .collect();

    Ok(Json(ChatMessagesListResponse { success: true, data }))
}
