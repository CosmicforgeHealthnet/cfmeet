use axum::{extract::State, http::HeaderMap, http::StatusCode, Json};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;
use validator::Validate;

use crate::{
    dto::*,
    error::{ApiError, ApiResult},
    models::users::{self, Entity as Users},
    services::account_security::{
        self as security, ActorKind, AuthMethod, Outcome, RevokeReason, SecurityEvent,
    },
    services::auth::{
        generate_verification_code, hash_password, verify_password, Claims, TokenKind,
    },
    services::otp::OtpType,
    state::AppState,
    utils::now_utc,
};

/// Per-client-address limit, active only when the trusted proxy header is configured
/// (`AUTH_CLIENT_IP_HEADER`); per-account limits apply regardless (ACC-015).
async fn limit_by_client(
    state: &AppState,
    headers: &HeaderMap,
    action: &str,
    max_attempts: i64,
    window_secs: u64,
) -> ApiResult<()> {
    if let Some(address) = state.auth_policy.client_ip(headers) {
        state
            .rate_limiter
            .check_action_rate(
                &format!("auth:{action}:ip"),
                &address,
                max_attempts,
                window_secs,
            )
            .await?;
    }
    Ok(())
}

/// ACC-022: one rendering of the account status for every v1 sign-in response
/// (`ACCOUNT_STATUS_FORMAT`, see `AuthPolicy::status_format`).
fn status_string(state: &AppState, user: &users::Model) -> String {
    state
        .auth_policy
        .status_format
        .render(&user.status)
        .to_owned()
}

/// Uniform answer for the account-recovery endpoints, whether or not an account exists
/// (ACC-014).
const RECOVERY_ACCEPTED: &str =
    "If an account matches, an email with further instructions has been sent.";

/// Register a new user
///
/// Creates an email-and-password account with the `user` role and `pending_verification` status,
/// then emails a 6-digit verification code (valid for 24 hours). Public endpoint; no tokens are
/// returned. Username must be 3-50 letters, digits or underscores and password 8-128 characters.
/// Email addresses are case-insensitive and stored lowercased. Returns 409 if the email or
/// username is already in use.
#[utoipa::path(
    post,
    path = "/api/v1/auth/register",
    tag = "Authentication",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "User registered successfully", body = RegisterResponse),
        (status = 400, description = "Validation error"),
        (status = 409, description = "User already exists"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<RegisterRequest>,
) -> ApiResult<(StatusCode, Json<RegisterResponse>)> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;
    limit_by_client(&state, &headers, "register", 10, 60 * 60).await?;
    let email = security::normalize_email(&payload.email);

    // Check if user already exists (case-insensitive, DATA-002)
    if security::find_user_by_email(&state.db, &email)
        .await?
        .is_some()
    {
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
        email: Set(email),
        password_hash: Set(Some(password_hash)),
        auth_type: Set(users::AuthType::Local),
        role: Set(users::UserRole::User),
        status: Set(users::UserStatus::PendingVerification),
        created_at: Set(now_utc()),
        updated_at: Set(now_utc()),
        last_login: Set(None),
        ..Default::default()
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
            tracing::info!(user_id = %user.id, job_id = %job_id, "verification email queued");
        }
        Err(e) => {
            tracing::error!(user_id = %user.id, "failed to queue verification email: {e}");
            // Don't fail registration if email queueing fails - user can request resend
        }
    }
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("ACCOUNT_REGISTERED", Outcome::Success).by_user(user.id),
    )
    .await;

    let response = RegisterResponse {
        user_id: user.id,
        email: user.email.clone(),
        role: format!("{:?}", user.role).to_lowercase(),
        status: status_string(&state, &user),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// Verify email with token
///
/// Activates a newly registered account using the 6-digit code emailed at registration. Public
/// endpoint. The code is single-use; repeated wrong codes for the same email are rate-limited
/// (429). Returns 400 if the code is wrong or expired; the answer does not reveal whether an
/// account exists or is already verified.
#[utoipa::path(
    post,
    path = "/api/v1/auth/verify-email",
    tag = "Authentication",
    request_body = VerifyEmailRequest,
    responses(
        (status = 200, description = "Email verified successfully", body = MessageResponse),
        (status = 400, description = "Invalid or expired code"),
        (status = 429, description = "Too many attempts"),
    )
)]
pub async fn verify_email(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<VerifyEmailRequest>,
) -> ApiResult<Json<MessageResponse>> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;
    limit_by_client(&state, &headers, "verify-email", 30, 15 * 60).await?;
    let invalid = || ApiError::BadRequest("Invalid or expired verification code".to_string());

    let user = security::find_user_by_email(&state.db, &payload.email)
        .await?
        .ok_or_else(invalid)?;
    if user.status != users::UserStatus::PendingVerification {
        return Err(invalid());
    }

    // Verify token from Redis (includes rate limiting and auto-deletion on success)
    let is_valid = state
        .otp_service
        .verify_code(&user.email, &payload.token, OtpType::Verification)
        .await?;

    if !is_valid {
        return Err(invalid());
    }

    // Update user status
    let user_id = user.id;
    let mut user: users::ActiveModel = user.into();
    user.status = Set(users::UserStatus::Active);
    user.updated_at = Set(now_utc());
    // Blueprint Section 9 (DATA-016): the proof is recorded, independent of `status`.
    user.email_verified_at = Set(Some(chrono::Utc::now()));

    user.update(&state.db).await?;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("EMAIL_VERIFIED", Outcome::Success).by_user(user_id),
    )
    .await;

    Ok(Json(MessageResponse {
        message: "Email verified successfully".to_string(),
    }))
}

