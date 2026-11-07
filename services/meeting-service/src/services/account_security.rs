//! Account security authority (Blueprint v1.0 Section 2).
//!
//! One place decides whether a CF Meet account token may be used:
//!
//! * token type (ACC-001): access tokens authenticate requests, refresh tokens only refresh;
//! * account revocation epoch (ACC-002/003): `account_security_state.credentials_not_before`,
//!   advanced by a database trigger on password and status changes and by logout-all. It is
//!   always enforced; the Communications epoch is enforced in addition when configured;
//! * account sessions (ACC-007/017): tokens carry a session id; a revoked or expired
//!   session ends every token issued for it, independent of Redis (SEC-007);
//! * refresh rotation with reuse detection and a 30-day absolute lifetime;
//! * an append-only security audit trail (SEC-013).
//!
//! Every verifier — v1 middleware, v2 actor resolution, the refresh endpoint and the
//! Developer Platform session bridge — calls [`authorize_user_token`].
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use sea_orm::{
    sea_query::{Expr, Func},
    ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter, Statement,
    TransactionTrait,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::users,
    services::auth::{Claims, TokenKind},
    state::AppState,
};

/// Absolute lifetime of an account session, whatever the refresh activity.
pub const SESSION_ABSOLUTE_LIFETIME_DAYS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Password,
    Google,
    LegacyRefresh,
}

impl AuthMethod {
    fn as_str(self) -> &'static str {
        match self {
            AuthMethod::Password => "PASSWORD",
            AuthMethod::Google => "GOOGLE",
            AuthMethod::LegacyRefresh => "LEGACY_REFRESH",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeReason {
    Logout,
    LogoutAll,
    UserRevoked,
    RefreshReuse,
    AdminRevoked,
}

impl RevokeReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RevokeReason::Logout => "LOGOUT",
            RevokeReason::LogoutAll => "LOGOUT_ALL",
            RevokeReason::UserRevoked => "USER_REVOKED",
            RevokeReason::RefreshReuse => "REFRESH_REUSE",
            RevokeReason::AdminRevoked => "ADMIN_REVOKED",
        }
    }
}

/// Runtime policy read once from the environment (no secret values).
#[derive(Debug, Clone)]
pub struct AuthPolicy {
    /// `exchange_code` (one-time code, ACC-005) or `legacy_query` (tokens in the redirect
    /// URL, kept only until the live web client adopts the exchange endpoint).
    pub google_token_delivery: GoogleTokenDelivery,
    /// Require the browser-binding cookie set by the OAuth start redirect.
    pub google_browser_binding: bool,
    /// Header set by the trusted reverse proxy with the client address (for example
    /// `x-real-ip`). Unset: per-address limits are off (the proxy address would be shared).
    pub client_ip_header: Option<axum::http::HeaderName>,
    /// ACC-022: the `status` string of the v1 sign-in responses (register, login, Google
    /// sign-in/exchange, `GET/PATCH /api/v1/users/me`). `ACCOUNT_STATUS_FORMAT=canonical`
    /// returns the contract value (`pending_verification`); the default `legacy` keeps the
    /// historical `pendingverification` spelling until every deployed client accepts both.
    /// Admin APIs and `/api/v2/me/profile` always use the canonical value.
    pub status_format: AccountStatusFormat,
}

/// Wire format of the account status on the v1 sign-in responses (ACC-022).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatusFormat {
    /// `pending_verification`, `active`, `inactive`, `suspended` (the contract).
    Canonical,
    /// As canonical, except `pendingverification` (the historical v1 spelling).
    Legacy,
}

