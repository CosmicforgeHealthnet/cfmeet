//! Provider callback ingress (Workstream G): `POST /provider-callbacks/v1/{providerId}`.
//!
//! Not customer- or service-authenticated: each request is authenticated by its provider
//! adapter (signature over the raw body, replay window) before anything is stored. A
//! refused callback is 401 and never applied; a redelivery is acknowledged (200,
//! `duplicate: true`) and changes nothing. Responses carry no provider data.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use super::error::{ApiError, ErrorCode};
use crate::{
    domain::current_context,
    providers::{events::IngestError, CallbackRejection, CallbackRequest},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/:provider_id", post(callback))
}

async fn callback(
    State(s): State<AppState>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let commands = s
        .providers
        .as_ref()
        .ok_or(ApiError::Code(ErrorCode::NotFound))?;
    if !crate::numbers::inventory::valid_provider(&provider_id) {
        return Err(ApiError::Code(ErrorCode::NotFound));
    }
    let request = CallbackRequest {
        headers: headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_string(), v.to_string()))
            })
            .collect(),
        body: body.to_vec(),
    };
    let ctx = current_context().unwrap_or_else(crate::domain::RequestContext::generate);
    match commands.ingest_callback(&ctx, &provider_id, &request).await {
        Ok(i) => Ok(Json(json!({
            "accepted": true,
            "duplicate": i.duplicate,
            "disposition": i.disposition,
        }))),
        Err(IngestError::UnknownProvider) => Err(ApiError::Code(ErrorCode::NotFound)),
        Err(IngestError::Rejected(CallbackRejection::Malformed)) => {
            Err(ApiError::Code(ErrorCode::ValidationError))
        }
        Err(IngestError::Rejected(_)) => Err(ApiError::Code(ErrorCode::AuthenticationFailed)),
        Err(IngestError::Domain(e)) => Err(e.into()),
    }
}
