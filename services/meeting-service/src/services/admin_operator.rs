//! Admin bridge operator reads (ADR 0068, design §6.1–6.2 / §7; contract
//! `packages/contracts/meeting-service/admin-operator/v1.openapi.json`).
//!
//! The CosmicForge central admin (HQ) reaches CF Meet only through `cfmeet.admin-bridge`.
//! These routes answer the bridge's account and meeting reads, and nothing else:
//!
//! * `account.administer` (bridge only): user search and detail, account sessions, Account
//!   devices, MFA status and the account security history.
//! * `meeting.administer` (bridge only): meeting spaces, conferences (search, detail,
//!   live), conference participants and the conference audit.
//!
//! Every request must also name the HQ staff member it reads for (`X-CF-Operator:
//! hq:<staff>`), parsed once by [`cf_service_auth::OperatorAttribution`]; attribution from
//! any other caller is refused on EVERY internal route of the service
//! ([`attribution_refused`] is called by each internal router).
//!
//! Minimal by construction (design §12): ids, states, counts and times. Never a password
//! hash, token, session label, device key or digest, MFA secret or recovery code, IP
//! address or location, message content or media. User search matches on the email
//! address (`x-contact-search`) but never returns it; the detail read shows it masked.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `ADMIN_OPERATOR_API_ENABLED` | `true` mounts the routes (default off) |
//! | `CF_RUNTIME_ENVIRONMENT` | runtime environment of the trust bundle |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | receiver trust bundle (`cfmeet.meeting-service`) |

use std::{path::PathBuf, sync::Arc};