/// Login user
///
/// Authenticates with email and password and returns an access token (valid 15 minutes), a refresh
/// token (valid 7 days, rotated on every refresh) and basic user info. Each login opens an account
/// session that lasts at most 30 days and can be listed and revoked from
/// `/api/v2/me/sessions`. Public endpoint. Accounts still pending email verification can log in, so
/// check `user.status` and route to verification if needed (unless the deployment enforces
/// verification at sign-in: then such an account gets `403 EMAIL_VERIFICATION_REQUIRED` after a
/// correct password); deactivated accounts get 403. More than
/// 5 attempts per email within 15 minutes returns 429; a successful login resets the count.
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
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> ApiResult<Json<LoginResponse>> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;
    limit_by_client(&state, &headers, "login", 30, 15 * 60).await?;
    let email = security::normalize_email(&payload.email);

    // Check rate limit before processing login
    state.rate_limiter.check_login_rate(&email).await?;

    // Unknown accounts, Google-only accounts and wrong passwords take the same time and
    // get the same answer (ACC-014).
    let user = security::find_user_by_email(&state.db, &email).await?;
    let hash = user.as_ref().and_then(|u| u.password_hash.clone());
    let is_valid = match hash.as_deref() {
        Some(hash) => verify_password(&payload.password, hash)?,
        None => {
            let _ = verify_password(&payload.password, security::timing_equalizer_hash());
            false
        }
    };
    let user = match user {
        Some(user) if is_valid => user,
        known => {
            if let Some(known) = known {
                security::record_event_best_effort(
                    &state.db,
                    SecurityEvent::new("LOGIN", Outcome::Failure)
                        .user(known.id)
                        .actor(ActorKind::Anonymous, None)
                        .detail(serde_json::json!({ "method": "PASSWORD" })),
                )
                .await;
            }
            return Err(ApiError::Unauthorized(
                "Invalid email or password".to_string(),
            ));
        }
    };

    // Only block inactive users, allow pending verification users to login
    // Frontend will handle redirect to verify page based on status
    if matches!(
        user.status,
        users::UserStatus::Inactive | users::UserStatus::Suspended
    ) {
        return Err(ApiError::Forbidden(
            "Account has been deactivated".to_string(),
        ));
    }

    // ACC-016 (owner decision, off by default): optionally refuse pending accounts at
    // sign-in. Checked only after a correct password, so it reveals nothing to others.
    if state.require_email_verification
        && state
            .auth_policy
            .email_verification_enforcement
            .covers_login()
        && user.status == users::UserStatus::PendingVerification
    {
        security::record_event_best_effort(
            &state.db,
            SecurityEvent::new("LOGIN", Outcome::Denied)
                .user(user.id)
                .actor(ActorKind::Anonymous, None)
                .detail(serde_json::json!({ "method": "PASSWORD", "reason": "EMAIL_VERIFICATION_REQUIRED" })),
        )
        .await;
        return Err(security::email_verification_required());
    }

    // Clear rate limit on successful login
    state.rate_limiter.clear_login_rate(&email).await?;

    let issued = security::start_session(
        &state,
        &user,
        AuthMethod::Password,
        None,
        security::client_label(&headers),
    )
    .await?;

    // Update last login
    let mut user_active: users::ActiveModel = user.clone().into();
    user_active.last_login = Set(Some(now_utc()));
    user_active.update(&state.db).await?;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("LOGIN", Outcome::Success)
            .by_user(user.id)
            .session(issued.session_id)
            .detail(serde_json::json!({ "method": "PASSWORD" })),
    )
    .await;

    let response = LoginResponse {
        access_token: issued.access_token,
        refresh_token: issued.refresh_token,
        user: UserInfo {
            id: user.id,
            username: user.username.clone(),
            role: security::role_claim(&user),
            status: status_string(&state, &user),
        },
    };

    Ok(Json(response))
}

/// Refresh access token
///
/// Exchanges a valid, unexpired refresh token for a new access token (valid 15 minutes) and a new
/// refresh token (valid 7 days). Public endpoint: send the refresh token in the request body, not
/// in the `Authorization` header. Refresh tokens are single-use: presenting a superseded refresh
/// token ends the whole session. A session lasts at most 30 days from sign-in. Returns 401 if the
/// token is invalid, expired, revoked or not a refresh token.
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
    let invalid = || ApiError::Unauthorized("Invalid refresh token".to_string());

    // Verify refresh token (ACC-001: an access token never refreshes)
    let claims = state
        .jwt_service
        .verify_token(&payload.refresh_token)
        .map_err(|_| invalid())?;
    if !claims.is_kind(TokenKind::Refresh) {
        return Err(invalid());
    }
    if state
        .token_blacklist
        .is_blacklisted(&payload.refresh_token)
        .await?
    {
        return Err(ApiError::Unauthorized("Token has been revoked".into()));
    }

    let subject = Uuid::parse_str(&claims.sub).map_err(|_| invalid())?;

    if claims.role == "guest" {
        // SEC-004: a guest refresh needs a live guest participation (see
        // `guest_refresh_allowed`), and the presented refresh token is single use.
        if !guest_refresh_allowed(&state, subject, &claims).await? {
            return Err(invalid());
        }
        let remaining = (claims.exp - chrono::Utc::now().timestamp()).max(1) as u64;
        state
            .token_blacklist
            .blacklist_token(&payload.refresh_token, remaining)
            .await?;
        let (access_token, refresh_token) =
            state
                .jwt_service
                .refreshed_tokens(subject, &claims.role, claims.auth_time)?;
        return Ok(Json(RefreshTokenResponse {
            access_token,
            refresh_token,
        }));
    }

    let user = users::Entity::find_by_id(subject)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("Account is unavailable".into()))?;
    if matches!(
        user.status,
        users::UserStatus::Inactive | users::UserStatus::Suspended
    ) {
        return Err(ApiError::Unauthorized("Account is unavailable".into()));
    }
    security::authorize_user_token(&state, &claims, user.id, TokenKind::Refresh).await?;

    let (access_token, refresh_token) = if claims.session_id().is_some() {
        security::rotate_session(&state, &claims, &user).await?
    } else {
        upgrade_legacy_refresh(&state, &claims, &payload.refresh_token, &user).await?
    };

    Ok(Json(RefreshTokenResponse {
        access_token,
        refresh_token,
    }))
}

/// Absolute lifetime of a legacy guest session, counted from the guest's join
/// (`auth_time`); refresh never extends it (SEC-004).
pub const GUEST_SESSION_MAX_SECONDS: i64 = 24 * 60 * 60;