impl AccountStatusFormat {
    pub fn render(self, status: &crate::models::users::UserStatus) -> &'static str {
        match (self, status) {
            (
                AccountStatusFormat::Legacy,
                crate::models::users::UserStatus::PendingVerification,
            ) => "pendingverification",
            (_, s) => s.as_contract_str(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleTokenDelivery {
    ExchangeCode,
    LegacyQuery,
}

impl AuthPolicy {
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let google_token_delivery = match get("GOOGLE_OAUTH_TOKEN_DELIVERY")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "exchange_code" => GoogleTokenDelivery::ExchangeCode,
            _ => GoogleTokenDelivery::LegacyQuery,
        };
        let google_browser_binding = !matches!(
            get("GOOGLE_OAUTH_BROWSER_BINDING")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "false" | "0" | "off"
        );
        let client_ip_header = get("AUTH_CLIENT_IP_HEADER")
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .and_then(|h| axum::http::HeaderName::from_bytes(h.as_bytes()).ok());
        let status_format = match get("ACCOUNT_STATUS_FORMAT")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "canonical" => AccountStatusFormat::Canonical,
            _ => AccountStatusFormat::Legacy,
        };
        Self {
            google_token_delivery,
            google_browser_binding,
            client_ip_header,
            status_format,
        }
    }

    /// The client address from the trusted proxy header (first entry), if configured.
    pub fn client_ip(&self, headers: &axum::http::HeaderMap) -> Option<String> {
        let name = self.client_ip_header.as_ref()?;
        let raw = headers.get(name)?.to_str().ok()?;
        let first = raw.split(',').next()?.trim();
        (!first.is_empty() && first.len() <= 64).then(|| first.to_string())
    }
}

impl Default for AuthPolicy {
    fn default() -> Self {
        Self::from_lookup(|_| None)
    }
}

// ---------------------------------------------------------------------------
// Email identity (DATA-002 / ACC-023)
// ---------------------------------------------------------------------------

pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// Case-insensitive account lookup. If legacy rows differ only by case, the exact
/// spelling wins; an ambiguous match is treated as "no account" and logged.
pub async fn find_user_by_email(
    db: &DatabaseConnection,
    email: &str,
) -> ApiResult<Option<users::Model>> {
    let normalized = normalize_email(email);
    if normalized.is_empty() {
        return Ok(None);
    }
    let mut rows = users::Entity::find()
        .filter(Expr::expr(Func::lower(Expr::col(users::Column::Email))).eq(normalized))
        .all(db)
        .await?;
    match rows.len() {
        0 => Ok(None),
        1 => Ok(rows.pop()),
        n => {
            let exact = email.trim();
            if let Some(i) = rows.iter().position(|u| u.email == exact) {
                Ok(Some(rows.swap_remove(i)))
            } else {
                tracing::warn!(
                    matches = n,
                    "account lookup is ambiguous: several accounts differ only by email case"
                );
                Ok(None)
            }
        }
    }
}

/// A bcrypt hash used to keep "unknown account" and "wrong password" equally slow.
pub fn timing_equalizer_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        bcrypt::hash("cfmeet-timing-equalizer", bcrypt::DEFAULT_COST)
            .unwrap_or_else(|_| String::new())
    })
}

// ---------------------------------------------------------------------------
// Token authorization
// ---------------------------------------------------------------------------

fn revoked() -> ApiError {
    ApiError::Unauthorized("Credentials have been revoked".into())
}

/// The latest instant before which this account's credentials are void: the account
/// epoch, and the Communications epoch when that integration is configured.
pub async fn credentials_cutoff(
    state: &AppState,
    user_id: Uuid,
) -> ApiResult<Option<DateTime<Utc>>> {
    let own: Option<DateTime<Utc>> = state
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT credentials_not_before FROM account_security_state WHERE user_id = $1",
            [user_id.into()],
        ))
        .await?
        .map(|r| r.try_get::<Option<DateTime<Utc>>>("", "credentials_not_before"))
        .transpose()?
        .flatten();
    let communications = if state.communications.is_some() {
        state
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT credentials_not_before FROM account_communication_state WHERE user_id = $1",
                [user_id.into()],
            ))
            .await?
            .map(|r| r.try_get::<Option<DateTime<Utc>>>("", "credentials_not_before"))
            .transpose()?
            .flatten()
    } else {
        None
    };
    Ok(own.max(communications))
}