use axum::{
    extract::{OriginalUri, Path, Query, State},
    http::{request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use cf_service_auth::{
    InternalPermission, InternalServiceContext, OperatorAttribution, RequestId, RuntimeEnvironment,
    ServiceId, ServiceTokenVerifier, SystemClock, TrustPolicy,
};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, Value as DbValue};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{config::env_bool, state::AppState};

pub const USERS: &str = "/internal/v1/operator/accounts/users";
pub const USER: &str = "/internal/v1/operator/accounts/users/:user_id";
pub const SESSIONS: &str = "/internal/v1/operator/accounts/users/:user_id/sessions";
pub const DEVICES: &str = "/internal/v1/operator/accounts/users/:user_id/devices";
pub const MFA: &str = "/internal/v1/operator/accounts/users/:user_id/mfa";
pub const SECURITY_EVENTS: &str = "/internal/v1/operator/accounts/users/:user_id/security-events";
pub const SPACES: &str = "/internal/v1/operator/meetings/spaces";
pub const CONFERENCES: &str = "/internal/v1/operator/meetings/conferences";
pub const CONFERENCE: &str = "/internal/v1/operator/meetings/conferences/:conference_id";
pub const PARTICIPANTS: &str =
    "/internal/v1/operator/meetings/conferences/:conference_id/participants";
pub const LIVE: &str = "/internal/v1/operator/meetings/live";
pub const AUDIT: &str = "/internal/v1/operator/meetings/audit-events";

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 100;
const MAX_WINDOW_DAYS: i64 = 31;

/// `(since, until)` of a windowed read.
type Window = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

#[derive(Debug, Clone)]
pub struct AdminOperatorConfig {
    pub runtime: RuntimeEnvironment,
    pub trust_bundle_file: PathBuf,
}

impl AdminOperatorConfig {
    /// `Ok(None)`: off. `Err`: on but misconfigured (startup fails closed).
    pub fn from_env() -> Result<Option<Self>, String> {
        if !env_bool("ADMIN_OPERATOR_API_ENABLED", false) {
            return Ok(None);
        }
        let var = |name: &str| match std::env::var(name) {
            Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
            _ => Err(format!(
                "{name} is required when ADMIN_OPERATOR_API_ENABLED is on"
            )),
        };
        let runtime = RuntimeEnvironment::parse(&var("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT must be local, test, staging or production")?;
        Ok(Some(Self {
            runtime,
            trust_bundle_file: PathBuf::from(var("CF_SERVICE_TRUST_BUNDLE_FILE")?),
        }))
    }

    pub fn verifier(&self) -> Result<Arc<ServiceTokenVerifier>, String> {
        let policy = TrustPolicy::from_file(
            &self.trust_bundle_file,
            &ServiceId::meeting_service(),
            self.runtime,
        )
        .map_err(|e| format!("CF_SERVICE_TRUST_BUNDLE_FILE: {e}"))?;
        Ok(Arc::new(ServiceTokenVerifier::new(
            policy,
            Arc::new(SystemClock),
        )))
    }
}

/// True when the request carries operator attribution (`X-CF-Operator`, `X-CF-Approver`,
/// `X-CF-Approval-Ref`, `X-CF-Intent-Digest`) that must be refused: from any caller other
/// than `cfmeet.admin-bridge`, or malformed, incomplete or self-approving. Every internal
/// router of the Meeting Service calls this after verifying the service token.
pub fn attribution_refused(caller: &ServiceId, headers: &HeaderMap) -> bool {
    match OperatorAttribution::from_headers(caller, headers) {
        Ok(_) => false,
        Err(e) => {
            tracing::warn!(caller = caller.as_str(), reason = %e, "operator attribution refused");
            true
        }
    }
}

#[derive(Clone)]
struct OperatorState {
    app: AppState,
    verifier: Arc<ServiceTokenVerifier>,
}

/// The operator router (mounted only when enabled).
pub fn router(app: AppState, verifier: Arc<ServiceTokenVerifier>) -> Router {
    Router::new()
        .route(USERS, get(users))
        .route(USER, get(user))
        .route(SESSIONS, get(sessions))
        .route(DEVICES, get(devices))
        .route(MFA, get(mfa))
        .route(SECURITY_EVENTS, get(security_events))
        .route(SPACES, get(spaces))
        .route(CONFERENCES, get(conferences))
        .route(CONFERENCE, get(conference))
        .route(PARTICIPANTS, get(participants))
        .route(LIVE, get(live))
        .route(AUDIT, get(audit))
        .with_state(OperatorState { app, verifier })
}

// ---------------------------------------------------------------------------
// Errors and authentication
// ---------------------------------------------------------------------------

fn error(status: StatusCode, code: &str, message: &str, request_id: &str) -> Response {
    let mut r = (
        status,
        Json(json!({ "error": { "code": code, "message": message, "requestId": request_id } })),
    )
        .into_response();
    r.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    r
}

fn ok(body: Value) -> Response {
    let mut r = (StatusCode::OK, Json(body)).into_response();
    r.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    r
}

fn validation(message: &str, rid: &str) -> Response {
    error(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", message, rid)
}

fn not_found(rid: &str) -> Response {
    error(StatusCode::NOT_FOUND, "NOT_FOUND", "Not found.", rid)
}

fn unavailable(rid: &str) -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "UNAVAILABLE",
        "The service is temporarily unavailable.",
        rid,
    )
}

/// A verified, request-bound `cfmeet.admin-bridge` holding `permission` and naming the
/// HQ staff member it reads for.
async fn authenticate(
    s: &OperatorState,
    request: &Parts,
    permission: InternalPermission,
) -> Result<InternalServiceContext, Box<Response>> {
    let path = request
        .extensions
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_owned())
        .unwrap_or_else(|| request.uri.path().to_owned());
    let ctx = match s
        .verifier
        .verify_request(&request.method, &path, &request.headers)
        .await
    {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::warn!(
                reason = e.failure.as_str(),
                "admin operator API: service authentication rejected"
            );
            return Err(Box::new(error(
                StatusCode::UNAUTHORIZED,
                "INTERNAL_AUTHENTICATION_FAILED",
                "Internal authentication failed.",
                RequestId::generate().as_str(),
            )));
        }
    };
    let rid = ctx.request_id().as_str().to_string();
    if ctx.require_permission(permission).is_err() {
        tracing::warn!(
            caller = ctx.authenticated_service().as_str(),
            permission = permission.as_str(),
            "admin operator API: internal permission denied"
        );
        return Err(Box::new(error(
            StatusCode::FORBIDDEN,
            "INTERNAL_PERMISSION_DENIED",
            "Internal permission denied.",
            &rid,
        )));
    }
    match OperatorAttribution::from_headers(ctx.authenticated_service(), &request.headers) {
        Ok(Some(_)) => Ok(ctx),
        _ => Err(Box::new(error(
            StatusCode::FORBIDDEN,
            "OPERATOR_REQUIRED",
            "A valid X-CF-Operator from the admin bridge is required.",
            &rid,
        ))),
    }
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
}

