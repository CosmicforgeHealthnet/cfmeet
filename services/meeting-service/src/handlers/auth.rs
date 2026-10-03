use axum::{extract::State, http::StatusCode, Json};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;
use validator::Validate;

use crate::{
    dto::*,
    error::{ApiError, ApiResult},
    models::users::{self, Entity as Users},
    services::auth::{generate_verification_code, hash_password, verify_password},
    services::otp::OtpType,
    state::AppState,
    utils::now_utc,
};

/// Register a new user
#[utoipa::path(
    post,
    path = "/api/v1/auth/register",
    tag = "Authentication",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "User registered successfully", body = RegisterResponse),
        (status = 400, description = "Validation error"),
        (status = 409, description = "User already exists"),
    )
)]
pub async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> ApiResult<(StatusCode, Json<RegisterResponse>)> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Check if user already exists
    let existing_user = Users::find()
        .filter(users::Column::Email.eq(&payload.email))
        .one(&state.db)
        .await?;

    if existing_user.is_some() {
        return Err(ApiError::Conflict(
            "A user with this email already exists".to_string(),
        ));
    }

    // Check if username is already taken
    let existing_username = Users::find()
        .filter(users::Column::Username.eq(&payload.username))
        .one(&state.db)
        .await?;

    if existing_username.is_some() {
        return Err(ApiError::Conflict(
            "Username has already been used".to_string(),
        ));
    }

    // Hash password
    let password_hash = hash_password(&payload.password)?;

    // Generate verification token
    let verification_code = generate_verification_code();

    // Create user (OTP codes are stored in Redis, not in DB)
    let user = users::ActiveModel {
        id: Set(Uuid::new_v4()),
        username: Set(payload.username),
        email: Set(payload.email.clone()),
        password_hash: Set(Some(password_hash)),
        auth_type: Set(users::AuthType::Local),
        role: Set(users::UserRole::User),
        status: Set(users::UserStatus::PendingVerification),
        created_at: Set(now_utc()),
        updated_at: Set(now_utc()),
        last_login: Set(None),
    };

    let user = user.insert(&state.db).await?;

    // Store verification code in Redis with TTL
    state
        .otp_service
        .store_code(&user.email, &verification_code, OtpType::Verification)
        .await?;

    // Queue verification email
    match state
        .email_service
        .send_verification_email(&user.email, &user.username, &verification_code)
        .await
    {
        Ok(job_id) => {
            tracing::info!(
                "Verification email queued for {}: job_id={}",
                user.email,
                job_id
            );
        }
        Err(e) => {
            tracing::error!(
                "Failed to queue verification email for {}: {}",
                user.email,
                e
            );
            // Don't fail registration if email queueing fails - user can request resend
        }
    }

    let response = RegisterResponse {
        user_id: user.id,
        email: user.email,
        role: format!("{:?}", user.role).to_lowercase(),
        status: format!("{:?}", user.status)
            .to_lowercase()
            .replace('_', "_"),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// Verify email with token
#[utoipa::path(
    post,
    path = "/api/v1/auth/verify-email",
    tag = "Authentication",
    request_body = VerifyEmailRequest,
    responses(
        (status = 200, description = "Email verified successfully", body = MessageResponse),
        (status = 400, description = "Invalid or expired token"),
        (status = 404, description = "User not found"),
    )
)]
pub async fn verify_email(
    State(state): State<AppState>,
    Json(payload): Json<VerifyEmailRequest>,
) -> ApiResult<Json<MessageResponse>> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Find user by email
    let user = Users::find()
        .filter(users::Column::Email.eq(&payload.email))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".to_string()))?;

    // Check if already verified
    if user.status == users::UserStatus::Active {
        return Err(ApiError::BadRequest("Email is already verified".to_string()));
    }

    // Verify token from Redis (includes rate limiting and auto-deletion on success)
    let is_valid = state
        .otp_service
        .verify_code(&payload.email, &payload.token, OtpType::Verification)
        .await?;

    if !is_valid {
        return Err(ApiError::BadRequest("Invalid verification code".to_string()));
    }

    // Update user status
    let mut user: users::ActiveModel = user.into();
    user.status = Set(users::UserStatus::Active);
    user.updated_at = Set(now_utc());

    user.update(&state.db).await?;

    Ok(Json(MessageResponse {
        message: "Email verified successfully".to_string(),
    }))
}