/// What the check established, for callers that track revocation evidence.
#[derive(Debug, Clone, Copy)]
pub struct TokenAuthorization {
    /// The token belongs to a live account session (database-checked, no Redis needed).
    pub session_verified: bool,
    pub session_id: Option<Uuid>,
}

/// Decide whether a registered user's token may be used for `expected`.
///
/// Session tokens: the session must exist for this user, be open and inside its
/// absolute lifetime, and the token must not predate the account epoch.
/// Legacy tokens (issued before sessions existed): only the epoch applies, with the
/// original inclusive comparison.
pub async fn authorize_user_token(
    state: &AppState,
    claims: &Claims,
    user_id: Uuid,
    expected: TokenKind,
) -> ApiResult<TokenAuthorization> {
    if !claims.is_kind(expected) {
        return Err(ApiError::Unauthorized("Invalid token type".into()));
    }
    let cutoff = credentials_cutoff(state, user_id).await?;
    match claims.session_id() {
        Some(session_id) => {
            // Session tokens take their auth_time from the database clock, so a strict
            // comparison lets a sign-in in the same second as an epoch advance survive,
            // while the session row itself covers tokens revoked in that second.
            if cutoff.is_some_and(|t| claims.auth_time.is_none_or(|a| a < t.timestamp())) {
                return Err(revoked());
            }
            let row = state
                .db
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT user_id, revoked_at IS NULL AND absolute_expires_at > now() AS open \
                     FROM account_sessions WHERE id = $1",
                    [session_id.into()],
                ))
                .await?
                .ok_or_else(revoked)?;
            let owner: Uuid = row.try_get("", "user_id")?;
            let open: bool = row.try_get("", "open")?;
            if owner != user_id || !open {
                return Err(revoked());
            }
            Ok(TokenAuthorization {
                session_verified: true,
                session_id: Some(session_id),
            })
        }
        None if claims.sid.is_some() => Err(revoked()),
        None => {
            if cutoff.is_some_and(|t| claims.auth_time.is_none_or(|a| a <= t.timestamp())) {
                return Err(revoked());
            }
            Ok(TokenAuthorization {
                session_verified: false,
                session_id: None,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

pub struct IssuedSession {
    pub session_id: Uuid,
    pub access_token: String,
    pub refresh_token: String,
}

pub fn role_claim(user: &users::Model) -> String {
    format!("{:?}", user.role).to_lowercase()
}

/// A short, printable client label from the User-Agent (never a fingerprint).
pub fn client_label(headers: &axum::http::HeaderMap) -> Option<String> {
    let ua = headers.get(axum::http::header::USER_AGENT)?.to_str().ok()?;
    let label: String = ua.chars().filter(|c| !c.is_control()).take(120).collect();
    (!label.trim().is_empty()).then_some(label)
}

/// Open a session and issue its token pair. `authenticated_at` defaults to the database
/// clock, so the token never predates an epoch advanced in the same moment.
pub async fn start_session(
    state: &AppState,
    user: &users::Model,
    method: AuthMethod,
    authenticated_at: Option<i64>,
    label: Option<String>,
) -> ApiResult<IssuedSession> {
    let session_id = Uuid::new_v4();
    let refresh_id = Uuid::new_v4();
    let row = state
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO account_sessions \
               (id, user_id, auth_method, authenticated_at, absolute_expires_at, current_refresh_id, client_label) \
             VALUES ($1, $2, $3, COALESCE(to_timestamp($4::bigint), now()), \
                     COALESCE(to_timestamp($4::bigint), now()) + make_interval(days => $5), $6, $7) \
             RETURNING floor(extract(epoch FROM authenticated_at))::bigint AS auth_time",
            [
                session_id.into(),
                user.id.into(),
                method.as_str().into(),
                authenticated_at.into(),
                (SESSION_ABSOLUTE_LIFETIME_DAYS as i32).into(),
                refresh_id.into(),
                label.into(),
            ],
        ))
        .await?
        .ok_or_else(|| ApiError::InternalError("session was not created".into()))?;
    let auth_time: i64 = row.try_get("", "auth_time")?;
    let (access_token, refresh_token) = state.jwt_service.session_tokens(
        user.id,
        &role_claim(user),
        auth_time,
        session_id,
        refresh_id,
    )?;
    Ok(IssuedSession {
        session_id,
        access_token,
        refresh_token,
    })
}

/// Rotate a session's refresh token. Presenting a superseded refresh token revokes the
/// whole session (reuse detection) and is audited.
pub async fn rotate_session(
    state: &AppState,
    claims: &Claims,
    user: &users::Model,
) -> ApiResult<(String, String)> {
    let session_id = claims.session_id().ok_or_else(revoked)?;
    let presented = claims.refresh_id();
    let txn = state.db.begin().await?;
    let row = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT user_id, current_refresh_id, \
                    floor(extract(epoch FROM authenticated_at))::bigint AS auth_time, \
                    revoked_at IS NULL AND absolute_expires_at > now() AS open \
             FROM account_sessions WHERE id = $1 FOR UPDATE",
            [session_id.into()],
        ))
        .await?
        .ok_or_else(revoked)?;
    let owner: Uuid = row.try_get("", "user_id")?;
    let current: Uuid = row.try_get("", "current_refresh_id")?;
    let session_auth_time: i64 = row.try_get("", "auth_time")?;
    let open: bool = row.try_get("", "open")?;
    if owner != user.id || !open {
        return Err(revoked());
    }
    if presented != Some(current) {
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_sessions SET revoked_at = now(), revoke_reason = 'REFRESH_REUSE' \
             WHERE id = $1 AND revoked_at IS NULL",
            [session_id.into()],
        ))
        .await?;
        record_event(
            &txn,
            SecurityEvent::new("REFRESH_TOKEN_REUSE", Outcome::Denied)
                .user(user.id)
                .session(session_id)
                .actor(ActorKind::Anonymous, None),
        )
        .await?;
        txn.commit().await?;
        tracing::warn!(%session_id, "refresh token reuse detected; session revoked");
        return Err(ApiError::Unauthorized(
            "This session has ended. Please sign in again.".into(),
        ));
    }
    let next = Uuid::new_v4();
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE account_sessions SET current_refresh_id = $2, \
                refresh_generation = refresh_generation + 1, last_refreshed_at = now() \
         WHERE id = $1",
        [session_id.into(), next.into()],
    ))
    .await?;
    txn.commit().await?;
    state.jwt_service.session_tokens(
        user.id,
        &role_claim(user),
        claims.auth_time.unwrap_or(session_auth_time),
        session_id,
        next,
    )
}

