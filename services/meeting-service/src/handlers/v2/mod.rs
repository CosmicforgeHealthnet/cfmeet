//! `/api/v2` meeting-domain transport (spec §A6).
//!
//! Handlers only extract, validate, resolve the [`ActorContext`](crate::services::actor::ActorContext),
//! call one service method, and serialize. No role checks, transactions, policy
//! orchestration or provider calls live here.

pub mod artifacts;
pub mod breakouts;
pub mod collaboration;
pub mod compat;
pub mod conferences;
pub mod engagement;
pub mod invitations;
pub mod members;
pub mod organizations;
pub mod participants;
pub mod quality;
pub mod realtime;
pub mod spaces;
pub mod specialized;
pub mod views;

use axum::{
    async_trait,
    extract::{
        rejection::JsonRejection, rejection::PathRejection, FromRequest, FromRequestParts, Request,
    },
    http::request::Parts,
};
use serde::de::DeserializeOwned;

use crate::error::{ApiError, ErrorCode};

/// JSON body extractor whose rejections use the standard error envelope.
pub struct V2Json<T>(pub T);

#[async_trait]
impl<S, T> FromRequest<S> for V2Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(V2Json(v)),
            Err(rejection) if rejection.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE => {
                Err(ApiError::domain(
                    ErrorCode::RequestTooLarge,
                    "Request body is too large",
                ))
            }
            Err(rejection) => Err(ApiError::domain(
                ErrorCode::ValidationError,
                json_message(rejection),
            )),
        }
    }
}

fn json_message(rejection: JsonRejection) -> String {
    match rejection {
        JsonRejection::JsonDataError(e) => format!("Invalid request body: {}", e.body_text()),
        JsonRejection::JsonSyntaxError(_) => "Request body is not valid JSON".into(),
        JsonRejection::MissingJsonContentType(_) => {
            "Expected Content-Type: application/json".into()
        }
        _ => "Invalid request body".into(),
    }
}

/// Optional JSON body (empty body = `T::default()`).
pub struct V2OptionalJson<T>(pub T);

#[async_trait]
impl<S, T> FromRequest<S> for V2OptionalJson<T>
where
    T: DeserializeOwned + Default,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|_| ApiError::domain(ErrorCode::ValidationError, "Invalid request body"))?;
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(V2OptionalJson(T::default()));
        }
        serde_json::from_slice(&bytes)
            .map(V2OptionalJson)
            .map_err(|e| {
                ApiError::domain(
                    ErrorCode::ValidationError,
                    format!("Invalid request body: {e}"),
                )
            })
    }
}

/// Path extractor whose rejections use the standard error envelope.
pub struct V2Path<T>(pub T);

#[async_trait]
impl<S, T> FromRequestParts<S> for V2Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(v)) => Ok(V2Path(v)),
            Err(rejection) => Err(ApiError::domain(
                ErrorCode::ValidationError,
                path_message(rejection),
            )),
        }
    }
}

fn path_message(rejection: PathRejection) -> String {
    match rejection {
        PathRejection::FailedToDeserializePathParams(_) => "Invalid path parameter".into(),
        _ => "Invalid path".into(),
    }
}

/// Query-string extractor whose rejections use the standard error envelope.
pub struct V2Query<T>(pub T);

#[async_trait]
impl<S, T> FromRequestParts<S> for V2Query<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(v)| V2Query(v))
            .map_err(|_| ApiError::domain(ErrorCode::ValidationError, "Invalid query parameters"))
    }
}
