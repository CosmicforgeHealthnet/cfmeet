use crate::{handlers::auth, state::AppState};
use axum::{
    routing::{get, post},
    Router,
};

/// Public authentication routes. Sign-in and sign-up are passwordless: request a code by
/// email, then verify the code (or the one-click link from the same email).
pub fn auth_routes() -> Router<AppState> {
    Router::new()
        .route("/otp/request", post(auth::request_otp))
        .route("/otp/verify", post(auth::verify_otp))
        .route("/otp/verify-link", post(auth::verify_otp_link))
        .route("/mfa/verify", post(auth::verify_mfa))
        .route("/refresh", post(auth::refresh_token))
        .route("/logout", post(auth::logout))
        .route("/email-change/revert", post(auth::revert_email_change))
        // OAuth routes
        .route("/oauth/google", get(auth::oauth_google_init))
        .route("/oauth/google/callback", get(auth::oauth_google_callback))
        .route("/oauth/google/exchange", post(auth::oauth_google_exchange))
}
