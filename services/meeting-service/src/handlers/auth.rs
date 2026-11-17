use crate::validation::ValidateRequest;
use axum::{extract::State, http::HeaderMap, Json};
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use crate::{
    dto::*,
    error::{ApiError, ApiResult},
    models::users::{self, Entity as Users},
    services::account_security::{
        self as security, ActorKind, AuthMethod, Outcome, RevokeReason, SecurityEvent,
    },
    services::auth::{Claims, TokenKind},
    services::ops_metrics::{inc, OPS_METRICS as OM},
    services::otp::{generate_code, generate_link_token, OtpCheck, OtpType},
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

/// Uniform answer of `POST /api/v1/auth/otp/request`, whether or not an account exists,
/// is disabled, or is over its send limit (ACC-014).
const CODE_SENT: &str =
    "If this address can receive email, a sign-in code is on its way. It expires in 10 minutes.";

/// Uniform answer to a wrong, expired, used or replaced code or link.
const INVALID_CODE: &str = "Invalid or expired code";

fn disabled(user: &users::Model) -> bool {
    matches!(
        user.status,
        users::UserStatus::Inactive | users::UserStatus::Suspended
    )
}

/// Request a sign-in code
///
/// Passwordless sign-in and sign-up in one flow. Emails a 6-digit code (valid 10 minutes) and a
/// one-click sign-in link to the address; finish with `POST /api/v1/auth/otp/verify` (code) or
/// `POST /api/v1/auth/otp/verify-link` (link). If no account exists yet, the first successful
/// verification creates it. Public endpoint. The answer is identical whether or not an account
/// exists (no email is sent to deactivated accounts or past the limit of 5 codes per address per
/// 15 minutes). A new request replaces the previous code and link. The email states when and from
/// which browser the code was requested.
#[utoipa::path(
    post,
    path = "/api/v1/auth/otp/request",
    tag = "Authentication",
    request_body = OtpRequest,
    responses(
        (status = 200, description = "Request accepted (same answer for every address)", body = MessageResponse),
        (status = 400, description = "Validation error"),
        (status = 429, description = "Too many requests from this network"),
    )
)]
pub async fn request_otp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<OtpRequest>,
) -> ApiResult<Json<MessageResponse>> {
    payload.validate_request()?;
    limit_by_client(&state, &headers, "otp-request", 20, 15 * 60).await?;
    inc(&OM.auth_otp_requested);
    let accepted = || {
        Json(MessageResponse {
            message: CODE_SENT.to_string(),
        })
    };
    let email = security::normalize_email(&payload.email);

    // Fixed-code review / acceptance accounts never receive email.
    if state.auth_policy.test_accounts.is_test_account(&email) {
        return Ok(accepted());
    }
    if let Some(user) = security::find_user_by_email(&state.db, &email).await? {
        if disabled(&user) {
            return Ok(accepted());
        }
    }
    // Per-address send limit; exceeding it is silent so the answer stays uniform.
    if state
        .otp_service
        .check_send_rate(&email, OtpType::SignIn)
        .await
        .is_err()
    {
        return Ok(accepted());
    }

    let code = generate_code();
    let link_token = generate_link_token();
    state
        .otp_service
        .store_challenge(&email, &code, Some(&link_token), OtpType::SignIn)
        .await?;
    let details = crate::services::passwordless::request_details(&state.auth_policy, &headers);
    match state
        .email_service
        .send_sign_in_code(&email, &code, Some(&link_token), &details)
        .await
    {
        Ok(_) => inc(&OM.auth_otp_sent),
        // A malformed or undeliverable address: nothing to send, same answer.
        Err(e) => tracing::warn!("sign-in code not queued: {e}"),
    }
    Ok(accepted())
}