/// Revoke one of the user's sessions. `false` if it was not open or not theirs.
pub async fn revoke_session<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    session_id: Uuid,
    reason: RevokeReason,
) -> ApiResult<bool> {
    let result = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_sessions SET revoked_at = now(), revoke_reason = $3 \
             WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
            [session_id.into(), user_id.into(), reason.as_str().into()],
        ))
        .await?;
    Ok(result.rows_affected() == 1)
}

/// End every credential of the account: advance the epoch (catches legacy tokens) and
/// close every open session. Returns the number of sessions closed.
pub async fn revoke_all<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    reason: RevokeReason,
) -> ApiResult<u64> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO account_security_state (user_id, credentials_not_before, updated_at) \
         VALUES ($1, now(), now()) \
         ON CONFLICT (user_id) DO UPDATE SET credentials_not_before = now(), updated_at = now()",
        [user_id.into()],
    ))
    .await?;
    let result = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_sessions SET revoked_at = now(), revoke_reason = $2 \
             WHERE user_id = $1 AND revoked_at IS NULL",
            [user_id.into(), reason.as_str().into()],
        ))
        .await?;
    Ok(result.rows_affected())
}

/// One account session as shown to its owner.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub session_id: Uuid,
    /// PASSWORD, GOOGLE or LEGACY_REFRESH (a pre-session sign-in carried forward).
    pub auth_method: String,
    pub authenticated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub last_refreshed_at: Option<DateTime<Utc>>,
    /// The session ends here at the latest, whatever the refresh activity.
    pub expires_at: DateTime<Utc>,
    /// Shortened User-Agent of the client that signed in.
    pub client_label: Option<String>,
    /// ACTIVE, REVOKED or EXPIRED.
    pub status: String,
    pub ended_at: Option<DateTime<Utc>>,
    pub end_reason: Option<String>,
    /// True for the session the request was made with.
    pub current: bool,
}

