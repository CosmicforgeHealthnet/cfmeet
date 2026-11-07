//! Legacy meeting-service webhook DTOs.
//!
//! DEPRECATED (Blueprint v1.0 Section 16, PLT-003 / DATA-007): registrations are stored
//! but no event is ever delivered. Every response says so (`delivery`, `notice`) and
//! carries `Deprecation` / `Sunset` headers. Replacement: Developer Platform webhooks
//! (`/v1/environments/{environmentId}/webhooks`), see
//! `docs/api/legacy-meeting-webhooks-deprecation.md`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct CreateWebhookRequest {
    /// Event type: meeting_start, meeting_end, participant_join, participant_leave
    #[validate(length(min = 1, message = "Event type is required"))]
    pub event_type: String,

    #[validate(url(message = "Invalid URL format"))]
    pub endpoint_url: String,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct UpdateWebhookRequest {
    #[validate(url(message = "Invalid URL format"))]
    pub endpoint_url: Option<String>,

    /// Status: active or inactive
    pub status: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub event_type: String,
    pub endpoint_url: String,
    pub status: String,
    /// Always `NOT_DELIVERED`: legacy meeting webhooks are never delivered (deprecated;
    /// use Developer Platform webhooks).
    #[schema(example = "NOT_DELIVERED")]
    pub delivery: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateWebhookResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub event_type: String,
    pub endpoint_url: String,
    /// Generated secret, returned only on creation. No event is ever signed with it,
    /// because legacy meeting webhooks are never delivered.
    pub secret: String,
    pub status: String,
    /// Always `NOT_DELIVERED`: legacy meeting webhooks are never delivered (deprecated;
    /// use Developer Platform webhooks).
    #[schema(example = "NOT_DELIVERED")]
    pub delivery: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookListResponse {
    pub success: bool,
    /// Deprecation notice: nothing registered here is delivered; use Developer Platform
    /// webhooks.
    pub notice: String,
    pub data: Vec<WebhookResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookApiResponse {
    pub success: bool,
    /// Deprecation notice: nothing registered here is delivered; use Developer Platform
    /// webhooks.
    pub notice: String,
    pub data: WebhookResponse,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateWebhookApiResponse {
    pub success: bool,
    /// Deprecation notice: nothing registered here is delivered; use Developer Platform
    /// webhooks.
    pub notice: String,
    pub data: CreateWebhookResponse,
}