/// Login user
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "Authentication",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Login successful", body = LoginResponse),
        (status = 401, description = "Invalid credentials"),
        (status = 403, description = "Email verification required or account inactive"),
        (status = 429, description = "Too many login attempts"),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> ApiResult<Json<LoginResponse>> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Check rate limit before processing login
    state.rate_limiter.check_login_rate(&payload.email).await?;

    // Find user by email
    let user = match Users::find()
        .filter(users::Column::Email.eq(&payload.email))
        .one(&state.db)
        .await?
    {
        Some(user) => user,
        None => {
            return Err(ApiError::Unauthorized("Invalid email or password".to_string()));
        }
    };

    // Verify password
    let password_hash = user
        .password_hash
        .as_ref()
        .ok_or_else(|| ApiError::Unauthorized("Invalid email or password".to_string()))?;

    let is_valid = verify_password(&payload.password, password_hash)?;
    if !is_valid {
        return Err(ApiError::Unauthorized(
            "Invalid email or password".to_string(),
        ));
    }

    // Only block inactive users, allow pending verification users to login
    // Frontend will handle redirect to verify page based on status
    if user.status == users::UserStatus::Inactive {
        return Err(ApiError::Forbidden("Account has been deactivated".to_string()));
    }

    // Clear rate limit on successful login
    state.rate_limiter.clear_login_rate(&payload.email).await?;

    // Generate tokens
    let role_str = format!("{:?}", user.role).to_lowercase();
    let access_token = state
        .jwt_service
        .generate_access_token(user.id, &role_str)?;
    let refresh_token = state
        .jwt_service
        .generate_refresh_token(user.id, &role_str)?;

    // Update last login
    let mut user_active: users::ActiveModel = user.clone().into();
    user_active.last_login = Set(Some(now_utc()));
    user_active.update(&state.db).await?;

    let response = LoginResponse {
        access_token,
        refresh_token,
        user: UserInfo {
            id: user.id,
            username: user.username,
            role: role_str,
            status: format!("{:?}", user.status).to_lowercase().replace('_', "_"),
        },
    };

    Ok(Json(response))
}

/// Refresh access token
#[utoipa::path(
    post,
    path = "/api/v1/auth/refresh",
    tag = "Authentication",
    request_body = RefreshTokenRequest,
    responses(
        (status = 200, description = "Token refreshed successfully", body = RefreshTokenResponse),
        (status = 401, description = "Invalid refresh token"),
    )
)]
pub async fn refresh_token(
    State(state): State<AppState>,
    Json(payload): Json<RefreshTokenRequest>,
) -> ApiResult<Json<RefreshTokenResponse>> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Verify refresh token
    let claims = state.jwt_service.verify_token(&payload.refresh_token)?;

    // Generate new tokens
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    let access_token = state
        .jwt_service
        .generate_access_token(user_id, &claims.role)?;
    let refresh_token = state
        .jwt_service
        .generate_refresh_token(user_id, &claims.role)?;

    Ok(Json(RefreshTokenResponse {
        access_token,
        refresh_token,
    }))
}

/// Logout user
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "Authentication",
    security(
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "Logout successful", body = MessageResponse),
        (status = 401, description = "Unauthorized"),
    )
)]
pub async fn logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<MessageResponse>> {
    // Extract token from Authorization header
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing authorization header".to_string()))?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or_else(|| ApiError::Unauthorized("Invalid authorization format".to_string()))?;

    // Verify token to get claims (to calculate remaining TTL)
    let claims = state
        .jwt_service
        .verify_token(token)
        .map_err(|_| ApiError::Unauthorized("Invalid or expired token".to_string()))?;

    // Calculate remaining TTL from token expiry
    let now = chrono::Utc::now().timestamp();
    let ttl_secs = if claims.exp > now {
        (claims.exp - now) as u64
    } else {
        0 // Already expired, but blacklist briefly anyway
    };

    // Only blacklist if there's remaining TTL
    if ttl_secs > 0 {
        state
            .token_blacklist
            .blacklist_token(token, ttl_secs)
            .await?;
    }

    Ok(Json(MessageResponse {
        message: "Logged out successfully".to_string(),
    }))
}