/// The user's sessions, newest first (open ones, plus ended ones on request).
pub async fn list_sessions(
    db: &DatabaseConnection,
    user_id: Uuid,
    include_ended: bool,
    current: Option<Uuid>,
) -> ApiResult<Vec<SessionRecord>> {
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id, auth_method, authenticated_at, created_at, last_refreshed_at,                     absolute_expires_at, client_label, revoked_at, revoke_reason,                     revoked_at IS NULL AND absolute_expires_at > now() AS open              FROM account_sessions              WHERE user_id = $1 AND ($2 OR (revoked_at IS NULL AND absolute_expires_at > now()))              ORDER BY created_at DESC LIMIT 100",
            [user_id.into(), include_ended.into()],
        ))
        .await?;
    rows.into_iter()
        .map(|r| {
            let id: Uuid = r.try_get("", "id")?;
            let revoked_at: Option<DateTime<Utc>> = r.try_get("", "revoked_at")?;
            let open: bool = r.try_get("", "open")?;
            let expires_at: DateTime<Utc> = r.try_get("", "absolute_expires_at")?;
            Ok(SessionRecord {
                session_id: id,
                auth_method: r.try_get("", "auth_method")?,
                authenticated_at: r.try_get("", "authenticated_at")?,
                created_at: r.try_get("", "created_at")?,
                last_refreshed_at: r.try_get("", "last_refreshed_at")?,
                expires_at,
                client_label: r.try_get("", "client_label")?,
                status: if open {
                    "ACTIVE"
                } else if revoked_at.is_some() {
                    "REVOKED"
                } else {
                    "EXPIRED"
                }
                .to_string(),
                ended_at: revoked_at.or((!open).then_some(expires_at)),
                end_reason: r.try_get("", "revoke_reason")?,
                current: current == Some(id),
            })
        })
        .collect::<Result<Vec<_>, sea_orm::DbErr>>()
        .map_err(ApiError::from)
}

// ---------------------------------------------------------------------------
// Google identity binding (ACC-004)
// ---------------------------------------------------------------------------

pub async fn user_for_google_subject<C: ConnectionTrait>(
    db: &C,
    subject: &str,
) -> ApiResult<Option<Uuid>> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT user_id FROM account_security_state WHERE google_subject = $1",
            [subject.into()],
        ))
        .await?
        .map(|r| r.try_get::<Uuid>("", "user_id"))
        .transpose()?)
}

pub async fn google_subject_for_user<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
) -> ApiResult<Option<String>> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT google_subject FROM account_security_state WHERE user_id = $1",
            [user_id.into()],
        ))
        .await?
        .map(|r| r.try_get::<Option<String>>("", "google_subject"))
        .transpose()?
        .flatten())
}

/// Bind a verified Google subject to an account that has none. Refuses to replace a
/// different subject; a subject already bound elsewhere is a conflict.
pub async fn bind_google_subject<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    subject: &str,
) -> ApiResult<()> {
    let result = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO account_security_state (user_id, google_subject, google_linked_at, updated_at) \
             VALUES ($1, $2, now(), now()) \
             ON CONFLICT (user_id) DO UPDATE \
               SET google_subject = EXCLUDED.google_subject, google_linked_at = now(), updated_at = now() \
             WHERE account_security_state.google_subject IS NULL \
                OR account_security_state.google_subject = EXCLUDED.google_subject",
            [user_id.into(), subject.into()],
        ))
        .await;
    match result {
        Ok(r) if r.rows_affected() == 1 => Ok(()),
        Ok(_) => Err(ApiError::Conflict(
            "This account is linked to a different Google identity".into(),
        )),
        Err(e) if e.to_string().contains("uq_account_security_google_subject") => Err(
            ApiError::Conflict("This Google identity is linked to another account".into()),
        ),
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// Security audit (SEC-013 / DATA-015)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum Outcome {
    Success,
    Failure,
    Denied,
}