/// Verify a sign-in code
///
/// Finishes a passwordless sign-in with the 6-digit code from `POST /api/v1/auth/otp/request`.
/// Returns an access token (valid 15 minutes), a refresh token (valid 7 days, rotated on every
/// refresh), basic user info and `is_new_account` (true when this sign-in created the account,
/// with an automatically generated username the user can change later). Each sign-in opens an
/// account session (at most 30 days) listed under `/api/v2/me/sessions`. Public endpoint.
///
/// The code is single-use and consumes the matching link too. A wrong, expired, used or replaced
/// code gets the same 400 answer whether or not an account exists. After 5 checks within 15
/// minutes the pending code is destroyed and the answer is 429 (requesting a new code does not
/// reset this). A correct code for a deactivated account gets 403. A correct code also verifies
/// the address of an account still pending verification. A sign-in from a device not seen on
/// the account in the last 90 days triggers a security alert email.
#[utoipa::path(
    post,
    path = "/api/v1/auth/otp/verify",
    tag = "Authentication",
    request_body = OtpVerifyRequest,
    responses(
        (status = 200, description = "Signed in", body = OtpVerifyResponse),
        (status = 400, description = "Invalid or expired code"),
        (status = 403, description = "Account has been deactivated"),
        (status = 429, description = "Too many attempts"),
    )
)]
pub async fn verify_otp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<OtpVerifyRequest>,
) -> ApiResult<Json<OtpVerifyResponse>> {
    payload.validate_request()?;
    limit_by_client(&state, &headers, "otp-verify", 30, 15 * 60).await?;
    let email = security::normalize_email(&payload.email);

    let test_account = match state.auth_policy.test_accounts.check(&email, &payload.code) {
        Some(true) => true,
        Some(false) => {
            inc(&OM.auth_otp_wrong);
            return Err(ApiError::BadRequest(INVALID_CODE.to_string()));
        }
        None => {
            let outcome = state
                .otp_service
                .check(&email, &payload.code, OtpType::SignIn)
                .await;
            settle_check(&state, &email, "CODE", outcome)?;
            false
        }
    };
    complete_sign_in(&state, &headers, &email, "CODE", test_account)
        .await
        .map(Json)
}

/// Verify a sign-in link
///
/// Finishes a passwordless sign-in with the one-click link from the same email as the code. The
/// link opens the web app (`/auth/magic?email=…&token=…`), which POSTs both values here; the link
/// itself never signs in on a GET, so email scanners that open links cannot use it up. Same
/// answers, limits and response as `POST /api/v1/auth/otp/verify`; the link and the code are one
/// challenge, so using either consumes both.
#[utoipa::path(
    post,
    path = "/api/v1/auth/otp/verify-link",
    tag = "Authentication",
    request_body = OtpVerifyLinkRequest,
    responses(
        (status = 200, description = "Signed in", body = OtpVerifyResponse),
        (status = 400, description = "Invalid or expired link"),
        (status = 403, description = "Account has been deactivated"),
        (status = 429, description = "Too many attempts"),
    )
)]
pub async fn verify_otp_link(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<OtpVerifyLinkRequest>,
) -> ApiResult<Json<OtpVerifyResponse>> {
    payload.validate_request()?;
    limit_by_client(&state, &headers, "otp-verify-link", 30, 15 * 60).await?;
    let email = security::normalize_email(&payload.email);
    let outcome = state
        .otp_service
        .check_link(&email, &payload.token, OtpType::SignIn)
        .await;
    settle_check(&state, &email, "LINK", outcome)?;
    inc(&OM.auth_magic_link_verified);
    complete_sign_in(&state, &headers, &email, "LINK", false)
        .await
        .map(Json)
}

/// Finish a sign-in with the second factor
///
/// Gap audit A1: when an account has an authenticator app, `POST /api/v1/auth/otp/verify` (and
/// `/otp/verify-link`) answer `401 MFA_REQUIRED` with a one-time `mfaTicket` (valid 5 minutes)
/// instead of tokens. Send it here with the 6-digit code from the authenticator app, or one of
/// the account's recovery codes; the answer is then exactly that of a completed sign-in. A wrong
/// code gets 400; after 5 attempts the ticket is destroyed (429: sign in again).
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/verify",
    tag = "Authentication",
    request_body = MfaVerifyRequest,
    responses(
        (status = 200, description = "Signed in", body = OtpVerifyResponse),
        (status = 400, description = "Invalid or expired code or ticket"),
        (status = 429, description = "Too many attempts"),
    )
)]
pub async fn verify_mfa(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<MfaVerifyRequest>,
) -> ApiResult<Json<OtpVerifyResponse>> {
    limit_by_client(&state, &headers, "mfa-verify", 30, 15 * 60).await?;
    let pending = crate::services::mfa::redeem(&state, &payload.mfa_ticket, &payload.code).await?;
    let user = Users::find_by_id(pending.user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::BadRequest("Invalid or expired code".into()))?;
    if disabled(&user) {
        return Err(ApiError::Forbidden(
            "Account has been deactivated".to_string(),
        ));
    }
    let via: &'static str = match pending.via.as_str() {
        "LINK" => "LINK",
        "GOOGLE" => "GOOGLE",
        _ => "CODE",
    };
    finish_sign_in(
        &state,
        &headers,
        user,
        pending.created,
        via,
        pending.test_account,
    )
    .await
    .map(Json)
}