/// SEC-004 liveness rule for a legacy guest refresh: the guest participant still exists,
/// was not removed (kicked) and has not left, its meeting is neither ended nor cancelled,
/// and the guest session is younger than [`GUEST_SESSION_MAX_SECONDS`].
async fn guest_refresh_allowed(
    state: &AppState,
    participant_id: Uuid,
    claims: &Claims,
) -> ApiResult<bool> {
    use crate::models::{
        meetings::{Entity as Meetings, MeetingStatus},
        participants::{Entity as Participants, ParticipantStatus},
    };
    let Some(auth_time) = claims.auth_time else {
        return Ok(false);
    };
    if chrono::Utc::now().timestamp() - auth_time > GUEST_SESSION_MAX_SECONDS {
        return Ok(false);
    }
    let Some(participant) = Participants::find_by_id(participant_id)
        .one(&state.db)
        .await?
    else {
        return Ok(false);
    };
    if participant.status == ParticipantStatus::Kicked || participant.leave_time.is_some() {
        return Ok(false);
    }
    let live_meeting = Meetings::find_by_id(participant.meeting_id)
        .one(&state.db)
        .await?
        .is_some_and(|m| !matches!(m.status, MeetingStatus::Ended | MeetingStatus::Cancelled));
    Ok(live_meeting)
}

/// A refresh token issued before sessions existed is accepted once: it opens a session
/// keeping its original authentication time (so the 30-day absolute lifetime still counts
/// from the original sign-in) and is then revoked, so it cannot be replayed.
async fn upgrade_legacy_refresh(
    state: &AppState,
    claims: &Claims,
    raw_token: &str,
    user: &users::Model,
) -> ApiResult<(String, String)> {
    let max_age = security::SESSION_ABSOLUTE_LIFETIME_DAYS * 24 * 60 * 60;
    let now = chrono::Utc::now().timestamp();
    let auth_time = claims
        .auth_time
        .filter(|t| now - *t < max_age)
        .ok_or_else(|| {
            ApiError::Unauthorized("This session has ended. Please sign in again.".into())
        })?;
    let ttl = (claims.exp - now).max(1) as u64;
    state
        .token_blacklist
        .blacklist_token(raw_token, ttl)
        .await?;
    let issued = security::start_session(
        state,
        user,
        AuthMethod::LegacyRefresh,
        Some(auth_time),
        None,
    )
    .await?;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("LEGACY_SESSION_UPGRADED", Outcome::Success)
            .by_user(user.id)
            .session(issued.session_id),
    )
    .await;
    Ok((issued.access_token, issued.refresh_token))
}

/// Logout user
///
/// Ends the current session: the bearer access token is revoked and the session's refresh
/// token stops working. Tokens issued before account sessions existed cannot be told apart, so
/// for those this signs the account out everywhere. Other sessions stay signed in; use
/// `POST /api/v2/me/sessions/revoke-all` to sign out everywhere. When the Messaging integration is
/// enabled, it also ends the account's Messaging sessions; a Messaging outage does not block
/// logout.
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
    headers: HeaderMap,
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
    if !claims.is_kind(TokenKind::Access) {
        return Err(ApiError::Unauthorized(
            "Invalid or expired token".to_string(),
        ));
    }

    // ACC-025: the database revocation below is authoritative (session row / account
    // epoch, checked on every request) and runs first: if it fails nothing is half done
    // and the client may retry. The Redis blacklist afterwards is defence in depth and
    // never turns a completed logout into an error.
    if claims.role != "guest" {
        if let Ok(user_id) = Uuid::parse_str(&claims.sub) {
            match claims.session_id() {
                Some(session_id) => {
                    security::revoke_session(&state.db, user_id, session_id, RevokeReason::Logout)
                        .await?;
                    security::record_event_best_effort(
                        &state.db,
                        SecurityEvent::new("LOGOUT", Outcome::Success)
                            .by_user(user_id)
                            .session(session_id),
                    )
                    .await;
                    // Section 14: the Hub devices this sign-in linked end with it.
                    if let Err(e) =
                        crate::services::communications::session_ended(&state, user_id, session_id)
                            .await
                    {
                        tracing::error!(error = ?e, "session-bound device revocation not queued");
                    }
                }
                None => {
                    // Legacy token: its refresh token is unknown, so end every credential.
                    security::revoke_all(&state.db, user_id, RevokeReason::LogoutAll).await?;
                    crate::services::communications::record_logout(&state, user_id).await?;
                    // Durable queue remains authoritative if synchronous delivery is unavailable.
                    let _ = crate::services::communications::deliver_due(&state).await;
                    security::record_event_best_effort(
                        &state.db,
                        SecurityEvent::new("LOGOUT_ALL", Outcome::Success)
                            .by_user(user_id)
                            .detail(serde_json::json!({ "trigger": "LEGACY_TOKEN_LOGOUT" })),
                    )
                    .await;
                }
            }
        }
    }

    let now = chrono::Utc::now().timestamp();
    if claims.exp > now {
        if let Err(e) = state
            .token_blacklist
            .blacklist_token(token, (claims.exp - now) as u64)
            .await
        {
            if claims.role == "guest" {
                // Guest tokens have no session row: the blacklist is their only revocation.
                return Err(e);
            }
            tracing::warn!(error = ?e, "logout: token blacklist unavailable; session revocation already recorded");
        }
    }

    // Part V H §104: logging out of CF Meet also ends the account's Messaging client
    // sessions (durable job; a Messaging outage never blocks the logout).
    if let (Some(um), Ok(user_id)) = (
        state.meeting.core.unified.as_ref(),
        Uuid::parse_str(&claims.sub),
    ) {
        um.revoke_sessions_for_user(user_id, "LOGOUT").await;
    }

    Ok(Json(MessageResponse {
        message: "Logged out successfully".to_string(),
    }))
}