/// Resend verification email
#[utoipa::path(
    post,
    path = "/api/v1/auth/resend-verification",
    tag = "Authentication",
    request_body = ResendVerificationRequest,
    responses(
        (status = 200, description = "Verification email sent", body = MessageResponse),
        (status = 400, description = "Validation error"),
        (status = 404, description = "User not found"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn resend_verification_email(
    State(state): State<AppState>,
    Json(payload): Json<ResendVerificationRequest>,
) -> ApiResult<Json<MessageResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    let user = Users::find()
        .filter(users::Column::Email.eq(&payload.email))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".to_string()))?;

    if user.status == users::UserStatus::Active {
        return Err(ApiError::BadRequest("User is already verified".to_string()));
    }

    // Check rate limit before sending
    state
        .otp_service
        .check_send_rate(&payload.email, OtpType::Verification)
        .await?;

    // Generate new verification code
    let verification_code = generate_verification_code();

    // Store in Redis with TTL
    state
        .otp_service
        .store_code(&payload.email, &verification_code, OtpType::Verification)
        .await?;

    // Send email
    if let Err(e) = state
        .email_service
        .send_verification_email(&payload.email, &user.username, &verification_code)
        .await
    {
        tracing::error!("Failed to resend verification email: {}", e);
    }

    Ok(Json(MessageResponse {
        message: "Verification email sent".to_string(),
    }))
}

/// Request password reset
#[utoipa::path(
    post,
    path = "/api/v1/auth/forgot-password",
    tag = "Authentication",
    request_body = ForgotPasswordRequest,
    responses(
        (status = 200, description = "Password reset code sent", body = MessageResponse),
        (status = 400, description = "Validation error"),
        (status = 404, description = "User not found"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn forgot_password(
    State(state): State<AppState>,
    Json(payload): Json<ForgotPasswordRequest>,
) -> ApiResult<Json<MessageResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Find user
    let user = Users::find()
        .filter(users::Column::Email.eq(&payload.email))
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            ApiError::NotFound("No account found with this email address".to_string())
        })?;

    // Only allow local auth users to reset password
    if user.auth_type != users::AuthType::Local {
        return Err(ApiError::BadRequest(
            "This account uses Google sign-in. Please use Google to access your account."
                .to_string(),
        ));
    }

    // Check rate limit before sending
    state
        .otp_service
        .check_send_rate(&payload.email, OtpType::PasswordReset)
        .await?;

    // Generate 6-digit reset code
    let reset_code = generate_verification_code();

    // Store in Redis with TTL (1 hour)
    state
        .otp_service
        .store_code(&payload.email, &reset_code, OtpType::PasswordReset)
        .await?;

    // Queue reset email
    match state
        .email_service
        .send_password_reset_email(&user.email, &user.username, &reset_code)
        .await
    {
        Ok(job_id) => {
            tracing::info!(
                "Password reset email queued for {}: job_id={}",
                user.email,
                job_id
            );
        }
        Err(e) => {
            tracing::error!(
                "Failed to queue password reset email for {}: {}",
                user.email,
                e
            );
        }
    }

    Ok(Json(MessageResponse {
        message: "Password reset code sent to your email".to_string(),
    }))
}

/// Reset password with token
#[utoipa::path(
    post,
    path = "/api/v1/auth/reset-password",
    tag = "Authentication",
    request_body = ResetPasswordRequest,
    responses(
        (status = 200, description = "Password reset successfully", body = MessageResponse),
        (status = 400, description = "Invalid or expired token"),
        (status = 429, description = "Too many attempts"),
    )
)]
pub async fn reset_password(
    State(state): State<AppState>,
    Json(payload): Json<ResetPasswordRequest>,
) -> ApiResult<Json<MessageResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Find user by email
    let user = Users::find()
        .filter(users::Column::Email.eq(&payload.email))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".to_string()))?;

    // Verify token from Redis (includes rate limiting and auto-deletion on success)
    let is_valid = state
        .otp_service
        .verify_code(&payload.email, &payload.token, OtpType::PasswordReset)
        .await?;

    if !is_valid {
        return Err(ApiError::BadRequest("Invalid or expired reset code".to_string()));
    }

    // Update password
    let password_hash = hash_password(&payload.new_password)?;

    let mut user: users::ActiveModel = user.into();
    user.password_hash = Set(Some(password_hash));
    user.updated_at = Set(now_utc());

    user.update(&state.db).await?;

    Ok(Json(MessageResponse {
        message: "Password reset successfully".to_string(),
    }))
}

/// Verify password reset token
#[utoipa::path(
    post,
    path = "/api/v1/auth/verify-reset-token",
    tag = "Authentication",
    request_body = VerifyResetTokenRequest,
    responses(
        (status = 200, description = "Token is valid", body = VerifyResetTokenResponse),
        (status = 400, description = "Invalid or expired token"),
        (status = 404, description = "User not found"),
    )
)]
pub async fn verify_reset_token(
    State(state): State<AppState>,
    Json(payload): Json<VerifyResetTokenRequest>,
) -> ApiResult<Json<VerifyResetTokenResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Check if user exists
    let _user = Users::find()
        .filter(users::Column::Email.eq(&payload.email))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".to_string()))?;

    // Check if reset code exists in Redis (without consuming it)
    let has_code = state
        .otp_service
        .has_code(&payload.email, OtpType::PasswordReset)
        .await?;

    if !has_code {
        return Err(ApiError::BadRequest("No password reset request found".to_string()));
    }

    // Note: We don't verify the actual code here to avoid consuming verification attempts
    // The actual verification happens in reset_password endpoint

    Ok(Json(VerifyResetTokenResponse {
        message: "Reset code exists. Please enter the code to reset your password.".to_string(),
        valid: true,
    }))
}