fn limit_of(q: &PageQuery, rid: &str) -> Result<i64, Box<Response>> {
    match q.limit {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(Box::new(validation("limit must be between 1 and 100", rid))),
    }
}

fn uuid_cursor(q: &PageQuery, rid: &str) -> Result<Option<Uuid>, Box<Response>> {
    q.cursor
        .as_deref()
        .map(|c| Uuid::parse_str(c).map_err(|_| Box::new(validation("cursor is invalid", rid))))
        .transpose()
}

/// `(since, until)`: RFC 3339, `until` after `since`, at most 31 days apart.
fn window(q: &PageQuery, rid: &str) -> Result<Window, Box<Response>> {
    let parse = |v: &Option<String>| -> Result<Option<DateTime<Utc>>, Box<Response>> {
        v.as_deref()
            .map(|s| {
                DateTime::parse_from_rfc3339(s)
                    .map(|t| t.with_timezone(&Utc))
                    .map_err(|_| {
                        Box::new(validation(
                            "since and until must be RFC 3339 timestamps",
                            rid,
                        ))
                    })
            })
            .transpose()
    };
    let (since, until) = (parse(&q.since)?, parse(&q.until)?);
    if let (Some(s), Some(u)) = (since, until) {
        if u <= s || u - s > Duration::days(MAX_WINDOW_DAYS) {
            return Err(Box::new(validation(
                "until must be after since, at most 31 days later",
                rid,
            )));
        }
    }
    Ok((since, until))
}

/// The search term: trimmed, 1–128 characters, LIKE metacharacters escaped.
fn search_term(q: &PageQuery, rid: &str) -> Result<String, Box<Response>> {
    let t = q.q.as_deref().map(str::trim).unwrap_or("");
    if t.is_empty() || t.chars().count() > 128 {
        return Err(Box::new(validation(
            "q is required (1-128 characters)",
            rid,
        )));
    }
    Ok(t.to_lowercase())
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// `<micros>.<uuid>` keyset cursor of newest-first lists without a sequence.
fn time_cursor(q: &PageQuery, rid: &str) -> Result<Option<(DateTime<Utc>, Uuid)>, Box<Response>> {
    let Some(c) = q.cursor.as_deref() else {
        return Ok(None);
    };
    let bad = || Box::new(validation("cursor is invalid", rid));
    let (m, id) = c.split_once('.').ok_or_else(bad)?;
    let micros: i64 = m.parse().map_err(|_| bad())?;
    let at = DateTime::<Utc>::from_timestamp_micros(micros).ok_or_else(bad)?;
    Ok(Some((at, Uuid::parse_str(id).map_err(|_| bad())?)))
}

fn rfc(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn opt_rfc(t: Option<DateTime<Utc>>) -> Value {
    t.map(|t| Value::String(rfc(t))).unwrap_or(Value::Null)
}

/// `a***@d***.com`: first character of the local part and of the domain label, the
/// top-level domain kept. Anything unusual masks completely.
pub fn mask_email(address: &str) -> String {
    let Some((local, domain)) = address.trim().rsplit_once('@') else {
        return "***".into();
    };
    let (label, tld) = domain.rsplit_once('.').unwrap_or((domain, ""));
    let first = |s: &str| s.chars().next().map(|c| c.to_string()).unwrap_or_default();
    if local.is_empty() || label.is_empty() {
        return "***".into();
    }
    if tld.is_empty() {
        format!("{}***@{}***", first(local), first(label))
    } else {
        format!("{}***@{}***.{}", first(local), first(label), tld)
    }
}

async fn rows<T: FromQueryResult>(
    s: &OperatorState,
    sql: &str,
    values: Vec<DbValue>,
) -> Result<Vec<T>, sea_orm::DbErr> {
    T::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
    .all(&s.app.db)
    .await
}

fn page(items: Vec<Value>, next: Option<String>) -> Response {
    ok(json!({ "items": items, "nextCursor": next }))
}

macro_rules! auth {
    ($s:expr, $req:expr, $perm:expr) => {
        match authenticate(&$s, &$req, $perm).await {
            Ok(ctx) => ctx.request_id().as_str().to_string(),
            Err(r) => return *r,
        }
    };
}

macro_rules! try_r {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return *r,
        }
    };
}