/// Gap audit A1: the second factor of a parked sign-in.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MfaVerifyRequest {
    /// The `mfaTicket` of the `401 MFA_REQUIRED` answer.
    pub mfa_ticket: String,
    /// A 6-digit authenticator code, or a recovery code (`xxxxx-xxxxx`).
    pub code: String,
}

/// Map a code/link check to the uniform answers (and count it). Gap audit A24: a wrong
/// code and a lock-out are audited for an existing account, off the request path so the
/// answer's timing never depends on whether the account exists.
fn settle_check(
    state: &AppState,
    email: &str,
    via: &'static str,
    outcome: ApiResult<OtpCheck>,
) -> ApiResult<()> {
    let audit = |outcome: Outcome, reason: &'static str| {
        let db = state.db.clone();
        let email = email.to_owned();
        tokio::spawn(async move {
            if let Ok(Some(user)) = security::find_user_by_email(&db, &email).await {
                security::record_event_best_effort(
                    &db,
                    SecurityEvent::new("SIGN_IN_CODE", outcome)
                        .user(user.id)
                        .actor(ActorKind::Anonymous, None)
                        .detail(serde_json::json!({ "via": via, "reason": reason })),
                )
                .await;
            }
        });
    };
    match outcome {
        Ok(OtpCheck::Valid) => Ok(()),
        Ok(OtpCheck::Missing) => {
            inc(&OM.auth_otp_expired);
            Err(ApiError::BadRequest(INVALID_CODE.to_string()))
        }
        Ok(OtpCheck::Wrong) => {
            inc(&OM.auth_otp_wrong);
            audit(Outcome::Failure, "WRONG_CODE");
            Err(ApiError::BadRequest(INVALID_CODE.to_string()))
        }
        Err(e @ ApiError::TooManyRequests(_)) => {
            inc(&OM.auth_otp_locked);
            audit(Outcome::Denied, "TOO_MANY_ATTEMPTS");
            Err(e)
        }
        Err(e) => Err(e),
    }
}

/// The address was proven (code, link or fixed test code): find or create the account,
/// open a session and send the security emails that apply.
async fn complete_sign_in(
    state: &AppState,
    headers: &HeaderMap,
    email: &str,
    via: &'static str,
    test_account: bool,
) -> ApiResult<OtpVerifyResponse> {
    let (mut user, created) = find_or_create_account(state, email).await?;

    if disabled(&user) {
        security::record_event_best_effort(
            &state.db,
            SecurityEvent::new("LOGIN", Outcome::Denied)
                .user(user.id)
                .actor(ActorKind::Anonymous, None)
                .detail(serde_json::json!({ "method": "EMAIL_OTP", "reason": "ACCOUNT_DISABLED" })),
        )
        .await;
        return Err(ApiError::Forbidden(
            "Account has been deactivated".to_string(),
        ));
    }

    // The code proved the address: a pending account becomes active and verified.
    if user.status == users::UserStatus::PendingVerification {
        let mut active: users::ActiveModel = user.into();
        active.status = Set(users::UserStatus::Active);
        active.email_verified_at = Set(Some(chrono::Utc::now()));
        active.updated_at = Set(now_utc());
        user = active.update(&state.db).await?;
        security::record_event_best_effort(
            &state.db,
            SecurityEvent::new("EMAIL_VERIFIED", Outcome::Success).by_user(user.id),
        )
        .await;
    } else if user.email_verified_at.is_none() {
        crate::services::account_profile::mark_email_verified(&state.db, user.id).await?;
    }

    // Gap audit A1: an account with an authenticator finishes with the second factor.
    if !test_account && crate::services::mfa::enabled(&state.db, user.id).await? {
        let pending = crate::services::mfa::Pending {
            user_id: user.id,
            via: via.to_string(),
            created,
            test_account,
        };
        return Err(crate::services::mfa::challenge(state, &pending).await?);
    }
    finish_sign_in(state, headers, user, created, via, test_account).await
}

