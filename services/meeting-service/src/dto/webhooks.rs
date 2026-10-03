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
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateWebhookResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub event_type: String,
    pub endpoint_url: String,
    /// Secret key for verifying webhook signatures. Only returned on creation.
    pub secret: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookListResponse {
    pub success: bool,
    pub data: Vec<WebhookResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookApiResponse {
    pub success: bool,
    pub data: WebhookResponse,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateWebhookApiResponse {
    pub success: bool,
    pub data: CreateWebhookResponse,
}
