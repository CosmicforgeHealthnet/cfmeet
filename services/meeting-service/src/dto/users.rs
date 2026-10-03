use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

/// Response for GET /api/v1/users/me
#[derive(Debug, Serialize, ToSchema)]
pub struct UserMeResponse {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub role: String,
    pub status: String,
    pub created_at: String, // ISO 8601 formatted
}

/// Request for PATCH /api/v1/users/me
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct UpdateMeRequest {
    #[validate(length(
        min = 3,
        max = 50,
        message = "Username must be between 3 and 50 characters"
    ))]
    pub username: Option<String>,
}
