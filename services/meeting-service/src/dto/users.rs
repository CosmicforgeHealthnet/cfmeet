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

/// Optional body of POST /api/v1/users/me/deactivate (ACC-026).
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct DeactivateAccountRequest {
    /// The account's current password. Required unless the caller signed in within the
    /// re-authentication window (default 10 minutes); accounts without a password (Google
    /// sign-in) re-authenticate by signing in again.
    pub current_password: Option<String>,
}

/// Request for PATCH /api/v1/users/me
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMeRequest {
    /// 3-50 letters, digits or underscores (validated after trimming, as at registration).
    pub username: Option<String>,
}
