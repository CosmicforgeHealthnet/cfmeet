//! Secure email change. With passwordless sign-in the email address IS the credential, so
//! changing it proves three things:
//!
//! 1. the requester controls the account's CURRENT mailbox — a step-up token for
//!    `CHANGE_EMAIL` (see [`crate::services::step_up`]), checked by the handler;
//! 2. the requester controls the NEW mailbox — a code sent there ([`request`] /
//!    [`confirm`]);
//! 3. the previous owner can take it back — the old address gets a single-use undo link
//!    valid for 7 days ([`revert`]), which restores the address and signs out every device.
//!
//! On success every OTHER session ends. One self-service change per account per 7 days.
//! An address that already has an account gets an "attempt" notice instead of a code, and
//! the requester the same answer, so the flow never reveals which addresses are
//! registered. Owners who lost their old mailbox go through support ([`admin_change`]).

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, Set, Statement, TransactionTrait,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::models::users;
use crate::services::account_security::{
    self as security, find_user_by_email, normalize_email, ActorKind, Outcome, RevokeReason,
    SecurityEvent,
};
use crate::services::email::{email_changed_notices_on, EMAIL_CHANGE_UNDO_WINDOW};
use crate::services::ops_metrics::{inc, OPS_METRICS as OM};
use crate::services::otp::{generate_code, generate_link_token, OtpCheck, OtpType};
use crate::state::AppState;

/// Minimum spacing of self-service email changes.
pub const CHANGE_COOLDOWN_DAYS: i32 = 7;

fn subject(user_id: Uuid, new_email: &str) -> String {
    format!("user:{user_id}:{new_email}")
}

fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn valid_address(email: &str) -> bool {
    use validator::ValidateEmail;
    email.len() <= 255 && email.validate_email()
}

async fn lock_address<C: ConnectionTrait>(conn: &C, email: &str) -> ApiResult<()> {
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        [format!("account-email:{email}").into()],
    ))
    .await?;
    Ok(())
}

async fn recent_change(state: &AppState, user_id: Uuid) -> ApiResult<bool> {
    Ok(state
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS recent FROM email_change_reverts \
             WHERE user_id = $1 AND reverted_at IS NULL \
               AND created_at > now() - make_interval(days => $2) LIMIT 1",
            [user_id.into(), CHANGE_COOLDOWN_DAYS.into()],
        ))
        .await?
        .is_some())
}

/// Step 2a: send a code to the new address (or an attempt notice when it is taken).
pub async fn request(state: &AppState, user: &users::Model, new_email: &str) -> ApiResult<()> {
    let new_email = normalize_email(new_email);
    if !valid_address(&new_email) {
        return Err(ApiError::BadRequest("Invalid email address".into()));
    }
    if new_email == normalize_email(&user.email) {
        return Err(ApiError::BadRequest(
            "This is already your email address".into(),
        ));
    }
    if recent_change(state, user.id).await? {
        return Err(ApiError::TooManyRequests(
            "The email address can be changed once every 7 days".into(),
        ));
    }
    let subject = subject(user.id, &new_email);
    state
        .otp_service
        .check_send_rate(&subject, OtpType::EmailChange)
        .await?;
    if find_user_by_email(&state.db, &new_email).await?.is_some() {
        if let Err(e) = state
            .email_service
            .send_email_in_use_notice(&new_email)
            .await
        {
            tracing::warn!("email-in-use notice not queued: {e}");
        }
    } else {
        let code = generate_code();
        state
            .otp_service
            .store_code(&subject, &code, OtpType::EmailChange)
            .await?;
        state
            .email_service
            .send_email_change_code(&new_email, &user.username, &code)
            .await?;
    }
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("EMAIL_CHANGE_REQUESTED", Outcome::Success).by_user(user.id),
    )
    .await;
    inc(&OM.auth_email_change_requested);
    Ok(())
}

/// Step 2b: the code from the new mailbox commits the change.
pub async fn confirm(
    state: &AppState,
    user: &users::Model,
    current_session: Uuid,
    new_email: &str,
    code: &str,
) -> ApiResult<users::Model> {
    let new_email = normalize_email(new_email);
    match state
        .otp_service
        .check(&subject(user.id, &new_email), code, OtpType::EmailChange)
        .await?
    {
        OtpCheck::Valid => {}
        OtpCheck::Missing | OtpCheck::Wrong => {
            return Err(ApiError::BadRequest("Invalid or expired code".into()))
        }
    }
    let old_email = user.email.clone();
    let change_id = Uuid::new_v4();
    let undo_token = generate_link_token();
    let undo_url = format!(
        "{}/auth/email-change/undo?token={undo_token}",
        state.email_service.app_url().trim_end_matches('/')
    );

    let txn = state.db.begin().await?;
    lock_address(&txn, &new_email).await?;
    if find_user_by_email(&txn, &new_email).await?.is_some() {
        return Err(ApiError::Conflict(
            "This address is no longer available".into(),
        ));
    }
    let mut active: users::ActiveModel = user.clone().into();
    active.email = Set(new_email.clone());
    active.email_verified_at = Set(Some(Utc::now()));
    active.updated_at = Set(crate::utils::now_utc());
    let updated = active.update(&txn).await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO email_change_reverts (id, user_id, old_email, new_email, token_digest, expires_at) \
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(days => $6))",
        [
            change_id.into(),
            user.id.into(),
            old_email.clone().into(),
            new_email.clone().into(),
            digest(&undo_token).into(),
            (EMAIL_CHANGE_UNDO_WINDOW.num_days() as i32).into(),
        ],
    ))
    .await?;
    crate::services::email::tolerate_unsendable(
        email_changed_notices_on(
            &txn,
            &old_email,
            &new_email,
            &user.username,
            change_id,
            Some(&undo_url),
        )
        .await
        .map(|_| Uuid::nil()),
        user.id,
        "email-changed",
    )?;
    let ended = security::revoke_others(
        &txn,
        user.id,
        current_session,
        RevokeReason::CredentialsChanged,
    )
    .await?;
    txn.commit().await?;

    for session_id in &ended {
        if let Err(e) =
            crate::services::communications::session_ended(state, user.id, *session_id).await
        {
            tracing::error!(%session_id, "ending a session after an email change: {e}");
        }
    }
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("EMAIL_CHANGED", Outcome::Success)
            .by_user(user.id)
            .session(current_session)
            .detail(serde_json::json!({ "sessionsEnded": ended.len(), "changeId": change_id })),
    )
    .await;
    inc(&OM.auth_email_changed);
    Ok(updated)
}