// ============================================================================
// Google OAuth Handlers
// ============================================================================

use axum::response::Redirect;
use rand::Rng;

/// OAuth state TTL (10 minutes)
const OAUTH_STATE_TTL_SECS: u64 = 10 * 60;

/// Initiate Google OAuth flow
///
/// Generates a secure state token, stores it in Redis, and redirects
/// the user to Google's OAuth consent page.
#[utoipa::path(
    get,
    path = "/api/v1/auth/oauth/google",
    tag = "Authentication",
    responses(
        (status = 302, description = "Redirect to Google OAuth consent page"),
        (status = 500, description = "Internal server error"),
    )
)]
pub async fn oauth_google_init(
    State(state): State<AppState>,
) -> ApiResult<Redirect> {
    // Generate cryptographically secure random state (32 bytes, hex encoded)
    let state_value: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    // Store state in Redis with 10-minute TTL
    let oauth_state_key = format!("oauth:state:{}", state_value);
    state
        .redis
        .set(&oauth_state_key, "google", Some(OAUTH_STATE_TTL_SECS))
        .await?;

    // Build Google OAuth URL using config
    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope=openid%20email%20profile&state={}&access_type=offline&prompt=consent",
        urlencoding::encode(&state.google_client_id),
        urlencoding::encode(&state.google_redirect_url),
        urlencoding::encode(&state_value)
    );

    Ok(Redirect::to(&auth_url))
}

