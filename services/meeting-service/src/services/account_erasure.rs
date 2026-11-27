//! Gap audit A2: personal-data export and account deletion (GDPR / NDPA).
//!
//! * **Export** ([`export`]): one JSON document of everything CF Meet holds about the
//!   signed-in account: from the Meeting Service its profile, sign-ins, linked devices,
//!   meeting spaces it owns, memberships, webinar registrations, conference participation
//!   and its own security events; and (S2) from Messaging (its conversations), the
//!   Communications Hub (profile, CF Meet ID, contacts, lists, saved messages, settings)
//!   and Calling (call history), read live through
//!   [`account_privacy::export_elsewhere`](crate::services::account_privacy::export_elsewhere).
//! * **Deletion** ([`erase`]): confirmed with a `DELETE_ACCOUNT` step-up. In one
//!   transaction the account row is anonymized (address, username, name, avatar, time zone,
//!   locale and password removed; status inactive, so it can never sign in again), the
//!   Google link is removed, meeting memberships are revoked, owned meeting spaces are
//!   archived and webinar registrations cancelled. Every sign-in and device ends, and the
//!   account-wide fan-out ends Hub, Messaging and Calling credentials. In the same
//!   transaction (S2) one durable erasure job per owner is queued
//!   ([`account_privacy`](crate::services::account_privacy)): Messaging erases the
//!   person's messages (PRINCIPAL_ERASURE), the Hub their profile, CF Meet ID, contacts,
//!   lists and saved messages, Calling their call history; delivered and retried until each
//!   owner confirms. The append-only
//!   security audit keeps an `ACCOUNT_DELETED` event without personal data. The old
//!   address gets one confirmation email (queued before it is removed).
use serde_json::{json, Value};
use uuid::Uuid;

use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

use crate::{
    error::{ApiError, ApiResult},
    services::account_security::{
        self as security, ActorKind, Outcome, RevokeReason, SecurityEvent,
    },
    services::admin_operations::{self, BridgeCommand},
    state::AppState,
};

/// Whether the account was already deleted (anonymized by [`erase`]).
async fn is_erased<C: ConnectionTrait>(db: &C, user_id: Uuid) -> ApiResult<bool> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS x FROM users WHERE id = $1 AND email LIKE 'deleted+%@deleted.invalid'",
            [user_id.into()],
        ))
        .await?
        .is_some())
}

async fn rows<C: ConnectionTrait>(db: &C, sql: &str, user: Uuid) -> ApiResult<Value> {
    let found = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT COALESCE(jsonb_agg(x), '[]'::jsonb) AS doc FROM ({sql}) x"),
            [user.into()],
        ))
        .await?;
    Ok(found
        .first()
        .map(|r| r.try_get::<Value>("", "doc"))
        .transpose()?
        .unwrap_or_else(|| json!([])))
}

/// The account's personal data held by the Meeting Service.
pub async fn export(state: &AppState, user_id: Uuid) -> ApiResult<Value> {
    let db = &state.db;
    let account = rows(db,
        "SELECT id, username, email, display_name, avatar_url, timezone, locale, status::text AS status, \
                role::text AS role, auth_type::text AS auth_type, created_at, last_login, email_verified_at \
           FROM users WHERE id = $1", user_id).await?;
    let account = account
        .get(0)
        .cloned()
        .ok_or_else(|| ApiError::NotFound("Account not found".into()))?;
    Ok(json!({
        "schemaVersion": 1,
        "generatedAt": chrono::Utc::now(),
        "account": account,
        "signIns": rows(db, "SELECT id, auth_method, authenticated_at, created_at, last_refreshed_at, absolute_expires_at, \
                                    client_label, revoked_at, revoke_reason FROM account_sessions WHERE user_id = $1 \
                              ORDER BY created_at DESC LIMIT 1000", user_id).await?,
        "devices": rows(db, "SELECT id, device_class, platform, label, trust, state, created_at, last_seen_at \
                               FROM account_devices WHERE user_id = $1 ORDER BY created_at", user_id).await?,
        "ownedMeetingSpaces": rows(db, "SELECT id, title, status, created_at FROM meeting_spaces \
                                         WHERE owner_user_id = $1 ORDER BY created_at", user_id).await?,
        "meetingMemberships": rows(db, "SELECT meeting_space_id, role, status, created_at FROM meeting_members \
                                         WHERE user_id = $1 ORDER BY created_at", user_id).await?,
        "webinarRegistrations": rows(db, "SELECT meeting_space_id, conference_id, status, created_at \
                                           FROM webinar_registrations WHERE user_id = $1 ORDER BY created_at", user_id).await?,
        "conferenceParticipation": rows(db, "SELECT conference_id, role, admission_state, requested_at, admitted_at FROM conference_participants \
                                              WHERE user_id = $1 ORDER BY requested_at DESC LIMIT 1000", user_id).await?,
        "securityEvents": rows(db, "SELECT event_type, outcome, occurred_at FROM account_security_events \
                                     WHERE user_id = $1 ORDER BY seq DESC LIMIT 1000", user_id).await?,
        // S2: the other services' data about the account, in the same document.
        "elsewhere": crate::services::account_privacy::export_elsewhere(db, user_id).await,
        "phone": rows(db, "SELECT phone_e164, verified_at, discoverable FROM account_phone_bindings WHERE user_id = $1", user_id).await?,
    }))
}

