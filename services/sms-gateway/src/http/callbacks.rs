//! Provider callbacks (`/provider-callbacks/v1/sms/{provider_id}`): authenticated by the
//! provider adapter's signature over the RAW body (timestamp window) BEFORE anything is
//! parsed — never by a customer credential or a service token. Duplicates are answered
//! with the original disposition. Inbound messages (N) and delivery receipts (O).

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    routing::post,
    Json, Router,
};

use super::error::{ApiError, ErrorCode};
use crate::{
    domain::current_context,
    providers::{Callback, CallbackError, SharedProvider},
    sms::{
        inbound::{self, CallbackAck},
        receipts, SmsError,
    },
    state::AppState,
};

const MALFORMED: &str = "The callback is malformed.";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/:provider_id/inbound", post(inbound_message))
        .route("/:provider_id/delivery-receipts", post(delivery_receipt))
}

/// The configured provider named by the path (an unknown id is NOT_FOUND).
fn provider(state: &AppState, provider_id: &str) -> Result<SharedProvider, ApiError> {
    state
        .provider
        .clone()
        .filter(|p| p.id() == provider_id)
        .ok_or(ApiError::Code(ErrorCode::NotFound))
}

/// Authenticates (then parses) a callback.
fn authenticated(
    state: &AppState,
    provider: &SharedProvider,
    kind: &'static str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Callback, ApiError> {
    if let Err(e) = provider.authenticate_callback(headers, body, state.now().timestamp()) {
        let reason = match e {
            CallbackError::Unauthenticated(r) | CallbackError::Malformed(r) => r,
        };
        crate::metrics::inc(
            "sms_callbacks_total",
            &[("kind", kind), ("outcome", "rejected")],
        );
        tracing::warn!(reason, kind, "provider callback rejected");
        return Err(ApiError::Code(ErrorCode::CallbackRejected));
    }
    provider.parse_callback(body).map_err(|e| {
        let reason = match e {
            CallbackError::Unauthenticated(r) | CallbackError::Malformed(r) => r,
        };
        crate::metrics::inc(
            "sms_callbacks_total",
            &[("kind", kind), ("outcome", "malformed")],
        );
        tracing::warn!(reason, kind, "provider callback malformed");
        ApiError::Sms(SmsError::Validation(MALFORMED))
    })
}

async fn inbound_message(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<CallbackAck>, ApiError> {
    let provider = provider(&state, &provider_id)?;
    let callback = authenticated(&state, &provider, "inbound", &headers, &body)?;
    let Callback::Inbound(message) = callback else {
        return Err(ApiError::Sms(SmsError::Validation(MALFORMED)));
    };
    let ctx = current_context().unwrap_or_else(crate::domain::RequestContext::generate);
    inbound::receive(
        &state,
        provider.id(),
        message,
        &inbound::body_digest(&body),
        &ctx,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

async fn delivery_receipt(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<CallbackAck>, ApiError> {
    let provider = provider(&state, &provider_id)?;
    let callback = authenticated(&state, &provider, "receipt", &headers, &body)?;
    let Callback::Receipt(receipt) = callback else {
        return Err(ApiError::Sms(SmsError::Validation(MALFORMED)));
    };
    let ctx = current_context().unwrap_or_else(crate::domain::RequestContext::generate);
    receipts::receive(
        &state,
        provider.id(),
        receipt,
        &inbound::body_digest(&body),
        &ctx,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}
