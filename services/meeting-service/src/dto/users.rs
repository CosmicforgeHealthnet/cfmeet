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
    /// Single-use token from `POST /api/v2/me/step-up/verify` with purpose
    /// `DEACTIVATE_ACCOUNT` (a code emailed to the account's address).
    #[serde(default, alias = "stepUpToken")]
    pub step_up_token: Option<String>,
}

/// Request for PATCH /api/v1/users/me
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMeRequest {
    /// 3-50 letters, digits or underscores (validated after trimming, as at registration).
    pub username: Option<String>,
}