/// Step 3: the previous address undoes the change (single use, within 7 days): the old
/// address is restored, its proof kept, and every session and device of the account ends.
pub async fn revert(state: &AppState, token: &str) -> ApiResult<()> {
    let invalid = || ApiError::BadRequest("Invalid or expired link".to_string());
    let txn = state.db.begin().await?;
    let row = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id, user_id, old_email, new_email FROM email_change_reverts \
             WHERE token_digest = $1 AND reverted_at IS NULL AND expires_at > now() FOR UPDATE",
            [digest(token).into()],
        ))
        .await?
        .ok_or_else(invalid)?;
    let change_id: Uuid = row.try_get("", "id")?;
    let user_id: Uuid = row.try_get("", "user_id")?;
    let old_email: String = row.try_get("", "old_email")?;
    lock_address(&txn, &old_email).await?;
    if find_user_by_email(&txn, &old_email)
        .await?
        .is_some_and(|u| u.id != user_id)
    {
        return Err(ApiError::Conflict(
            "The previous address now belongs to another account; contact support".into(),
        ));
    }
    let user = users::Entity::find_by_id(user_id)
        .one(&txn)
        .await?
        .ok_or_else(invalid)?;
    let mut active: users::ActiveModel = user.into();
    active.email = Set(old_email);
    active.updated_at = Set(crate::utils::now_utc());
    active.update(&txn).await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE email_change_reverts SET reverted_at = now() WHERE id = $1",
        [change_id.into()],
    ))
    .await?;
    // A hijacker who changed the address loses every session and linked device.
    security::revoke_all(&txn, user_id, RevokeReason::LogoutAll).await?;
    txn.commit().await?;
    end_everything(state, user_id).await;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("EMAIL_CHANGE_REVERTED", Outcome::Success)
            .user(user_id)
            .actor(ActorKind::Anonymous, None)
            .detail(serde_json::json!({ "changeId": change_id })),
    )
    .await;
    inc(&OM.auth_email_change_reverted);
    Ok(())
}

/// Support-assisted change for an owner who lost access to the old mailbox (verified by
/// support outside the product). Signs out every session and device, notifies both
/// addresses (no undo link: support already verified the owner) and is audited with the
/// administrator and the reason.
pub async fn admin_change(
    state: &AppState,
    admin_id: Uuid,
    user_id: Uuid,
    new_email: &str,
    reason: &str,
) -> ApiResult<users::Model> {
    let new_email = normalize_email(new_email);
    if !valid_address(&new_email) {
        return Err(ApiError::BadRequest("Invalid email address".into()));
    }
    let txn = state.db.begin().await?;
    lock_address(&txn, &new_email).await?;
    let user = users::Entity::find_by_id(user_id)
        .one(&txn)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    if find_user_by_email(&txn, &new_email)
        .await?
        .is_some_and(|u| u.id != user_id)
    {
        return Err(ApiError::Conflict(
            "This address belongs to another account".into(),
        ));
    }
    let old_email = user.email.clone();
    let username = user.username.clone();
    let mut active: users::ActiveModel = user.into();
    active.email = Set(new_email.clone());
    active.email_verified_at = Set(None);
    active.updated_at = Set(crate::utils::now_utc());
    let updated = active.update(&txn).await?;
    let change_id = Uuid::new_v4();
    crate::services::email::tolerate_unsendable(
        email_changed_notices_on(&txn, &old_email, &new_email, &username, change_id, None)
            .await
            .map(|_| Uuid::nil()),
        user_id,
        "email-changed-by-support",
    )?;
    security::revoke_all(&txn, user_id, RevokeReason::LogoutAll).await?;
    txn.commit().await?;
    end_everything(state, user_id).await;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("EMAIL_CHANGED_BY_ADMIN", Outcome::Success)
            .user(user_id)
            .actor(ActorKind::Admin, Some(admin_id))
            .detail(serde_json::json!({ "reason": reason, "changeId": change_id })),
    )
    .await;
    inc(&OM.auth_admin_email_changed);
    Ok(updated)
}

/// Account-wide fan-out after every session ended (as `POST /me/sessions/revoke-all`).
async fn end_everything(state: &AppState, user_id: Uuid) {
    if let Err(e) = crate::services::communications::record_logout(state, user_id).await {
        tracing::error!(%user_id, "communications logout not queued: {e}");
    }
    crate::services::communications::credentials_ended(state, user_id, "LOGOUT").await;
}