/// The sign-in after every proof (address, and second factor when enabled): session,
/// audit and the security emails that apply.
async fn finish_sign_in(
    state: &AppState,
    headers: &HeaderMap,
    user: users::Model,
    created: bool,
    via: &str,
    test_account: bool,
) -> ApiResult<OtpVerifyResponse> {
    let label = security::client_label(headers);
    let issued =
        security::start_session(state, &user, AuthMethod::EmailOtp, None, label.clone()).await?;
    let mut user_active: users::ActiveModel = user.clone().into();
    user_active.last_login = Set(Some(now_utc()));
    let user = user_active.update(&state.db).await?;

    let mut detail = serde_json::json!({ "method": "EMAIL_OTP", "via": via });
    if test_account {
        detail["test_account"] = serde_json::Value::Bool(true);
        inc(&OM.auth_test_account_sign_in);
    }
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("LOGIN", Outcome::Success)
            .by_user(user.id)
            .session(issued.session_id)
            .detail(detail),
    )
    .await;
    inc(&OM.auth_otp_verified);

    if created {
        if !test_account {
            if let Err(e) = state
                .email_service
                .send_welcome_email(&user.email, &user.username, user.id)
                .await
            {
                tracing::warn!(user_id = %user.id, "welcome email not queued: {e}");
            }
        }
    } else if !test_account
        && !crate::services::passwordless::is_known_device(
            &state.db,
            user.id,
            issued.session_id,
            label.as_deref(),
        )
        .await?
    {
        let details = crate::services::passwordless::request_details(&state.auth_policy, headers);
        match state
            .email_service
            .send_new_sign_in_alert(&user.email, &user.username, issued.session_id, &details)
            .await
        {
            Ok(_) => inc(&OM.auth_new_sign_in_alerts),
            Err(e) => tracing::warn!(user_id = %user.id, "new sign-in alert not queued: {e}"),
        }
    }

    Ok(OtpVerifyResponse {
        access_token: issued.access_token,
        refresh_token: issued.refresh_token,
        user: UserInfo {
            id: user.id,
            username: user.username.clone(),
            role: security::role_claim(&user),
            status: status_string(state, &user),
        },
        is_new_account: created,
    })
}

/// A random public username (`user_` + 10 lower-case letters/digits). Never derived from
/// the email address, which usernames would otherwise reveal; the owner can change it.
fn random_username() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    let suffix: String = (0..10)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect();
    format!("user_{suffix}")
}

/// The account for a proven address, created on first sign-in. Serialized per address
/// (advisory lock) so two simultaneous first sign-ins create one account.
async fn find_or_create_account(state: &AppState, email: &str) -> ApiResult<(users::Model, bool)> {
    if let Some(user) = security::find_user_by_email(&state.db, email).await? {
        return Ok((user, false));
    }
    let txn = state.db.begin().await?;
    txn.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        [format!("account-email:{email}").into()],
    ))
    .await?;
    if let Some(user) = security::find_user_by_email(&txn, email).await? {
        txn.commit().await?;
        return Ok((user, false));
    }
    // Several legacy rows differing only by case cannot be told apart: never add another.
    let ambiguous = Users::find()
        .filter(
            sea_orm::sea_query::Expr::expr(sea_orm::sea_query::Func::lower(
                sea_orm::sea_query::Expr::col(users::Column::Email),
            ))
            .eq(email),
        )
        .one(&txn)
        .await?;
    if ambiguous.is_some() {
        return Err(ApiError::Conflict(
            "This address matches several accounts; contact support".to_string(),
        ));
    }
    let mut username = random_username();
    for _ in 0..4 {
        let taken = security::username_taken(&txn, &username, None).await?;
        if !taken {
            break;
        }
        username = random_username();
    }
    let user = users::ActiveModel {
        id: Set(Uuid::new_v4()),
        username: Set(username),
        email: Set(email.to_string()),
        password_hash: Set(None),
        auth_type: Set(users::AuthType::Local),
        role: Set(users::UserRole::User),
        status: Set(users::UserStatus::Active),
        created_at: Set(now_utc()),
        updated_at: Set(now_utc()),
        last_login: Set(None),
        // The code proved the address.
        email_verified_at: Set(Some(chrono::Utc::now())),
        ..Default::default()
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("ACCOUNT_REGISTERED", Outcome::Success)
            .by_user(user.id)
            .detail(serde_json::json!({ "method": "EMAIL_OTP" })),
    )
    .await;
    inc(&OM.auth_account_created);
    Ok((user, true))
}