/// Resend verification email
///
/// Sends a new 6-digit verification code to an account that is still pending verification,
/// replacing the previous code. Public endpoint. The answer is the same whether or not such an
/// account exists; at most 5 codes per email are sent within 15 minutes.
#[utoipa::path(
    post,
    path = "/api/v1/auth/resend-verification",
    tag = "Authentication",
    request_body = ResendVerificationRequest,
    responses(
        (status = 200, description = "Request accepted", body = MessageResponse),
        (status = 400, description = "Validation error"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn resend_verification_email(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ResendVerificationRequest>,
) -> ApiResult<Json<MessageResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;
    limit_by_client(&state, &headers, "resend-verification", 20, 15 * 60).await?;
    let accepted = || {
        Json(MessageResponse {
            message: RECOVERY_ACCEPTED.to_string(),
        })
    };

    let Some(user) = security::find_user_by_email(&state.db, &payload.email).await? else {
        return Ok(accepted());
    };
    if user.status != users::UserStatus::PendingVerification {
        return Ok(accepted());
    }

    // Per-account send limit; exceeding it is silent so the answer stays uniform.
    if state
        .otp_service
        .check_send_rate(&user.email, OtpType::Verification)
        .await
        .is_err()
    {
        return Ok(accepted());
    }

    // Generate new verification code
    let verification_code = generate_verification_code();

    // Store in Redis with TTL
    state
        .otp_service
        .store_code(&user.email, &verification_code, OtpType::Verification)
        .await?;

    // Send email
    if let Err(e) = state
        .email_service
        .send_verification_email(&user.email, &user.username, &verification_code)
        .await
    {
        tracing::error!(user_id = %user.id, "failed to resend verification email: {e}");
    }

    Ok(accepted())
}

/// Request password reset
///
/// Starts a password reset by emailing a 6-digit reset code, valid for 1 hour, to an
/// email-and-password account. Public endpoint. The answer is the same whether or not such an
/// account exists (no email is sent for unknown or Google sign-in accounts); at most 5 codes per
/// email are sent within 15 minutes. Finish with `POST /api/v1/auth/reset-password`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/forgot-password",
    tag = "Authentication",
    request_body = ForgotPasswordRequest,
    responses(
        (status = 200, description = "Request accepted", body = MessageResponse),
        (status = 400, description = "Validation error"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn forgot_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ForgotPasswordRequest>,
) -> ApiResult<Json<MessageResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;
    limit_by_client(&state, &headers, "forgot-password", 20, 15 * 60).await?;
    let accepted = || {
        Json(MessageResponse {
            message: RECOVERY_ACCEPTED.to_string(),
        })
    };

    let Some(user) = security::find_user_by_email(&state.db, &payload.email).await? else {
        return Ok(accepted());
    };

    // Only local accounts have a password to reset; others get the same answer.
    if user.auth_type != users::AuthType::Local
        || matches!(
            user.status,
            users::UserStatus::Inactive | users::UserStatus::Suspended
        )
    {
        return Ok(accepted());
    }

    if state
        .otp_service
        .check_send_rate(&user.email, OtpType::PasswordReset)
        .await
        .is_err()
    {
        return Ok(accepted());
    }

    // Generate 6-digit reset code
    let reset_code = generate_verification_code();

    // Store in Redis with TTL (1 hour)
    state
        .otp_service
        .store_code(&user.email, &reset_code, OtpType::PasswordReset)
        .await?;

    // Queue reset email
    match state
        .email_service
        .send_password_reset_email(&user.email, &user.username, &reset_code)
        .await
    {
        Ok(job_id) => {
            tracing::info!(user_id = %user.id, job_id = %job_id, "password reset email queued");
        }
        Err(e) => {
            tracing::error!(user_id = %user.id, "failed to queue password reset email: {e}");
        }
    }
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("PASSWORD_RESET_REQUESTED", Outcome::Success)
            .user(user.id)
            .actor(ActorKind::Anonymous, None),
    )
    .await;

    Ok(accepted())
}

/// Reset password with token
///
/// Sets a new password using the 6-digit code sent by `POST /api/v1/auth/forgot-password`. Public
/// endpoint. `new_password` must be 8-128 characters and match `confirm_password`. The code is
/// single-use; repeated wrong codes for the same email are rate-limited (429). A reset signs the
/// account out of every session.
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
    headers: HeaderMap,
    Json(payload): Json<ResetPasswordRequest>,
) -> ApiResult<Json<MessageResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;
    limit_by_client(&state, &headers, "reset-password", 30, 15 * 60).await?;
    let invalid = || ApiError::BadRequest("Invalid or expired reset code".to_string());

    let user = security::find_user_by_email(&state.db, &payload.email)
        .await?
        .ok_or_else(invalid)?;
    if user.auth_type != users::AuthType::Local {
        return Err(invalid());
    }

    // Verify token from Redis (includes rate limiting and auto-deletion on success)
    let is_valid = state
        .otp_service
        .verify_code(&user.email, &payload.token, OtpType::PasswordReset)
        .await?;

    if !is_valid {
        return Err(invalid());
    }

    // Update password. The account-security trigger advances the revocation epoch and
    // closes every session in the same transaction.
    let password_hash = hash_password(&payload.new_password)?;
    let user_id = user.id;

    let mut user: users::ActiveModel = user.into();
    user.password_hash = Set(Some(password_hash));
    user.updated_at = Set(now_utc());

    user.update(&state.db).await?;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("PASSWORD_RESET", Outcome::Success)
            .user(user_id)
            .actor(ActorKind::Anonymous, None),
    )
    .await;
    crate::services::communications::credentials_ended(&state, user_id, "PASSWORD_CHANGED").await;

    Ok(Json(MessageResponse {
        message: "Password reset successfully".to_string(),
    }))
}

