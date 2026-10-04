//! Part F `/api/v2` DTOs (media controls & collaboration). Request bodies reject
//! unknown fields and never carry authority (actor, sender, role, provider identity).
//! Domain request/view types live next to their services and are re-exported here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::Role;

pub use crate::services::collaboration::{
    breakouts::{
        AssignBreakoutsRequest, AssignmentMode, AssignmentPair, AssignmentResult,
        BreakoutAnnouncementRequest, BreakoutStatus, BreakoutView, CloseBreakoutRequest,
        CreateBreakoutsRequest, ManualAssignment, MediaContextView, MoveParticipantRequest,
        RoomAssignmentsPatch,
    },
    engagement::RaisedHand,
    messaging::{MessageKind, MessagePage, MessageView, SendMessageRequest},
    policy::{
        BreakoutRecordingPolicy, ChatSendMode, CollaborationPolicy, CollaborationPolicyPatch,
        ConsentKind, PrivateChatMode, ScreenSharePolicy,
    },
    polls::{
        CreatePollRequest, PollAnswer, PollAnswerInput, PollOption, PollQuestion,
        PollQuestionInput, PollQuestionType, PollResponseReceipt, PollResults, PollStatus,
        PollView, QuestionResult, ResultsVisibility, SubmitPollResponseRequest, TextAnswer,
    },
    prejoin::{
        ConsentGrant, PreJoinAccess, PreJoinConsents, PreJoinDevices, PreJoinDisplayName,
        PreJoinMedia, PreJoinNotices, PreJoinOrganization, PreJoinOrganizer, PreJoinResponse,
    },
    questions::{AnswerQuestionRequest, QuestionStatus, QuestionView, SubmitQuestionRequest},
};

use super::v2::{JoinPermissions, ProviderEnforcement, RtcCredentialResponse};

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaLockResponse {
    pub participant_id: Uuid,
    pub audio_locked: bool,
    pub video_locked: bool,
    pub screen_share_locked: bool,
    pub changed: bool,
    /// Effective publication rights after the change.
    pub permissions: JoinPermissions,
    pub provider_enforcement: ProviderEnforcement,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StopScreenShareResponse {
    pub participant_id: Uuid,
    pub stopped_sessions: u32,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpotlightRequest {
    pub participant_id: Uuid,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpotlightView {
    pub participant_id: Uuid,
    pub display_name: String,
    pub spotlighted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpotlightListResponse {
    pub changed: bool,
    pub spotlights: Vec<SpotlightView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangedResponse {
    pub changed: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollaborationPolicyUpdateRequest {
    /// Optimistic concurrency: the `collaborationVersion` the client last read.
    pub collaboration_version: i32,
    pub changes: CollaborationPolicyPatch,
}

/// Chat-only subset of the collaboration policy.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatPolicyUpdateRequest {
    pub collaboration_version: i32,
    pub chat_enabled: Option<bool>,
    pub chat_send_mode: Option<ChatSendMode>,
    pub private_chat_mode: Option<PrivateChatMode>,
    pub breakout_chat_enabled: Option<bool>,
    pub main_chat_visible_in_breakouts: Option<bool>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationPolicyResponse {
    pub conference_id: Uuid,
    pub collaboration_version: i32,
    pub policy: CollaborationPolicy,
    pub provider_enforcement: ProviderEnforcement,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyCollaborationState {
    pub participant_id: Uuid,
    pub role: Role,
    pub permissions: JoinPermissions,
    pub audio_locked: bool,
    pub video_locked: bool,
    pub screen_share_locked: bool,
    pub chat_restricted: bool,
    pub hand_raised_at: Option<DateTime<Utc>>,
    pub media_context: MediaContextView,
}

/// Canonical collaboration snapshot (REST recovery after realtime gaps).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationStateResponse {
    pub conference_id: Uuid,
    pub collaboration_version: i32,
    /// Realtime cursor this snapshot is at least as new as.
    pub event_sequence: i64,
    pub policy: CollaborationPolicy,
    pub me: Option<MyCollaborationState>,
    pub spotlights: Vec<SpotlightView>,
    pub raised_hands: Vec<RaisedHand>,
    pub active_screen_shares: Vec<Uuid>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaCredentialRequest {
    pub participant_session_id: Uuid,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaCredentialResponse {
    pub rtc: RtcCredentialResponse,
    pub media_context: MediaContextView,
    pub permissions: JoinPermissions,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReactionRequest {
    pub reaction: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReactionResponse {
    pub reaction: String,
    /// Transient: false when the realtime fanout was unavailable (nothing is stored).
    pub delivered: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HandRaiseResponse {
    pub participant_id: Uuid,
    pub raised: bool,
    pub raised_at: Option<DateTime<Utc>>,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RaisedHandsResponse {
    pub hands: Vec<RaisedHand>,
}

#[derive(Debug, Clone, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageListQuery {
    /// MAIN (default), BREAKOUT or PRIVATE.
    pub scope: Option<String>,
    pub breakout_id: Option<Uuid>,
    /// PRIVATE: the other participant of the conversation.
    pub peer_id: Option<Uuid>,
    /// Cursor from a previous page (`nextCursor`).
    pub before: Option<String>,
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BreakoutListResponse {
    pub breakouts: Vec<BreakoutView>,
    pub my_context: Option<MediaContextView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BreakoutListCreatedResponse {
    pub breakouts: Vec<BreakoutView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BreakoutResponse {
    pub breakout: BreakoutView,
    pub changed: bool,
    pub provider_enforcement: ProviderEnforcement,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloseBreakoutsResponse {
    pub breakouts: Vec<BreakoutView>,
    pub provider_enforcement: ProviderEnforcement,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaContextResponse {
    pub participant_id: Option<Uuid>,
    pub media_context: MediaContextView,
    pub changed: bool,
    pub provider_enforcement: ProviderEnforcement,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncementResponse {
    pub rooms: u32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollListResponse {
    pub polls: Vec<PollView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollTransitionResponse {
    pub poll: PollView,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuestionListResponse {
    pub questions: Vec<QuestionView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuestionActionResponse {
    pub question: QuestionView,
    pub changed: bool,
}