/// Undo an email change
///
/// Restores the previous email address of an account with the single-use token from the "your
/// email address was changed" notice (valid 7 days), and signs out every session and linked device
/// of the account. The owner then signs in with the previous address. Public endpoint (the token is
/// the proof); the web app's `/auth/email-change/undo` page POSTs it. Returns 409 if the previous
/// address has since been taken by another account (support then helps).
#[utoipa::path(
    post,
    path = "/api/v1/auth/email-change/revert",
    tag = "Authentication",
    request_body = EmailChangeRevertRequest,
    responses(
        (status = 200, description = "Change undone; every session ended", body = MessageResponse),
        (status = 400, description = "Invalid, used or expired link"),
        (status = 409, description = "The previous address belongs to another account"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn revert_email_change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<EmailChangeRevertRequest>,
) -> ApiResult<Json<MessageResponse>> {
    payload.validate_request()?;
    limit_by_client(&state, &headers, "email-change-revert", 20, 15 * 60).await?;
    crate::services::email_change::revert(&state, &payload.token).await?;
    Ok(Json(MessageResponse {
        message: "The email change was undone. Sign in with your previous address.".to_string(),
    }))
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
    payload.validate_request()?;
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
    // A new mobile awaiting its primary-mobile replacement may keep its (restricted)
    // sign-in alive; every other endpoint still refuses it.
    security::authorize_user_token_allow_pending(&state, &claims, user.id, TokenKind::Refresh)
        .await?;

    let (access_token, refresh_token) = if claims.session_id().is_some() {
        security::rotate_session(&state, &claims, &user, payload.device_proof.as_ref()).await?
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
    // sessions (durable job; a Messaging outage never blocks the logout). Gap audit A11:
    // a sign-in with a session id ends only its own Messaging sessions (done above through
    // `session_ended`); only a legacy token, whose logout already ends every credential,
    // ends every Messaging client session of the account.
    if claims.session_id().is_none() {
        if let (Some(um), Ok(user_id)) = (
            state.meeting.core.unified.as_ref(),
            Uuid::parse_str(&claims.sub),
        ) {
            um.revoke_sessions_for_user(user_id, "LOGOUT").await;
        }
    }

    Ok(Json(MessageResponse {
        message: "Logged out successfully".to_string(),
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
            Err(ApiError::MfaRequired { ticket }) => {
                // Gap audit A1: the web app finishes with POST /api/v1/auth/mfa/verify.
                return with_cookie(
                    Redirect::to(&format!(
                        "{}/google/callback?mfa_ticket={}",
                        frontend_url,
                        urlencoding::encode(&ticket)
                    ))
                    .into_response(),
                    &clear,
                );
            }
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
/// `POST /api/v1/auth/otp/verify` (without `is_new_account`). Public endpoint.
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
    payload.validate_request()?;
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
        let taken = security::username_taken(&state.db, &candidate, None).await?;
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
    // Gap audit A1: Google proves the address, not the second factor.
    if crate::services::mfa::enabled(&state.db, user.id).await? {
        let pending = crate::services::mfa::Pending {
            user_id: user.id,
            via: "GOOGLE".into(),
            created: false,
            test_account: false,
        };
        return Err(crate::services::mfa::challenge(state, &pending).await?);
    }
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
    // Gap audit A13: linking Google to an existing account is a new way in.
    if matches!(event, "GOOGLE_LINKED" | "GOOGLE_CLAIMED_UNVERIFIED_ACCOUNT") {
        security::notify_security(
            state,
            user.id,
            crate::services::email::SecurityNotice::GoogleLinked,
            &issued.session_id.to_string(),
        )
        .await;
    }
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
