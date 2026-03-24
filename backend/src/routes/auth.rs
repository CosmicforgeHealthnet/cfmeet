use crate::{handlers::auth, state::AppState};
use axum::{routing::{get, post}, Router};

pub fn auth_routes() -> Router<AppState> {
    Router::new()
        .route("/register", post(auth::register))
        .route("/verify-email", post(auth::verify_email))
        .route("/login", post(auth::login))
        .route("/refresh", post(auth::refresh_token))
        .route("/logout", post(auth::logout))
        .route(
            "/resend-verification",
            post(auth::resend_verification_email),
        )
        .route("/forgot-password", post(auth::forgot_password))
        .route("/verify-reset-token", post(auth::verify_reset_token))
        .route("/reset-password", post(auth::reset_password))
        // OAuth routes
        .route("/oauth/google", get(auth::oauth_google_init))
        .route("/oauth/google/callback", get(auth::oauth_google_callback))
}
