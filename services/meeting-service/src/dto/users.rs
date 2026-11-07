use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Response for GET /api/v1/users/me
#[derive(Debug, Serialize, ToSchema)]
pub struct UserMeResponse {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub role: String,
    /// `pending_verification` (spelled `pendingverification` until
    /// `ACCOUNT_STATUS_FORMAT=canonical`), `active`, `inactive` or `suspended`.
    #[schema(example = "active")]
    pub status: String,
    pub created_at: String, // ISO 8601 formatted
}

/// Request for PATCH /api/v1/users/me
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMeRequest {
    /// 3-50 letters, digits or underscores (validated after trimming, as at registration).
    pub username: Option<String>,
}
