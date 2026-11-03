use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("authentication required")]
    Authentication,
    #[error("permission denied")]
    Forbidden,
    #[error("resource not found")]
    NotFound,
    #[error("validation failed")]
    Validation,
    #[error("version conflict")]
    Version,
    #[error("idempotency conflict")]
    Idempotency,
    #[error("request already pending")]
    Pending,
    #[error("rate limited")]
    RateLimited,
    #[error("recent authentication required")]
    RecentAuthentication,
    #[error("dependency unavailable")]
    Unavailable,
    #[error("database operation failed")]
    Database(#[from] sea_orm::DbErr),
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Authentication => "AUTHENTICATION_REQUIRED",
            Self::Forbidden => "PERMISSION_DENIED",
            Self::NotFound => "NOT_FOUND",
            Self::Validation => "VALIDATION_ERROR",
            Self::Version => "VERSION_CONFLICT",
            Self::Idempotency => "IDEMPOTENCY_CONFLICT",
            Self::Pending => "REQUEST_PENDING",
            Self::RateLimited => "RATE_LIMITED",
            Self::RecentAuthentication => "RECENT_AUTHENTICATION_REQUIRED",
            Self::Unavailable | Self::Database(_) => "COMMUNICATIONS_UNAVAILABLE",
        }
    }
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Authentication => StatusCode::UNAUTHORIZED,
            Self::Forbidden | Self::RecentAuthentication => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Validation => StatusCode::BAD_REQUEST,
            Self::Version | Self::Idempotency | Self::Pending => StatusCode::CONFLICT,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable | Self::Database(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        if matches!(self, Self::Database(_)) {
            tracing::error!(code = self.code(), "Communications database request failed");
        }
        let mut response = (
            self.status(),
            Json(
                json!({"schemaVersion":1,"error":{"code":self.code(),"message":self.to_string()}}),
            ),
        )
            .into_response();
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
        if matches!(self, Self::RateLimited) {
            response
                .headers_mut()
                .insert("retry-after", HeaderValue::from_static("60"));
        }
        response
    }
}