/// Deletes (anonymizes) the account; see the module documentation.
pub async fn erase(state: &AppState, user_id: Uuid, session: Option<Uuid>) -> ApiResult<()> {
    erase_with(state, user_id, session, None).await
}

/// [`erase`], or (ADR 0068, `accounts.user.delete`) the same deletion relayed by the HQ
/// admin bridge: the key is claimed and the account locked at HQ's expected
/// `account_version` in the deletion's transaction, the command is recorded there
/// (`meeting_admin_operations`, state `DELETED`), and the confirmation to the address the
/// person knew is queued only once the deletion committed (a refused command sends
/// nothing).
pub async fn erase_with(
    state: &AppState,
    user_id: Uuid,
    session: Option<Uuid>,
    bridge: Option<&BridgeCommand>,
) -> ApiResult<()> {
    use sea_orm::EntityTrait;
    let account = crate::models::users::Entity::find_by_id(user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Account not found".into()))?;
    if bridge.is_none() {
        // The confirmation goes to the address the person knew, before it is removed.
        security::notify_security(
            state,
            user_id,
            crate::services::email::SecurityNotice::Deleted,
            &user_id.to_string(),
        )
        .await;
    }
    let short = user_id.simple().to_string();
    let txn = state.db.begin().await?;
    if let Some(cmd) = bridge {
        admin_operations::claim(&txn, cmd).await?;
        let version = admin_operations::account_version(&txn, user_id, true)
            .await?
            .ok_or_else(|| ApiError::NotFound("Account not found".into()))?;
        admin_operations::check_version(cmd, version)?;
        if is_erased(&txn, user_id).await? {
            return Err(ApiError::Conflict("This account is already deleted".into()));
        }
    }
    for (sql, values) in [
        (
            "UPDATE users SET email = $2, username = $3, display_name = NULL, avatar_url = NULL, timezone = NULL, \
                    locale = NULL, password_hash = NULL, status = 'inactive'::user_status, updated_at = now() \
              WHERE id = $1",
            vec![
                user_id.into(),
                format!("deleted+{short}@deleted.invalid").into(),
                format!("deleted_{}", &short[..16]).into(),
            ],
        ),
        (
            "UPDATE account_security_state SET google_subject = NULL, updated_at = now() WHERE user_id = $1",
            vec![user_id.into()],
        ),
        (
            "DELETE FROM account_avatars WHERE user_id = $1",
            vec![user_id.into()],
        ),
        ("DELETE FROM account_phone_challenges WHERE user_id = $1", vec![user_id.into()]),
        ("DELETE FROM account_phone_bindings WHERE user_id = $1", vec![user_id.into()]),
        (
            "UPDATE meeting_members SET status = 'REVOKED', revoked_at = now() WHERE user_id = $1 AND status = 'ACTIVE'",
            vec![user_id.into()],
        ),
        (
            "UPDATE meeting_spaces SET status = 'ARCHIVED', updated_at = now() WHERE owner_user_id = $1 AND status = 'ACTIVE'",
            vec![user_id.into()],
        ),
        (
            "UPDATE webinar_registrations SET status = 'CANCELLED', cancelled_at = now(), cancelled_by = 'self:account-deleted' \
              WHERE user_id = $1 AND status = 'REGISTERED'",
            vec![user_id.into()],
        ),
    ] {
        txn.execute(Statement::from_sql_and_values(DbBackend::Postgres, sql, values))
            .await?;
    }
    security::revoke_all(&txn, user_id, RevokeReason::LogoutAll).await?;
    // S2: the data the other services hold goes too (durable, retried until confirmed).
    crate::services::account_privacy::queue_erasure(&txn, user_id).await?;
    match bridge {
        None => {
            security::record_event(
                &txn,
                SecurityEvent::new("ACCOUNT_DELETED", Outcome::Success)
                    .by_user(user_id)
                    .detail(json!({ "session": session.is_some() })),
            )
            .await?
        }
        Some(cmd) => {
            security::record_event(
                &txn,
                SecurityEvent::new("ACCOUNT_DELETED", Outcome::Success)
                    .user(user_id)
                    .actor(ActorKind::Admin, None)
                    .detail(json!({ "operator": cmd.operator(), "approver": cmd.approver() })),
            )
            .await?;
            let version = admin_operations::account_version(&txn, user_id, false).await?;
            admin_operations::record(
                &txn,
                cmd,
                admin_operations::subject::USER,
                user_id,
                "DELETED",
                version,
            )
            .await?;
        }
    }
    txn.commit().await?;
    if bridge.is_some()
        && account.role != crate::models::users::UserRole::Guest
        && !account.email.trim().is_empty()
    {
        // The confirmation to the address the person knew (captured before removal).
        if let Err(e) = state
            .email_service
            .send_security_notice(
                &account.email,
                &account.username,
                crate::services::email::SecurityNotice::Deleted,
                &user_id.to_string(),
            )
            .await
        {
            tracing::warn!(%user_id, "account deletion notice not queued: {e}");
        }
    }
    // Hub, Messaging, Calling and Platform Access follow (durable, retried).
    crate::services::communications::credentials_ended(state, user_id, "ACCOUNT_DELETED").await;
    // First delivery attempt now (the privacy worker retries whatever does not finish).
    let db = state.db.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::services::account_privacy::deliver_due(&db).await {
            tracing::warn!("account erasure delivery deferred to the privacy worker: {e}");
        }
    });
    Ok(())
}