/// Verify password reset token
///
/// Checks whether a password reset is pending for the given email, for example before showing the
/// reset form. Public endpoint. It only confirms that an unexpired reset code exists: the submitted
/// code must be 6 digits but is not compared or consumed (`POST /api/v1/auth/reset-password` does
/// that). Returns 400 if no reset is pending, including when the email is unknown.
#[utoipa::path(
    post,
    path = "/api/v1/auth/verify-reset-token",
    tag = "Authentication",
    request_body = VerifyResetTokenRequest,
    responses(
        (status = 200, description = "Token is valid", body = VerifyResetTokenResponse),
        (status = 400, description = "No pending reset"),
    )
)]
pub async fn verify_reset_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<VerifyResetTokenRequest>,
) -> ApiResult<Json<VerifyResetTokenResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;
    limit_by_client(&state, &headers, "verify-reset-token", 30, 15 * 60).await?;
    let none = || ApiError::BadRequest("No password reset request found".to_string());

    let user = security::find_user_by_email(&state.db, &payload.email)
        .await?
        .ok_or_else(none)?;

    // Check if reset code exists in Redis (without consuming it)
    let has_code = state
        .otp_service
        .has_code(&user.email, OtpType::PasswordReset)
        .await?;

    if !has_code {
        return Err(none());
    }

    // Note: We don't verify the actual code here to avoid consuming verification attempts
    // The actual verification happens in reset_password endpoint

    Ok(Json(VerifyResetTokenResponse {
        message: "Reset code exists. Please enter the code to reset your password.".to_string(),
        valid: true,
    }))
}

/// Change password for authenticated user
///
/// Changes the signed-in user's password after checking `current_password`. Requires a bearer
/// access token. `new_password` must be 8-128 characters and match `confirm_password`. Returns 400
/// if the current password is wrong or the account uses Google sign-in. A change signs the account
/// out of every session, including the current one; sign in again with the new password.
#[utoipa::path(
    post,
    path = "/api/v1/auth/change-password",
    tag = "Authentication",
    security(
        ("bearer_auth" = [])
    ),
    request_body = ChangePasswordRequest,
    responses(
        (status = 200, description = "Password changed successfully", body = MessageResponse),
        (status = 400, description = "Invalid current password or validation error"),
        (status = 401, description = "Unauthorized"),
    )
)]
pub async fn change_password(
    State(state): State<AppState>,
    axum::Extension(claims): axum::Extension<crate::services::auth::Claims>,
    Json(payload): Json<ChangePasswordRequest>,
) -> ApiResult<Json<MessageResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Get current user
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    let user = Users::find_by_id(user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".to_string()))?;

    // Check if user uses local auth (not OAuth)
    if user.auth_type != users::AuthType::Local {
        return Err(ApiError::BadRequest(
            "Password change is not available for OAuth accounts".to_string(),
        ));
    }

    // Verify current password
    let password_hash = user
        .password_hash
        .as_ref()
        .ok_or_else(|| ApiError::BadRequest("No password set for this account".to_string()))?;

    let is_valid = verify_password(&payload.current_password, password_hash)?;
    if !is_valid {
        security::record_event_best_effort(
            &state.db,
            SecurityEvent::new("PASSWORD_CHANGE", Outcome::Failure).by_user(user_id),
        )
        .await;
        return Err(ApiError::BadRequest(
            "Current password is incorrect".to_string(),
        ));
    }

    // Hash new password and update (the trigger ends every session and token).
    let new_password_hash = hash_password(&payload.new_password)?;

    let mut user_active: users::ActiveModel = user.into();
    user_active.password_hash = Set(Some(new_password_hash));
    user_active.updated_at = Set(now_utc());
    user_active.update(&state.db).await?;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("PASSWORD_CHANGED", Outcome::Success).by_user(user_id),
    )
    .await;
    crate::services::communications::credentials_ended(&state, user_id, "PASSWORD_CHANGED").await;

    Ok(Json(MessageResponse {
        message: "Password changed successfully".to_string(),
    }))
}

// ============================================================================
// Google OAuth Handlers (Blueprint Section 2: ACC-004, ACC-005, ACC-006)
// ============================================================================

use axum::response::{IntoResponse, Redirect, Response};
use rand::Rng;
use sea_orm::TransactionTrait;
use sha2::{Digest, Sha256};

/// OAuth state TTL (10 minutes)
const OAUTH_STATE_TTL_SECS: u64 = 10 * 60;
/// One-time sign-in exchange code TTL.
const OAUTH_EXCHANGE_TTL_SECS: u64 = 60;
/// Browser-binding cookie, scoped to the OAuth routes on the API host.
const OAUTH_BINDING_COOKIE: &str = "cfm_oauth_binding";
const OAUTH_COOKIE_PATH: &str = "/api/v1/auth/oauth";

fn random_token(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn digest_hex(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn binding_cookie(state: &AppState, value: &str, max_age: u64) -> String {
    let secure = if state.google_redirect_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{OAUTH_BINDING_COOKIE}={value}; Path={OAUTH_COOKIE_PATH}; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}"
    )
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn with_cookie(mut response: Response, cookie: &str) -> Response {
    if let Ok(value) = axum::http::HeaderValue::from_str(cookie) {
        response
            .headers_mut()
            .append(axum::http::header::SET_COOKIE, value);
    }
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Initiate Google OAuth flow
///
/// Generates single-use `state` and `nonce` values, binds them to this browser with an HttpOnly
/// cookie, and redirects the user to Google's consent page. Public endpoint.
#[utoipa::path(
    get,
    path = "/api/v1/auth/oauth/google",
    tag = "Authentication",
    responses(
        (status = 303, description = "Redirect to Google OAuth consent page"),
        (status = 500, description = "Internal server error"),
    )
)]
pub async fn oauth_google_init(State(state): State<AppState>) -> ApiResult<Response> {
    let state_value = random_token(43);
    let nonce = random_token(43);
    let binding = random_token(43);

    let record = serde_json::json!({
        "provider": "google",
        "nonce": nonce,
        "binding": digest_hex(&binding),
    });
    state
        .redis
        .set(
            &format!("oauth:state:{state_value}"),
            &record.to_string(),
            Some(OAUTH_STATE_TTL_SECS),
        )
        .await?;

    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope=openid%20email%20profile&state={}&nonce={}&prompt=select_account",
        urlencoding::encode(&state.google_client_id),
        urlencoding::encode(&state.google_redirect_url),
        urlencoding::encode(&state_value),
        urlencoding::encode(&nonce),
    );

    Ok(with_cookie(
        Redirect::to(&auth_url).into_response(),
        &binding_cookie(&state, &binding, OAUTH_STATE_TTL_SECS),
    ))
}