/// Google OAuth callback handler
///
/// Validates the state token, exchanges the authorization code for tokens,
/// verifies the ID token, and creates/authenticates the user.
#[utoipa::path(
    get,
    path = "/api/v1/auth/oauth/google/callback",
    tag = "Authentication",
    params(
        ("code" = String, Query, description = "Authorization code from Google"),
        ("state" = String, Query, description = "State token for CSRF protection"),
    ),
    responses(
        (status = 200, description = "Login successful", body = LoginResponse),
        (status = 400, description = "Invalid state or code"),
        (status = 401, description = "Authentication failed"),
        (status = 403, description = "Email verification required or account deactivated"),
    )
)]
pub async fn oauth_google_callback(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Redirect, Redirect> {
    let frontend_url = &state.app_url;

    // Helper to build error redirect
    let error_redirect = |msg: &str| -> Redirect {
        Redirect::to(&format!(
            "{}/google/callback?error={}",
            frontend_url,
            urlencoding::encode(msg)
        ))
    };

    // Extract code and state from query params
    let code = match params.get("code") {
        Some(c) => c,
        None => return Err(error_redirect("Missing authorization code")),
    };
    let state_value = match params.get("state") {
        Some(s) => s,
        None => return Err(error_redirect("Missing state parameter")),
    };

    // Handle error parameter if present
    if let Some(error) = params.get("error") {
        return Err(error_redirect(&format!("OAuth error: {}", error)));
    }

    // Run the async OAuth logic, mapping any ApiError to a redirect
    let result = oauth_google_callback_inner(&state, code, state_value).await;

    match result {
        Ok((access_token, refresh_token, user_id, username, role, status)) => {
            let redirect_url = format!(
                "{}/google/callback?access_token={}&refresh_token={}&user_id={}&username={}&role={}&status={}",
                frontend_url,
                urlencoding::encode(&access_token),
                urlencoding::encode(&refresh_token),
                urlencoding::encode(&user_id),
                urlencoding::encode(&username),
                urlencoding::encode(&role),
                urlencoding::encode(&status),
            );
            Ok(Redirect::to(&redirect_url))
        }
        Err(e) => Err(error_redirect(&e.to_string())),
    }
}

/// Inner logic for Google OAuth callback, returns tokens + user info or an ApiError
async fn oauth_google_callback_inner(
    state: &AppState,
    code: &str,
    state_value: &str,
) -> ApiResult<(String, String, String, String, String, String)> {
    // Validate state from Redis (must exist)
    let oauth_state_key = format!("oauth:state:{}", state_value);

    let provider = state
        .redis
        .get(&oauth_state_key)
        .await?
        .ok_or_else(|| ApiError::BadRequest("Invalid or expired state".to_string()))?;

    // Verify it's a Google OAuth state
    if provider != "google" {
        return Err(ApiError::BadRequest("Invalid OAuth provider".to_string()));
    }

    // Delete state (single-use) - automatically expired by TTL if not deleted
    state.redis.del(&oauth_state_key).await?;

    // Exchange code for tokens
    let token_url = "https://oauth2.googleapis.com/token";
    let client = reqwest::Client::new();

    let token_response = client
        .post(token_url)
        .form(&[
            ("code", code),
            ("client_id", state.google_client_id.as_str()),
            ("client_secret", state.google_client_secret.as_str()),
            ("redirect_uri", state.google_redirect_url.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| ApiError::InternalError(format!("Token exchange failed: {}", e)))?;

    if !token_response.status().is_success() {
        let error_text = token_response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        return Err(ApiError::InternalError(format!("Token exchange failed: {}", error_text)));
    }

    let token_data: serde_json::Value = token_response
        .json()
        .await
        .map_err(|e| ApiError::InternalError(format!("Failed to parse token response: {}", e)))?;

    let id_token = token_data["id_token"].as_str()
        .ok_or_else(|| ApiError::InternalError("No ID token in response".to_string()))?;

    // Decode ID token (basic decoding without signature verification for now)
    let token_parts: Vec<&str> = id_token.split('.').collect();
    if token_parts.len() != 3 {
        return Err(ApiError::InternalError("Invalid ID token format".to_string()));
    }

    use base64::{Engine as _, engine::general_purpose};
    let payload = general_purpose::URL_SAFE_NO_PAD.decode(token_parts[1])
        .map_err(|e| ApiError::InternalError(format!("Failed to decode ID token: {}", e)))?;

    let claims: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|e| ApiError::InternalError(format!("Failed to parse ID token claims: {}", e)))?;

    // Extract user info from claims
    let email = claims["email"].as_str()
        .ok_or_else(|| ApiError::InternalError("No email in ID token".to_string()))?
        .to_string();
    let name = claims["name"].as_str().unwrap_or("Google User").to_string();
    let _google_sub = claims["sub"].as_str()
        .ok_or_else(|| ApiError::InternalError("No sub in ID token".to_string()))?
        .to_string();

    // Verify issuer
    let iss = claims["iss"].as_str().unwrap_or("");
    if iss != "https://accounts.google.com" && iss != "accounts.google.com" {
        return Err(ApiError::Unauthorized("Invalid issuer".to_string()));
    }

    // Find or create user
    let existing_user = Users::find()
        .filter(users::Column::Email.eq(&email))
        .one(&state.db)
        .await?;

    let user = if let Some(mut user) = existing_user {
        if user.status == users::UserStatus::Inactive {
            return Err(ApiError::Forbidden("Account has been deactivated".to_string()));
        }

        if user.status == users::UserStatus::PendingVerification {
            let mut user_active: users::ActiveModel = user.clone().into();
            user_active.status = Set(users::UserStatus::Active);
            user_active.updated_at = Set(chrono::Utc::now().naive_utc());
            user = user_active.update(&state.db).await?;
        }

        user
    } else {
        let new_user = users::ActiveModel {
            id: Set(Uuid::new_v4()),
            username: Set(name.clone()),
            email: Set(email.clone()),
            password_hash: Set(None),
            auth_type: Set(users::AuthType::Google),
            role: Set(users::UserRole::User),
            status: Set(users::UserStatus::Active),
            created_at: Set(chrono::Utc::now().naive_utc()),
            updated_at: Set(chrono::Utc::now().naive_utc()),
            last_login: Set(Some(chrono::Utc::now().naive_utc())),
        };

        new_user.insert(&state.db).await?
    };

    // Update last login
    let mut user_active: users::ActiveModel = user.clone().into();
    user_active.last_login = Set(Some(chrono::Utc::now().naive_utc()));
    user_active.update(&state.db).await?;

    // Generate JWT tokens
    let role_str = format!("{:?}", user.role).to_lowercase();
    let status_str = format!("{:?}", user.status).to_lowercase().replace('_', "_");
    let access_token = state
        .jwt_service
        .generate_access_token(user.id, &role_str)?;
    let refresh_token = state
        .jwt_service
        .generate_refresh_token(user.id, &role_str)?;

    Ok((
        access_token,
        refresh_token,
        user.id.to_string(),
        user.username,
        role_str,
        status_str,
    ))
}
