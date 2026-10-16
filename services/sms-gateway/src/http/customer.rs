//! The customer API (`/v1/sms/**`, Platform Access authentication, `sms.read` /
//! `sms.send` scopes). Message reads and threads are Workstream N; sending is O.

use axum::Router;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
}