/// Google OAuth callback handler
///
/// Consumes the single-use state, checks the browser binding, exchanges the authorization code,
/// validates the ID token (issuer, audience, expiry, nonce, verified email) and signs the user in.
/// It then redirects to `{APP_URL}/google/callback`. With `GOOGLE_OAUTH_TOKEN_DELIVERY=exchange_code`
/// the redirect carries only a 60-second single-use `exchange_code` for
/// `POST /api/v1/auth/oauth/google/exchange`; the legacy mode (tokens in the query string) is kept
/// only until the live web client adopts the exchange. Errors redirect with a short `error` code.
#[utoipa::path(
    get,
    path = "/api/v1/auth/oauth/google/callback",
    tag = "Authentication",
    params(
        ("code" = String, Query, description = "Authorization code from Google"),
        ("state" = String, Query, description = "State token for CSRF protection"),
    ),
    responses(
        (status = 303, description = "Redirect to the web application's Google callback page"),
    )
)]
pub async fn oauth_google_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let frontend_url = state.app_url.clone();
    let clear = binding_cookie(&state, "", 0);
    let error_redirect = |code: &str| -> Response {
        with_cookie(
            Redirect::to(&format!(
                "{}/google/callback?error={}",
                frontend_url,
                urlencoding::encode(code)
            ))
            .into_response(),
            &clear,
        )
    };

    if params.contains_key("error") {
        return error_redirect("oauth_cancelled");
    }
    let (Some(code), Some(state_value)) = (params.get("code"), params.get("state")) else {
        return error_redirect("oauth_invalid_request");
    };
    let binding = cookie_value(&headers, OAUTH_BINDING_COOKIE);

    let (user, issued) =
        match oauth_google_callback_inner(&state, &headers, code, state_value, binding).await {
            Ok(done) => done,
            Err(e) => {
                let reason = match &e {
                    ApiError::Forbidden(_) => "account_disabled",
                    ApiError::Conflict(_) => "account_conflict",
                    ApiError::BadRequest(_) => "oauth_invalid_state",
                    _ => "oauth_failed",
                };
                tracing::warn!(reason, "google sign-in refused: {e}");
                return error_redirect(reason);
            }
        };
    let role = security::role_claim(&user);
    let status = status_string(&state, &user);

    let target = match state.auth_policy.google_token_delivery {
        security::GoogleTokenDelivery::ExchangeCode => {
            let exchange_code = random_token(48);
            let bundle = serde_json::json!({
                "access_token": issued.access_token,
                "refresh_token": issued.refresh_token,
                "user_id": user.id,
                "username": user.username,
                "role": role,
                "status": status,
            });
            if state
                .redis
                .set(
                    &format!("oauth:exchange:{}", digest_hex(&exchange_code)),
                    &bundle.to_string(),
                    Some(OAUTH_EXCHANGE_TTL_SECS),
                )
                .await
                .is_err()
            {
                return error_redirect("oauth_failed");
            }
            format!(
                "{}/google/callback?exchange_code={}",
                frontend_url,
                urlencoding::encode(&exchange_code)
            )
        }
        security::GoogleTokenDelivery::LegacyQuery => format!(
            "{}/google/callback?access_token={}&refresh_token={}&user_id={}&username={}&role={}&status={}",
            frontend_url,
            urlencoding::encode(&issued.access_token),
            urlencoding::encode(&issued.refresh_token),
            urlencoding::encode(&user.id.to_string()),
            urlencoding::encode(&user.username),
            urlencoding::encode(&role),
            urlencoding::encode(&status),
        ),
    };
    with_cookie(Redirect::to(&target).into_response(), &clear)
}

/// Exchange a one-time Google sign-in code for tokens
///
/// Completes Google sign-in when the API runs with `GOOGLE_OAUTH_TOKEN_DELIVERY=exchange_code`.
/// The code from the callback redirect is valid once, for 60 seconds. Returns the same body as
/// `POST /api/v1/auth/login`. Public endpoint.
#[utoipa::path(
    post,
    path = "/api/v1/auth/oauth/google/exchange",
    tag = "Authentication",
    request_body = GoogleExchangeRequest,
    responses(
        (status = 200, description = "Signed in", body = LoginResponse),
        (status = 400, description = "Invalid or expired exchange code"),
    )
)]
pub async fn oauth_google_exchange(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<GoogleExchangeRequest>,
) -> ApiResult<Json<LoginResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;
    limit_by_client(&state, &headers, "oauth-exchange", 60, 15 * 60).await?;
    let invalid = || ApiError::BadRequest("Invalid or expired exchange code".to_string());
    let raw = state
        .redis
        .take(&format!(
            "oauth:exchange:{}",
            digest_hex(&payload.exchange_code)
        ))
        .await?
        .ok_or_else(invalid)?;
    let bundle: serde_json::Value = serde_json::from_str(&raw).map_err(|_| invalid())?;
    let text = |k: &str| bundle[k].as_str().map(str::to_string).ok_or_else(invalid);
    Ok(Json(LoginResponse {
        access_token: text("access_token")?,
        refresh_token: text("refresh_token")?,
        user: UserInfo {
            id: Uuid::parse_str(&text("user_id")?).map_err(|_| invalid())?,
            username: text("username")?,
            role: text("role")?,
            status: text("status")?,
        },
    }))
}

/// A Google identity established from a validated ID token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleIdentity {
    pub subject: String,
    pub email: String,
    pub name: String,
}