fn path_uuid(raw: &str, rid: &str) -> Result<Uuid, Box<Response>> {
    Uuid::parse_str(raw).map_err(|_| Box::new(not_found(rid)))
}

async fn user_exists(s: &OperatorState, id: Uuid) -> Result<bool, sea_orm::DbErr> {
    let r = s
        .app
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS one FROM users WHERE id = $1",
            vec![id.into()],
        ))
        .await?;
    Ok(r.is_some())
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

#[derive(FromQueryResult)]
struct UserRow {
    id: Uuid,
    username: String,
    display_name: Option<String>,
    role: String,
    status: String,
    created_at: DateTime<Utc>,
    last_login_at: Option<DateTime<Utc>>,
}

const USER_COLUMNS: &str = "id, username, display_name, role::text AS role, status::text AS status, \
     (created_at AT TIME ZONE 'UTC') AS created_at, (last_login AT TIME ZONE 'UTC') AS last_login_at";

fn user_json(u: &UserRow) -> Value {
    json!({
        "userId": u.id,
        "username": u.username,
        "displayName": u.display_name,
        "role": u.role,
        "status": u.status,
        "createdAt": rfc(u.created_at),
        "lastLoginAt": opt_rfc(u.last_login_at),
    })
}

/// `GET /internal/v1/operator/accounts/users?q&limit&cursor`: exact email or user id,
/// username prefix or display-name substring. The email is matched, never returned.
async fn users(State(s): State<OperatorState>, Query(q): Query<PageQuery>, req: Parts) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    let term = try_r!(search_term(&q, &rid));
    let esc = like_escape(&term);
    let sql = format!(
        "SELECT {USER_COLUMNS} FROM users WHERE ($1::uuid IS NULL OR id > $1) \
         AND (lower(email) = $2 OR id::text = $2 OR lower(username) LIKE $3 ESCAPE '\\' \
              OR lower(coalesce(display_name, '')) LIKE $4 ESCAPE '\\') \
         ORDER BY id LIMIT $5"
    );
    let found: Vec<UserRow> = match rows(
        &s,
        &sql,
        vec![
            after.into(),
            term.clone().into(),
            format!("{esc}%").into(),
            format!("%{esc}%").into(),
            (limit + 1).into(),
        ],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found.iter().take(limit as usize).map(user_json).collect(),
        next,
    )
}

#[derive(FromQueryResult)]
struct UserDetailRow {
    id: Uuid,
    username: String,
    display_name: Option<String>,
    role: String,
    status: String,
    created_at: DateTime<Utc>,
    last_login_at: Option<DateTime<Utc>>,
    email: String,
    auth_type: String,
    email_verified_at: Option<DateTime<Utc>>,
    locale: Option<String>,
    timezone: Option<String>,
}

/// `GET /internal/v1/operator/accounts/users/{userId}`: one account; the address masked.
async fn user(State(s): State<OperatorState>, Path(id): Path<String>, req: Parts) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    let sql = format!(
        "SELECT {USER_COLUMNS}, email, auth_type::text AS auth_type, email_verified_at, locale, timezone \
         FROM users WHERE id = $1"
    );
    let found: Vec<UserDetailRow> = match rows(&s, &sql, vec![id.into()]).await {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let Some(u) = found.first() else {
        return not_found(&rid);
    };
    ok(json!({
        "userId": u.id,
        "username": u.username,
        "displayName": u.display_name,
        "role": u.role,
        "status": u.status,
        "createdAt": rfc(u.created_at),
        "lastLoginAt": opt_rfc(u.last_login_at),
        "emailMasked": mask_email(&u.email),
        "emailVerified": u.email_verified_at.is_some(),
        "authType": u.auth_type,
        "locale": u.locale,
        "timezone": u.timezone,
    }))
}

#[derive(FromQueryResult)]
struct SessionRow {
    id: Uuid,
    auth_method: String,
    authenticated_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    last_refreshed_at: Option<DateTime<Utc>>,
    absolute_expires_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
    revoke_reason: Option<String>,
}