#[derive(Debug, Clone, Copy)]
pub enum ActorKind {
    User,
    Admin,
    System,
    Anonymous,
}

pub struct SecurityEvent {
    event_type: &'static str,
    outcome: Outcome,
    user_id: Option<Uuid>,
    session_id: Option<Uuid>,
    actor_kind: ActorKind,
    actor_user_id: Option<Uuid>,
    detail: Value,
}

impl SecurityEvent {
    pub fn new(event_type: &'static str, outcome: Outcome) -> Self {
        Self {
            event_type,
            outcome,
            user_id: None,
            session_id: None,
            actor_kind: ActorKind::System,
            actor_user_id: None,
            detail: json!({}),
        }
    }
    pub fn user(mut self, id: Uuid) -> Self {
        self.user_id = Some(id);
        self
    }
    pub fn session(mut self, id: Uuid) -> Self {
        self.session_id = Some(id);
        self
    }
    pub fn actor(mut self, kind: ActorKind, user: Option<Uuid>) -> Self {
        self.actor_kind = kind;
        self.actor_user_id = user;
        self
    }
    /// Self-acting user (the common case).
    pub fn by_user(self, id: Uuid) -> Self {
        self.user(id).actor(ActorKind::User, Some(id))
    }
    /// Small, non-sensitive detail (never emails, tokens, passwords or addresses).
    pub fn detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }
}