/// Validate ID-token claims. The token comes straight from Google's token endpoint over TLS,
/// authenticated with our client secret (OpenID Connect Core §3.1.3.7 allows TLS server
/// validation in place of the signature in that case); every claim is still checked.
pub fn validate_google_claims(
    claims: &serde_json::Value,
    client_id: &str,
    expected_nonce: Option<&str>,
    now: i64,
) -> Result<GoogleIdentity, &'static str> {
    let iss = claims["iss"].as_str().unwrap_or_default();
    if iss != "https://accounts.google.com" && iss != "accounts.google.com" {
        return Err("issuer");
    }
    let audience_ok = match &claims["aud"] {
        serde_json::Value::String(a) => a == client_id,
        serde_json::Value::Array(list) => list.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience_ok {
        return Err("audience");
    }
    if let Some(azp) = claims["azp"].as_str() {
        if azp != client_id {
            return Err("authorized_party");
        }
    }
    let exp = claims["exp"].as_i64().ok_or("expiry")?;
    if exp + 60 < now {
        return Err("expired");
    }
    if let Some(iat) = claims["iat"].as_i64() {
        if iat > now + 300 {
            return Err("issued_in_future");
        }
    }
    if let Some(expected) = expected_nonce {
        if claims["nonce"].as_str() != Some(expected) {
            return Err("nonce");
        }
    }
    let verified = match &claims["email_verified"] {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => s == "true",
        _ => false,
    };
    if !verified {
        return Err("email_unverified");
    }
    let subject = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 255)
        .ok_or("subject")?
        .to_string();
    let email = claims["email"]
        .as_str()
        .map(security::normalize_email)
        .filter(|e| e.contains('@') && e.len() <= 100)
        .ok_or("email")?;
    let name = claims["name"].as_str().unwrap_or_default().to_string();
    Ok(GoogleIdentity {
        subject,
        email,
        name,
    })
}

/// A username that satisfies the registration rules (3-50 letters, digits, underscores).
pub fn username_base(name: &str, email: &str) -> String {
    fn clean(raw: &str) -> String {
        let mut out = String::new();
        for c in raw.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c);
            } else if matches!(c, ' ' | '_' | '-' | '.') && !out.ends_with('_') {
                out.push('_');
            }
        }
        out.trim_matches('_')
            .chars()
            .take(40)
            .collect::<String>()
            .trim_end_matches('_')
            .to_string()
    }
    let from_name = clean(name);
    if from_name.len() >= 3 {
        return from_name;
    }
    let from_email = clean(email.split('@').next().unwrap_or_default());
    if from_email.len() >= 3 {
        return from_email;
    }
    "user".to_string()
}

async fn unique_username(state: &AppState, name: &str, email: &str) -> ApiResult<String> {
    let base = username_base(name, email);
    for attempt in 0..8 {
        let candidate = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}_{}", rand::thread_rng().gen_range(1000..1_000_000))
        };
        let taken = Users::find()
            .filter(users::Column::Username.eq(&candidate))
            .one(&state.db)
            .await?
            .is_some();
        if !taken {
            return Ok(candidate);
        }
    }
    Ok(format!(
        "user_{}",
        &Uuid::new_v4().simple().to_string()[..12]
    ))
}

/// Inner logic for the Google callback: returns the signed-in user and its new session.
async fn oauth_google_callback_inner(
    state: &AppState,
    headers: &HeaderMap,
    code: &str,
    state_value: &str,
    binding: Option<String>,
) -> ApiResult<(users::Model, security::IssuedSession)> {
    // Single-use state, consumed atomically.
    let raw = state
        .redis
        .take(&format!("oauth:state:{state_value}"))
        .await?
        .ok_or_else(|| ApiError::BadRequest("Invalid or expired state".to_string()))?;
    let record: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|_| ApiError::BadRequest("Invalid or expired state".to_string()))?;
    if record["provider"].as_str() != Some("google") {
        return Err(ApiError::BadRequest("Invalid OAuth provider".to_string()));
    }
    if state.auth_policy.google_browser_binding {
        let presented = binding.map(|b| digest_hex(&b));
        if presented.is_none() || presented.as_deref() != record["binding"].as_str() {
            return Err(ApiError::BadRequest(
                "Sign-in was started in a different browser".to_string(),
            ));
        }
    }

    // Exchange code for tokens
    let token_response = reqwest::Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", code),
            ("client_id", state.google_client_id.as_str()),
            ("client_secret", state.google_client_secret.as_str()),
            ("redirect_uri", state.google_redirect_url.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| ApiError::InternalError(format!("Token exchange failed: {e}")))?;
    if !token_response.status().is_success() {
        return Err(ApiError::Unauthorized(format!(
            "Token exchange refused ({})",
            token_response.status()
        )));
    }
    let token_data: serde_json::Value = token_response
        .json()
        .await
        .map_err(|e| ApiError::InternalError(format!("Failed to parse token response: {e}")))?;
    let id_token = token_data["id_token"]
        .as_str()
        .ok_or_else(|| ApiError::Unauthorized("No ID token in response".to_string()))?;
    let payload = id_token
        .split('.')
        .nth(1)
        .filter(|_| id_token.split('.').count() == 3)
        .ok_or_else(|| ApiError::Unauthorized("Invalid ID token format".to_string()))?;
    use base64::{engine::general_purpose, Engine as _};
    let claims: serde_json::Value = general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| ApiError::Unauthorized("Invalid ID token".to_string()))?;
    let identity = validate_google_claims(
        &claims,
        &state.google_client_id,
        record["nonce"].as_str(),
        chrono::Utc::now().timestamp(),
    )
    .map_err(|reason| ApiError::Unauthorized(format!("ID token rejected: {reason}")))?;

    let (user, event) = resolve_google_account(state, &identity).await?;
    let issued = security::start_session(
        state,
        &user,
        AuthMethod::Google,
        None,
        security::client_label(headers),
    )
    .await?;
    let mut user_active: users::ActiveModel = user.clone().into();
    user_active.last_login = Set(Some(now_utc()));
    let user = user_active.update(&state.db).await?;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new(event, Outcome::Success)
            .by_user(user.id)
            .session(issued.session_id),
    )
    .await;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("LOGIN", Outcome::Success)
            .by_user(user.id)
            .session(issued.session_id)
            .detail(serde_json::json!({ "method": "GOOGLE" })),
    )
    .await;
    Ok((user, issued))
}