/// `GET .../users/{userId}/sessions?limit&cursor`: account sessions (no label, no token).
async fn sessions(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    match user_exists(&s, id).await {
        Ok(true) => {}
        Ok(false) => return not_found(&rid),
        Err(_) => return unavailable(&rid),
    }
    let found: Vec<SessionRow> = match rows(
        &s,
        "SELECT id, auth_method, authenticated_at, created_at, last_refreshed_at, absolute_expires_at, \
         revoked_at, revoke_reason FROM account_sessions WHERE user_id = $1 \
         AND ($2::uuid IS NULL OR id > $2) ORDER BY id LIMIT $3",
        vec![id.into(), after.into(), (limit + 1).into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(|r| {
                json!({
                    "sessionId": r.id,
                    "authMethod": r.auth_method,
                    "authenticatedAt": rfc(r.authenticated_at),
                    "createdAt": rfc(r.created_at),
                    "lastRefreshedAt": opt_rfc(r.last_refreshed_at),
                    "expiresAt": rfc(r.absolute_expires_at),
                    "revokedAt": opt_rfc(r.revoked_at),
                    "revokeReason": r.revoke_reason,
                })
            })
            .collect(),
        next,
    )
}

#[derive(FromQueryResult)]
struct DeviceRow {
    id: Uuid,
    device_class: String,
    platform: String,
    trust: String,
    trusted_via: Option<String>,
    trusted_at: Option<DateTime<Utc>>,
    state: String,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
    revoke_reason: Option<String>,
}

/// `GET .../users/{userId}/devices?limit&cursor`: Account devices (no label, key or digest).
async fn devices(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    match user_exists(&s, id).await {
        Ok(true) => {}
        Ok(false) => return not_found(&rid),
        Err(_) => return unavailable(&rid),
    }
    let found: Vec<DeviceRow> = match rows(
        &s,
        "SELECT id, device_class, platform, trust, trusted_via, trusted_at, state, created_at, \
         last_seen_at, revoked_at, revoke_reason FROM account_devices WHERE user_id = $1 \
         AND ($2::uuid IS NULL OR id > $2) ORDER BY id LIMIT $3",
        vec![id.into(), after.into(), (limit + 1).into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(|r| {
                json!({
                    "deviceId": r.id,
                    "deviceClass": r.device_class,
                    "platform": r.platform,
                    "trust": r.trust,
                    "trustedVia": r.trusted_via,
                    "trustedAt": opt_rfc(r.trusted_at),
                    "state": r.state,
                    "createdAt": rfc(r.created_at),
                    "lastSeenAt": rfc(r.last_seen_at),
                    "revokedAt": opt_rfc(r.revoked_at),
                    "revokeReason": r.revoke_reason,
                })
            })
            .collect(),
        next,
    )
}

#[derive(FromQueryResult)]
struct MfaRow {
    confirmed_at: Option<DateTime<Utc>>,
    created_at: Option<DateTime<Utc>>,
    enrolled: bool,
    recovery_remaining: i64,
}

/// `GET .../users/{userId}/mfa`: TOTP enrolment and the count of unused recovery codes.
/// Never the sealed secret or a code.
async fn mfa(State(s): State<OperatorState>, Path(id): Path<String>, req: Parts) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    match user_exists(&s, id).await {
        Ok(true) => {}
        Ok(false) => return not_found(&rid),
        Err(_) => return unavailable(&rid),
    }
    let found: Vec<MfaRow> = match rows(
        &s,
        "SELECT t.confirmed_at, t.created_at, (t.user_id IS NOT NULL) AS enrolled, \
         (SELECT count(*) FROM account_mfa_recovery_codes c WHERE c.user_id = $1 AND c.used_at IS NULL) AS recovery_remaining \
         FROM (SELECT 1) one LEFT JOIN account_mfa_totp t ON t.user_id = $1",
        vec![id.into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let Some(m) = found.first() else {
        return unavailable(&rid);
    };
    let state = match (m.enrolled, m.confirmed_at) {
        (false, _) => "NOT_ENROLLED",
        (true, None) => "PENDING_CONFIRMATION",
        (true, Some(_)) => "ENABLED",
    };
    ok(json!({
        "userId": id,
        "totp": {
            "state": state,
            "enrolledAt": opt_rfc(m.created_at),
            "confirmedAt": opt_rfc(m.confirmed_at),
        },
        "recoveryCodesRemaining": m.recovery_remaining,
    }))
}

#[derive(FromQueryResult)]
struct SecurityRow {
    seq: i64,
    event_id: Uuid,
    session_id: Option<Uuid>,
    event_type: String,
    outcome: String,
    actor_kind: String,
    actor_user_id: Option<Uuid>,
    occurred_at: DateTime<Utc>,
}

/// `GET .../users/{userId}/security-events?since&until&limit&cursor`: newest first by
/// sequence (`cursor` = sequence below). Never the event detail or request id.
async fn security_events(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    let limit = try_r!(limit_of(&q, &rid));
    let (since, until) = try_r!(window(&q, &rid));
    let before: Option<i64> = match q.cursor.as_deref() {
        None => None,
        Some(c) => match c.parse::<i64>() {
            Ok(n) if n > 0 => Some(n),
            _ => return validation("cursor is invalid", &rid),
        },
    };
    match user_exists(&s, id).await {
        Ok(true) => {}
        Ok(false) => return not_found(&rid),
        Err(_) => return unavailable(&rid),
    }
    let found: Vec<SecurityRow> = match rows(
        &s,
        "SELECT seq, event_id, session_id, event_type, outcome, actor_kind, actor_user_id, occurred_at \
         FROM account_security_events WHERE user_id = $1 AND ($2::bigint IS NULL OR seq < $2) \
         AND ($3::timestamptz IS NULL OR occurred_at >= $3) AND ($4::timestamptz IS NULL OR occurred_at < $4) \
         ORDER BY seq DESC LIMIT $5",
        vec![id.into(), before.into(), since.into(), until.into(), (limit + 1).into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].seq.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(|r| {
                json!({
                    "eventId": r.event_id,
                    "sequence": r.seq,
                    "eventType": r.event_type,
                    "outcome": r.outcome,
                    "actorKind": r.actor_kind,
                    "actorUserId": r.actor_user_id,
                    "sessionId": r.session_id,
                    "occurredAt": rfc(r.occurred_at),
                })
            })
            .collect(),
        next,
    )
}

// ---------------------------------------------------------------------------
// Meetings
// ---------------------------------------------------------------------------

#[derive(FromQueryResult)]
struct SpaceRow {
    id: Uuid,
    meeting_code: String,
    title: String,
    meeting_type: String,
    status: String,
    owner_user_id: Option<Uuid>,
    organization_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    archived_at: Option<DateTime<Utc>>,
    version: i32,
}

/// `GET /internal/v1/operator/meetings/spaces?q&limit&cursor`: by id, meeting code or
/// title substring. No description, passcode or policy documents.
async fn spaces(
    State(s): State<OperatorState>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    let term = try_r!(search_term(&q, &rid));
    let esc = like_escape(&term);
    let found: Vec<SpaceRow> = match rows(
        &s,
        "SELECT id, meeting_code, title, meeting_type::text AS meeting_type, status::text AS status, \
         owner_user_id, organization_id, created_at, updated_at, archived_at, version \
         FROM meeting_spaces WHERE ($1::uuid IS NULL OR id > $1) \
         AND (id::text = $2 OR lower(meeting_code) = $2 OR lower(title) LIKE $3 ESCAPE '\\') \
         ORDER BY id LIMIT $4",
        vec![after.into(), term.clone().into(), format!("%{esc}%").into(), (limit + 1).into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(|r| {
                json!({
                    "spaceId": r.id,
                    "meetingCode": r.meeting_code,
                    "title": r.title,
                    "meetingType": r.meeting_type,
                    "status": r.status,
                    "ownerUserId": r.owner_user_id,
                    "organizationId": r.organization_id,
                    "createdAt": rfc(r.created_at),
                    "updatedAt": rfc(r.updated_at),
                    "archivedAt": opt_rfc(r.archived_at),
                    "version": r.version,
                })
            })
            .collect(),
        next,
    )
}

#[derive(FromQueryResult)]
struct ConferenceRow {
    id: Uuid,
    meeting_space_id: Uuid,
    space_title: String,
    kind: String,
    status: String,
    scheduled_start_at: Option<DateTime<Utc>>,
    scheduled_end_at: Option<DateTime<Utc>>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    termination_reason: Option<String>,
    is_locked: bool,
    recording_state: String,
    transcription_state: String,
    created_at: DateTime<Utc>,
    version: i32,
    participants_admitted: i64,
    participants_total: i64,
}

const CONFERENCE_SQL: &str = "SELECT c.id, c.meeting_space_id, s.title AS space_title, c.kind::text AS kind, \
     c.status::text AS status, c.scheduled_start_at, c.scheduled_end_at, c.started_at, c.ended_at, \
     c.termination_reason::text AS termination_reason, c.is_locked, c.recording_state, c.transcription_state, \
     c.created_at, c.version, \
     (SELECT count(*) FROM conference_participants p WHERE p.conference_id = c.id AND p.admission_state::text = 'ADMITTED') AS participants_admitted, \
     (SELECT count(*) FROM conference_participants p WHERE p.conference_id = c.id) AS participants_total \
     FROM conference_sessions c JOIN meeting_spaces s ON s.id = c.meeting_space_id";

fn conference_json(r: &ConferenceRow) -> Value {
    json!({
        "conferenceId": r.id,
        "spaceId": r.meeting_space_id,
        "title": r.space_title,
        "kind": r.kind,
        "status": r.status,
        "scheduledStartAt": opt_rfc(r.scheduled_start_at),
        "scheduledEndAt": opt_rfc(r.scheduled_end_at),
        "startedAt": opt_rfc(r.started_at),
        "endedAt": opt_rfc(r.ended_at),
        "terminationReason": r.termination_reason,
        "locked": r.is_locked,
        "recordingState": r.recording_state,
        "transcriptionState": r.transcription_state,
        "participantsAdmitted": r.participants_admitted,
        "participantsTotal": r.participants_total,
        "createdAt": rfc(r.created_at),
        "version": r.version,
    })
}

/// `GET /internal/v1/operator/meetings/conferences?q&limit&cursor`: conferences of the
/// spaces matching `q` (id, meeting code or title), or the conference with that id.
async fn conferences(
    State(s): State<OperatorState>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    let term = try_r!(search_term(&q, &rid));
    let esc = like_escape(&term);
    let sql = format!(
        "{CONFERENCE_SQL} WHERE ($1::uuid IS NULL OR c.id > $1) \
         AND (c.id::text = $2 OR s.id::text = $2 OR lower(s.meeting_code) = $2 OR lower(s.title) LIKE $3 ESCAPE '\\') \
         ORDER BY c.id LIMIT $4"
    );
    let found: Vec<ConferenceRow> = match rows(
        &s,
        &sql,
        vec![
            after.into(),
            term.clone().into(),
            format!("%{esc}%").into(),
            (limit + 1).into(),
        ],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(conference_json)
            .collect(),
        next,
    )
}

/// `GET /internal/v1/operator/meetings/conferences/{conferenceId}`.
async fn conference(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    let sql = format!("{CONFERENCE_SQL} WHERE c.id = $1");
    let found: Vec<ConferenceRow> = match rows(&s, &sql, vec![id.into()]).await {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    match found.first() {
        Some(r) => ok(conference_json(r)),
        None => not_found(&rid),
    }
}

/// `GET /internal/v1/operator/meetings/live?limit&cursor`: conferences that are LIVE now.
async fn live(State(s): State<OperatorState>, Query(q): Query<PageQuery>, req: Parts) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    let sql = format!(
        "{CONFERENCE_SQL} WHERE c.status::text = 'LIVE' AND ($1::uuid IS NULL OR c.id > $1) ORDER BY c.id LIMIT $2"
    );
    let found: Vec<ConferenceRow> =
        match rows(&s, &sql, vec![after.into(), (limit + 1).into()]).await {
            Ok(r) => r,
            Err(_) => return unavailable(&rid),
        };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(conference_json)
            .collect(),
        next,
    )
}

#[derive(FromQueryResult)]
struct ParticipantRow {
    id: Uuid,
    identity_type: String,
    user_id: Option<Uuid>,
    display_name: String,
    role: String,
    admission_state: String,
    requested_at: DateTime<Utc>,
    admitted_at: Option<DateTime<Utc>>,
    removed_at: Option<DateTime<Utc>>,
    banned_at: Option<DateTime<Utc>>,
}

/// `GET .../conferences/{conferenceId}/participants?limit&cursor`: no guest identity,
/// external reference, credential or denial note.
async fn participants(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    let exists = s
        .app
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS one FROM conference_sessions WHERE id = $1",
            vec![id.into()],
        ))
        .await;
    match exists {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(&rid),
        Err(_) => return unavailable(&rid),
    }
    let found: Vec<ParticipantRow> = match rows(
        &s,
        "SELECT id, identity_type::text AS identity_type, user_id, display_name, role::text AS role, \
         admission_state::text AS admission_state, requested_at, admitted_at, removed_at, banned_at \
         FROM conference_participants WHERE conference_id = $1 AND ($2::uuid IS NULL OR id > $2) \
         ORDER BY id LIMIT $3",
        vec![id.into(), after.into(), (limit + 1).into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(|r| {
                json!({
                    "participantId": r.id,
                    "identityType": r.identity_type,
                    "userId": r.user_id,
                    "displayName": r.display_name,
                    "role": r.role,
                    "admissionState": r.admission_state,
                    "requestedAt": rfc(r.requested_at),
                    "admittedAt": opt_rfc(r.admitted_at),
                    "removedAt": opt_rfc(r.removed_at),
                    "bannedAt": opt_rfc(r.banned_at),
                })
            })
            .collect(),
        next,
    )
}

#[derive(FromQueryResult)]
struct AuditRow {
    id: Uuid,
    meeting_space_id: Uuid,
    conference_id: Option<Uuid>,
    actor_type: String,
    action: String,
    target_type: Option<String>,
    created_at: DateTime<Utc>,
}

/// `GET /internal/v1/operator/meetings/audit-events?since&until&limit&cursor`: newest
/// first (`cursor` = `<micros>.<uuid>`). Never the actor or target reference, metadata or
/// request id.
async fn audit(State(s): State<OperatorState>, Query(q): Query<PageQuery>, req: Parts) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let limit = try_r!(limit_of(&q, &rid));
    let (since, until) = try_r!(window(&q, &rid));
    let cursor = try_r!(time_cursor(&q, &rid));
    let (c_at, c_id) = match cursor {
        Some((a, i)) => (Some(a), Some(i)),
        None => (None, None),
    };
    let found: Vec<AuditRow> = match rows(
        &s,
        "SELECT id, meeting_space_id, conference_id, actor_type, action, target_type, created_at \
         FROM conference_audit_events WHERE ($1::timestamptz IS NULL OR (created_at, id) < ($1, $2::uuid)) \
         AND ($3::timestamptz IS NULL OR created_at >= $3) AND ($4::timestamptz IS NULL OR created_at < $4) \
         ORDER BY created_at DESC, id DESC LIMIT $5",
        vec![c_at.into(), c_id.into(), since.into(), until.into(), (limit + 1).into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| {
        let r = &found[limit as usize - 1];
        format!("{}.{}", r.created_at.timestamp_micros(), r.id)
    });
    page(
        found
            .iter()
            .take(limit as usize)
            .map(|r| {
                json!({
                    "eventId": r.id,
                    "spaceId": r.meeting_space_id,
                    "conferenceId": r.conference_id,
                    "actorType": r.actor_type,
                    "action": r.action,
                    "targetType": r.target_type,
                    "occurredAt": rfc(r.created_at),
                })
            })
            .collect(),
        next,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_are_masked() {
        assert_eq!(mask_email("ada.lovelace@example.com"), "a***@e***.com");
        assert_eq!(mask_email("x@localhost"), "x***@l***");
        assert_eq!(mask_email("broken"), "***");
        assert_eq!(mask_email("@example.com"), "***");
        assert!(!mask_email("ada@example.com").contains("ada@"));
    }

    #[test]
    fn foreign_attribution_is_refused_and_absent_attribution_is_not() {
        let mut h = HeaderMap::new();
        assert!(!attribution_refused(&ServiceId::organizations(), &h));
        h.insert("x-cf-operator", HeaderValue::from_static("hq:staff-1"));
        assert!(attribution_refused(&ServiceId::organizations(), &h));
        assert!(!attribution_refused(&ServiceId::admin_bridge(), &h));
        h.insert("x-cf-operator", HeaderValue::from_static("staff-1"));
        assert!(attribution_refused(&ServiceId::admin_bridge(), &h));
    }
}