pub async fn record_event<C: ConnectionTrait>(db: &C, event: SecurityEvent) -> ApiResult<()> {
    let outcome = match event.outcome {
        Outcome::Success => "SUCCESS",
        Outcome::Failure => "FAILURE",
        Outcome::Denied => "DENIED",
    };
    let actor = match event.actor_kind {
        ActorKind::User => "USER",
        ActorKind::Admin => "ADMIN",
        ActorKind::System => "SYSTEM",
        ActorKind::Anonymous => "ANONYMOUS",
    };
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO account_security_events \
           (event_id, user_id, session_id, event_type, outcome, actor_kind, actor_user_id, request_id, detail) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        [
            Uuid::new_v4().into(),
            event.user_id.into(),
            event.session_id.into(),
            event.event_type.into(),
            outcome.into(),
            actor.into(),
            event.actor_user_id.into(),
            crate::middleware::request_id::current_request_id()
                .map(|r| r.chars().take(100).collect::<String>())
                .into(),
            event.detail.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Audit that must not block the user-facing operation (sign-in attempts): failures are
/// logged loudly instead.
pub async fn record_event_best_effort(db: &DatabaseConnection, event: SecurityEvent) {
    let kind = event.event_type;
    if let Err(e) = record_event(db, event).await {
        tracing::error!(
            event_type = kind,
            "account security audit write failed: {e}"
        );
    }
}

/// One account security audit event (administrator view; never secrets or contact data).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecurityEventRecord {
    /// Cursor value: pass it as `before` to page further back.
    pub seq: i64,
    pub event_id: Uuid,
    pub user_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub event_type: String,
    /// SUCCESS, FAILURE or DENIED.
    pub outcome: String,
    /// USER, ADMIN, SYSTEM or ANONYMOUS.
    pub actor_kind: String,
    pub actor_user_id: Option<Uuid>,
    pub request_id: Option<String>,
    #[schema(value_type = Object)]
    pub detail: Value,
    pub occurred_at: DateTime<Utc>,
}

/// Paged audit read for platform administrators (newest first, `before` = seq cursor).
pub async fn list_events(
    db: &DatabaseConnection,
    user_id: Option<Uuid>,
    event_type: Option<String>,
    before: Option<i64>,
    limit: u64,
) -> ApiResult<(Vec<SecurityEventRecord>, Option<i64>)> {
    let limit = limit.clamp(1, 200) as i64;
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT seq, event_id, user_id, session_id, event_type, outcome, actor_kind,                     actor_user_id, request_id, detail, occurred_at              FROM account_security_events              WHERE ($1::uuid IS NULL OR user_id = $1)                AND ($2::text IS NULL OR event_type = $2)                AND ($3::bigint IS NULL OR seq < $3)              ORDER BY seq DESC LIMIT $4",
            [
                user_id.into(),
                event_type.into(),
                before.into(),
                (limit + 1).into(),
            ],
        ))
        .await?;
    let more = rows.len() as i64 > limit;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows.iter().take(limit as usize) {
        out.push(SecurityEventRecord {
            seq: r.try_get("", "seq")?,
            event_id: r.try_get("", "event_id")?,
            user_id: r.try_get("", "user_id")?,
            session_id: r.try_get("", "session_id")?,
            event_type: r.try_get("", "event_type")?,
            outcome: r.try_get("", "outcome")?,
            actor_kind: r.try_get("", "actor_kind")?,
            actor_user_id: r.try_get("", "actor_user_id")?,
            request_id: r.try_get("", "request_id")?,
            detail: r.try_get("", "detail")?,
            occurred_at: r.try_get("", "occurred_at")?,
        });
    }
    let next = if more {
        out.last().map(|e| e.seq)
    } else {
        None
    };
    Ok((out, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_status_format_is_legacy_until_switched() {
        use crate::models::users::UserStatus;
        let legacy = AuthPolicy::from_lookup(|_| None).status_format;
        assert_eq!(legacy, AccountStatusFormat::Legacy);
        assert_eq!(
            legacy.render(&UserStatus::PendingVerification),
            "pendingverification"
        );
        assert_eq!(legacy.render(&UserStatus::Active), "active");
        let canonical = AuthPolicy::from_lookup(|k| {
            (k == "ACCOUNT_STATUS_FORMAT").then(|| " Canonical ".to_string())
        })
        .status_format;
        assert_eq!(canonical, AccountStatusFormat::Canonical);
        for s in [
            UserStatus::PendingVerification,
            UserStatus::Active,
            UserStatus::Inactive,
            UserStatus::Suspended,
        ] {
            assert_eq!(canonical.render(&s), s.as_contract_str());
        }
        assert_eq!(
            canonical.render(&UserStatus::PendingVerification),
            "pending_verification"
        );
    }

    #[test]
    fn policy_defaults_keep_live_client_compatibility_and_bind_browsers() {
        let p = AuthPolicy::from_lookup(|_| None);
        assert_eq!(p.google_token_delivery, GoogleTokenDelivery::LegacyQuery);
        assert!(p.google_browser_binding);
        assert!(p.client_ip_header.is_none());
        let p = AuthPolicy::from_lookup(|k| match k {
            "GOOGLE_OAUTH_TOKEN_DELIVERY" => Some("EXCHANGE_CODE".into()),
            "GOOGLE_OAUTH_BROWSER_BINDING" => Some("false".into()),
            "AUTH_CLIENT_IP_HEADER" => Some("X-Real-IP".into()),
            _ => None,
        });
        assert_eq!(p.google_token_delivery, GoogleTokenDelivery::ExchangeCode);
        assert!(!p.google_browser_binding);
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-real-ip", "203.0.113.7, 10.0.0.1".parse().unwrap());
        assert_eq!(p.client_ip(&h).as_deref(), Some("203.0.113.7"));
    }

    #[test]
    fn email_normalization_is_trim_and_lowercase() {
        assert_eq!(normalize_email("  Alice@Example.COM "), "alice@example.com");
    }
}