/// Find or create the account for a verified Google identity (ACC-004).
///
/// * A bound Google subject always wins.
/// * An unbound account with the same email is linked only when that is safe:
///   - an ACTIVE account (its email was already verified with CF Meet) gets the subject;
///   - a PENDING password account was never verified, so its password may belong to whoever
///     registered the address first: the password is discarded (ending any of its sessions)
///     before the Google owner takes the account;
///   - an account bound to a different Google subject is refused.
pub async fn resolve_google_account(
    state: &AppState,
    identity: &GoogleIdentity,
) -> ApiResult<(users::Model, &'static str)> {
    let disabled = |u: &users::Model| {
        matches!(
            u.status,
            users::UserStatus::Inactive | users::UserStatus::Suspended
        )
    };
    if let Some(user_id) = security::user_for_google_subject(&state.db, &identity.subject).await? {
        let user = Users::find_by_id(user_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| ApiError::Unauthorized("Account is unavailable".into()))?;
        if disabled(&user) {
            return Err(ApiError::Forbidden("Account has been deactivated".into()));
        }
        return Ok((user, "GOOGLE_SIGN_IN"));
    }

    match security::find_user_by_email(&state.db, &identity.email).await? {
        Some(user) => {
            if disabled(&user) {
                return Err(ApiError::Forbidden("Account has been deactivated".into()));
            }
            if security::google_subject_for_user(&state.db, user.id)
                .await?
                .is_some()
            {
                return Err(ApiError::Conflict(
                    "This account is linked to a different Google identity".into(),
                ));
            }
            let txn = state.db.begin().await?;
            let (user, event) = if user.status == users::UserStatus::PendingVerification {
                let mut claim: users::ActiveModel = user.into();
                claim.password_hash = Set(None);
                claim.auth_type = Set(users::AuthType::Google);
                claim.status = Set(users::UserStatus::Active);
                claim.updated_at = Set(now_utc());
                (
                    claim.update(&txn).await?,
                    "GOOGLE_CLAIMED_UNVERIFIED_ACCOUNT",
                )
            } else {
                (user, "GOOGLE_LINKED")
            };
            security::bind_google_subject(&txn, user.id, &identity.subject).await?;
            // Google proved the address (claims validated, `email_verified`).
            crate::services::account_profile::mark_email_verified(&txn, user.id).await?;
            txn.commit().await?;
            Ok((user, event))
        }
        None => {
            let username = unique_username(state, &identity.name, &identity.email).await?;
            let txn = state.db.begin().await?;
            let user = users::ActiveModel {
                id: Set(Uuid::new_v4()),
                username: Set(username),
                email: Set(identity.email.clone()),
                password_hash: Set(None),
                auth_type: Set(users::AuthType::Google),
                role: Set(users::UserRole::User),
                status: Set(users::UserStatus::Active),
                created_at: Set(now_utc()),
                updated_at: Set(now_utc()),
                last_login: Set(None),
                // Google proved the address (claims validated, `email_verified`).
                email_verified_at: Set(Some(chrono::Utc::now())),
                ..Default::default()
            }
            .insert(&txn)
            .await?;
            security::bind_google_subject(&txn, user.id, &identity.subject).await?;
            txn.commit().await?;
            Ok((user, "GOOGLE_ACCOUNT_CREATED"))
        }
    }
}

#[cfg(test)]
mod google_tests {
    use super::*;

    fn claims(overrides: serde_json::Value) -> serde_json::Value {
        let mut base = serde_json::json!({
            "iss": "https://accounts.google.com",
            "aud": "client-1",
            "azp": "client-1",
            "exp": 2_000,
            "iat": 1_000,
            "nonce": "n-1",
            "email_verified": true,
            "sub": "1234567890",
            "email": "Alice@Example.com",
            "name": "Alice Example",
        });
        for (k, v) in overrides.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    #[test]
    fn accepts_a_complete_identity_and_normalizes_email() {
        let id = validate_google_claims(
            &claims(serde_json::json!({})),
            "client-1",
            Some("n-1"),
            1_500,
        )
        .unwrap();
        assert_eq!(id.subject, "1234567890");
        assert_eq!(id.email, "alice@example.com");
    }

    #[test]
    fn refuses_every_weak_or_forged_claim() {
        let cases = [
            (serde_json::json!({"iss": "https://evil.example"}), "issuer"),
            (serde_json::json!({"aud": "other-client"}), "audience"),
            (serde_json::json!({"aud": ["x", "y"]}), "audience"),
            (
                serde_json::json!({"azp": "other-client"}),
                "authorized_party",
            ),
            (serde_json::json!({"exp": 1_000}), "expired"),
            (serde_json::json!({"nonce": "replayed"}), "nonce"),
            (
                serde_json::json!({"email_verified": false}),
                "email_unverified",
            ),
            (
                serde_json::json!({"email_verified": null}),
                "email_unverified",
            ),
            (serde_json::json!({"sub": ""}), "subject"),
        ];
        for (override_, reason) in cases {
            assert_eq!(
                validate_google_claims(&claims(override_), "client-1", Some("n-1"), 1_500),
                Err(reason)
            );
        }
        assert!(validate_google_claims(
            &claims(serde_json::json!({"aud": ["x", "client-1"], "azp": "client-1"})),
            "client-1",
            Some("n-1"),
            1_500
        )
        .is_ok());
    }

    #[test]
    fn usernames_are_valid_for_common_and_long_names() {
        assert_eq!(username_base("Alice Example", "a@x.com"), "Alice_Example");
        assert_eq!(username_base("José", "jose.p@x.com"), "Jos");
        assert_eq!(username_base("李", "li.wei@x.com"), "li_wei");
        assert_eq!(username_base("", "@x.com"), "user");
        let long = username_base(&"a".repeat(200), "x@x.com");
        assert_eq!(long.len(), 40);
        for name in [
            "Alice Example",
            "José",
            "李",
            "",
            "--__..",
            &"b ".repeat(80),
        ] {
            let u = username_base(name, "someone@example.com");
            assert!((3..=40).contains(&u.len()), "{u}");
            assert!(
                u.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{u}"
            );
        }
    }
}
