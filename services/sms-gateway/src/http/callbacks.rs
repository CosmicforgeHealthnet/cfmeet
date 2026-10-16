//! Provider callbacks (`/provider-callbacks/v1/sms/{provider_id}`): authenticated by the
//! provider adapter's signature, never by a customer credential or a service token.
//! Inbound messages and delivery receipts are Workstreams N / O.

use axum::Router;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
}
