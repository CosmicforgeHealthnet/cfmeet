//! Notification Service owner APIs (Document 4 §3.4, contract
//! `packages/contracts/meeting-service/notification-owner/v1.openapi.json`).
//!
//! The unified Notification Service asks the owners at send time instead of copying their
//! data ("authorities are asked, not copied"). Meeting / Account answers:
//!
//! * **X1** `POST /internal/v1/accounts/notification-contacts/resolve`
//!   (`resolveAccountContacts`, `account-contacts.read`): per `cfmeet-user:<uuid>` (1–100,
//!   no duplicates) the account status, email address, display name, locale, time zone
//!   and the states of its Account devices (ADR 0060). Addresses are returned only to
//!   `cfmeet.notifications`, which writes them only into email ledger rows. An id Account
//!   does not know (or a guest identity, which is not an addressable account) answers
//!   `UNKNOWN` with nulls.
//! * **X2** `POST /internal/v1/conferences/{conferenceId}/notification-recipients/resolve`
//!   (`resolveConferenceRecipients`, `meeting-notification-recipients.read`): for a notice
//!   kind (`SCHEDULED`, `RESCHEDULED`, `CANCELLED`, `REMINDER`) and optionally the start it
//!   announces, the recipients, the presented title, start, end, space time zone, join path
//!   and whether the notice is still true. It runs the SAME rules as the Meeting
//!   notification worker ([`notifications::conference_recipients`],
//!   [`notifications::presented_title`], [`notifications::notice_still_true`]): accounts are
//!   `cfmeet-user:<uuid>` (address resolved through X1, so `email` is null), email-only
//!   participants `meeting-email-principal:<uuid>` with their address; the email-principal
//!   id is derived from the normalized address, so it is stable across notices. The
//!   person who made a change is not known here; the producer's event names them.
//! * **X6** `POST /internal/v1/accounts/notification-route-decisions/leases`
//!   (`claimCriticalSendLeases`, `account-route-lease.claim`) and **X7**
//!   `POST /internal/v1/accounts/notification-route-decisions/completions`
//!   (`completeCriticalSends`, same permission): the CRITICAL-lane fencing token of
//!   Document 4 §9.8 (roadmap N12). Per decision id (1–100, no duplicates), each in its own
//!   row-locked Meeting transaction ([`critical_notifications::claim_lease`],
//!   [`critical_notifications::complete_send`]): X6 answers `GRANTED` (with `leaseUntil`,
//!   Meeting DB clock, `leaseSeconds` 5–30), `REVOKED`, `EXPIRED` or `UNKNOWN` (also while
//!   another lease is live: never two leases); X7 `COMPLETED`, `ALREADY_COMPLETED`,
//!   `NO_LIVE_LEASE` or `UNKNOWN`. A repeated X7 is `ALREADY_COMPLETED`. A database
//!   failure on one X6 decision answers `UNKNOWN` for it (503 when every one fails); on
//!   X7 it answers 503 (completions already recorded stay).
//!
//! Every permission is registry-grantable to `cfmeet.notifications` only, and the
//! receiver trust bundle is authoritative: a caller without the permission is 403, a
//! request without a valid, request-bound service token 401. Errors use the Notification
//! Service shape `{error:{code,message,requestId}}`.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `NOTIFICATION_OWNER_API_ENABLED` | `true` mounts the routes (default off) |
//! | `CF_RUNTIME_ENVIRONMENT` | runtime environment of the trust bundle |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | receiver trust bundle (`cfmeet.meeting-service`) |

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    extract::{OriginalUri, Path, Request, State},
    http::{request::Parts, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use cf_service_auth::{
    InternalPermission, InternalServiceContext, RequestId, RuntimeEnvironment, ServiceId,
    ServiceTokenVerifier, SystemClock, TrustPolicy,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, EntityTrait, FromQueryResult, Statement};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    config::env_bool,
    error::ApiResult,
    models::{conference_sessions, meeting_spaces},
    services::{
        critical_notifications,
        notifications::{self, NoticeKind},
    },
    state::AppState,
};

/// X1 `resolveAccountContacts`.
pub const CONTACTS_PATH: &str = "/internal/v1/accounts/notification-contacts/resolve";
/// X2 `resolveConferenceRecipients` (axum route syntax).
pub const RECIPIENTS_ROUTE: &str =
    "/internal/v1/conferences/:conference_id/notification-recipients/resolve";
/// X6 `claimCriticalSendLeases`.
pub const LEASES_PATH: &str = "/internal/v1/accounts/notification-route-decisions/leases";
/// X7 `completeCriticalSends`.
pub const COMPLETIONS_PATH: &str = "/internal/v1/accounts/notification-route-decisions/completions";
/// Recipients answered by X2 (contract `maxItems`).
pub const MAX_RECIPIENTS: usize = 500;

/// Batch bound of X1 (contract `maxItems`).
pub const MAX_ACCOUNT_REFS: usize = 100;
/// Devices listed per account (contract `maxItems`).
const MAX_DEVICES: usize = 50;
/// Request bodies are reference lists; anything larger is not a request.
const MAX_BODY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct NotificationOwnerConfig {
    pub runtime: RuntimeEnvironment,
    pub trust_bundle_file: PathBuf,
}

impl NotificationOwnerConfig {
    /// `Ok(None)`: off. `Err`: on but misconfigured (startup fails closed).
    pub fn from_env() -> Result<Option<Self>, String> {
        if !env_bool("NOTIFICATION_OWNER_API_ENABLED", false) {
            return Ok(None);
        }
        let var = |name: &str| match std::env::var(name) {
            Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
            _ => Err(format!(
                "{name} is required when NOTIFICATION_OWNER_API_ENABLED is on"
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

#[derive(Clone)]
struct OwnerState {
    app: AppState,
    verifier: Arc<ServiceTokenVerifier>,
}

/// The owner-API router (mounted only when enabled).
pub fn router(app: AppState, verifier: Arc<ServiceTokenVerifier>) -> Router {
    Router::new()
        .route(CONTACTS_PATH, post(resolve_contacts))
        .route(RECIPIENTS_ROUTE, post(resolve_recipients))
        .route(LEASES_PATH, post(claim_leases))
        .route(COMPLETIONS_PATH, post(complete_sends))
        .with_state(OwnerState { app, verifier })
}

// ---------------------------------------------------------------------------
// Errors and authentication
// ---------------------------------------------------------------------------

fn reply(status: StatusCode, body: Value) -> Response {
    let mut r = (status, Json(body)).into_response();
    r.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    r
}

/// The Notification Service error shape (`common/v1.schema.json#/$defs/Error`).
fn error(status: StatusCode, code: &str, message: &str, request_id: &str) -> Response {
    reply(
        status,
        json!({ "error": { "code": code, "message": message, "requestId": request_id } }),
    )
}

fn validation(message: &str, request_id: &str) -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "VALIDATION_ERROR",
        message,
        request_id,
    )
}

fn unavailable(request_id: &str) -> Response {
    let mut r = error(
        StatusCode::SERVICE_UNAVAILABLE,
        "UNAVAILABLE",
        "The owner is temporarily unavailable.",
        request_id,
    );
    r.headers_mut()
        .insert("retry-after", HeaderValue::from_static("5"));
    r
}

/// A verified, request-bound caller holding `permission` (SEC-012: bound to this
/// request's method and received path; single-use on POST).
async fn authenticate(
    s: &OwnerState,
    parts: &Parts,
    permission: InternalPermission,
) -> Result<InternalServiceContext, Response> {
    let path = parts
        .extensions
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_owned())
        .unwrap_or_else(|| parts.uri.path().to_owned());
    let ctx = match s
        .verifier
        .verify_request(&parts.method, &path, &parts.headers)
        .await
    {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::warn!(
                reason = e.failure.as_str(),
                "notification owner API: service authentication rejected"
            );
            return Err(error(
                StatusCode::UNAUTHORIZED,
                "INTERNAL_AUTHENTICATION_FAILED",
                "Internal authentication failed.",
                RequestId::generate().as_str(),
            ));
        }
    };
    if ctx.require_permission(permission).is_err() {
        tracing::warn!(
            caller = ctx.authenticated_service().as_str(),
            permission = permission.as_str(),
            "notification owner API: internal permission denied"
        );
        return Err(error(
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "Internal permission denied.",
            ctx.request_id().as_str(),
        ));
    }
    Ok(ctx)
}

/// The bounded JSON body of `request`, decoded into `T` (unknown fields refused).
async fn body<T: DeserializeOwned>(body: Body, request_id: &str) -> Result<T, Response> {
    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| {
            error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                "The request body is too large.",
                request_id,
            )
        })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        validation(
            "The request body has a missing, unknown or mistyped field.",
            request_id,
        )
    })
}

/// `cfmeet-user:<uuid>` in canonical (lower-case, hyphenated) form.
pub fn parse_account_ref(reference: &str) -> Option<Uuid> {
    let id = Uuid::parse_str(reference.strip_prefix("cfmeet-user:")?).ok()?;
    (format!("cfmeet-user:{id}") == reference).then_some(id)
}

fn bounded(value: Option<String>, max: usize) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v.chars().count() <= max)
}

// ---------------------------------------------------------------------------
// X1 resolveAccountContacts
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContactsRequest {
    account_refs: Vec<String>,
}

/// One X1 result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountContact {
    pub account_ref: String,
    pub status: &'static str,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub locale: Option<String>,
    pub time_zone: Option<String>,
    pub devices: Vec<AccountDeviceState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDeviceState {
    pub account_device_ref: String,
    pub state: &'static str,
}

#[derive(Debug, FromQueryResult)]
struct ContactRow {
    id: Uuid,
    email: String,
    username: String,
    display_name: Option<String>,
    locale: Option<String>,
    timezone: Option<String>,
    status: String,
    role: String,
}

#[derive(Debug, FromQueryResult)]
struct DeviceRow {
    user_id: Uuid,
    id: Uuid,
    state: String,
}

/// The contract status of an account. A pending-verification account is an account in
/// good standing (its verification codes must reach it, and meeting notices already do);
/// a guest identity is not an addressable account.
fn contact_status(status: &str, role: &str) -> &'static str {
    if role == "guest" {
        return "UNKNOWN";
    }
    match status {
        "active" | "pending_verification" => "ACTIVE",
        "inactive" => "DEACTIVATED",
        "suspended" => "SUSPENDED",
        _ => "UNKNOWN",
    }
}

/// ADR 0060 device state: `REPLACEMENT_PENDING` is the contract's `PENDING`.
fn device_state(state: &str) -> &'static str {
    match state {
        "ACTIVE" => "ACTIVE",
        "REPLACEMENT_PENDING" => "PENDING",
        _ => "REVOKED",
    }
}

fn unknown(id: Uuid) -> AccountContact {
    AccountContact {
        account_ref: format!("cfmeet-user:{id}"),
        status: "UNKNOWN",
        email: None,
        display_name: None,
        locale: None,
        time_zone: None,
        devices: Vec::new(),
    }
}

/// X1 for `ids`, in request order. `include_email` is true only for the Notification
/// Service (the only grantee of `account-contacts.read`).
pub async fn account_contacts<C: ConnectionTrait>(
    db: &C,
    ids: &[Uuid],
    include_email: bool,
) -> ApiResult<Vec<AccountContact>> {
    let rows = ContactRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, email, username, display_name, locale, timezone, status::text AS status, role::text AS role \
           FROM users WHERE id = ANY($1)",
        [ids.to_vec().into()],
    ))
    .all(db)
    .await?;
    let devices = DeviceRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT user_id, id, state FROM account_devices WHERE user_id = ANY($1) \
          ORDER BY user_id, (state = 'REVOKED'), created_at DESC, id",
        [ids.to_vec().into()],
    ))
    .all(db)
    .await?;
    let mut by_user: HashMap<Uuid, Vec<AccountDeviceState>> = HashMap::new();
    for d in devices {
        let list = by_user.entry(d.user_id).or_default();
        if list.len() < MAX_DEVICES {
            list.push(AccountDeviceState {
                account_device_ref: d.id.to_string(),
                state: device_state(&d.state),
            });
        }
    }
    let mut accounts: HashMap<Uuid, ContactRow> = rows.into_iter().map(|r| (r.id, r)).collect();
    Ok(ids
        .iter()
        .map(|id| {
            let Some(row) = accounts.remove(id) else {
                return unknown(*id);
            };
            let status = contact_status(&row.status, &row.role);
            if status == "UNKNOWN" {
                return unknown(*id);
            }
            let name = row
                .display_name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(row.username);
            AccountContact {
                account_ref: format!("cfmeet-user:{id}"),
                status,
                email: include_email
                    .then_some(row.email)
                    .and_then(|e| bounded(Some(e), 254)),
                display_name: Some(name.trim().chars().take(200).collect::<String>())
                    .filter(|n| !n.is_empty()),
                locale: bounded(row.locale, 35),
                time_zone: bounded(row.timezone, 64),
                devices: by_user.remove(id).unwrap_or_default(),
            }
        })
        .collect())
}

async fn resolve_contacts(State(s): State<OwnerState>, request: Request) -> Response {
    let (parts, raw) = request.into_parts();
    let ctx = match authenticate(&s, &parts, InternalPermission::AccountContactsRead).await {
        Ok(ctx) => ctx,
        Err(r) => return r,
    };
    let rid = ctx.request_id().as_str().to_string();
    let req: ContactsRequest = match body(raw, &rid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    if req.account_refs.is_empty() || req.account_refs.len() > MAX_ACCOUNT_REFS {
        return validation("accountRefs must hold 1 to 100 references.", &rid);
    }
    let mut ids = Vec::with_capacity(req.account_refs.len());
    for reference in &req.account_refs {
        let Some(id) = parse_account_ref(reference) else {
            return validation("accountRefs must be cfmeet-user:<uuid> references.", &rid);
        };
        if ids.contains(&id) {
            return validation("accountRefs must not repeat a reference.", &rid);
        }
        ids.push(id);
    }
    // Defence in depth: the permission is grantable to the Notification Service only, and
    // addresses go to it alone.
    let include_email = ctx.authenticated_service() == &ServiceId::notifications();
    match account_contacts(&s.app.db, &ids, include_email).await {
        Ok(results) => reply(StatusCode::OK, json!({ "results": results })),
        Err(e) => {
            tracing::error!("notification owner API: contacts lookup failed: {e}");
            unavailable(&rid)
        }
    }
}

// ---------------------------------------------------------------------------
// X2 resolveConferenceRecipients
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum RequestedKind {
    Scheduled,
    Rescheduled,
    Cancelled,
    Reminder,
}

impl From<RequestedKind> for NoticeKind {
    fn from(k: RequestedKind) -> Self {
        match k {
            RequestedKind::Scheduled => NoticeKind::Scheduled,
            RequestedKind::Rescheduled => NoticeKind::Rescheduled,
            RequestedKind::Cancelled => NoticeKind::Cancelled,
            RequestedKind::Reminder => NoticeKind::Reminder,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecipientsRequest {
    kind: RequestedKind,
    #[serde(default)]
    starts_at: Option<DateTime<Utc>>,
}

/// The X2 answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceRecipients {
    pub conference_id: Uuid,
    pub still_true: bool,
    pub presented_title: String,
    pub starts_at: DateTime<Utc>,
    pub time_zone: Option<String>,
    pub join_path: Option<String>,
    pub ends_at: Option<DateTime<Utc>>,
    pub recipients: Vec<RecipientRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecipientRef {
    #[serde(rename = "ref")]
    pub reference: String,
    pub email: Option<String>,
}

/// The stable reference of an email-only participant: a UUID (v8 layout) derived from the
/// normalized address, so every notice to the same address names the same principal.
pub fn email_principal_ref(email: &str) -> String {
    let hash = Sha256::new()
        .chain_update(b"cfmeet.meeting-email-principal.v1:")
        .chain_update(email.trim().to_ascii_lowercase().as_bytes())
        .finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    format!(
        "meeting-email-principal:{}",
        uuid::Builder::from_custom_bytes(bytes).into_uuid()
    )
}

/// The contract's join-path grammar (`^/[A-Za-z0-9-]{1,100}$`); anything else is null.
fn contract_join_path(path: String) -> Option<String> {
    let code = path.strip_prefix('/')?;
    (!code.is_empty()
        && code.len() <= 100
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    .then_some(path)
}

/// X2 for one conference (`None`: the conference or its space does not exist).
/// `announced_start` defaults to the conference's current scheduled start.
pub async fn conference_recipients<C: ConnectionTrait>(
    db: &C,
    conference_id: Uuid,
    kind: NoticeKind,
    announced_start: Option<DateTime<Utc>>,
    max_recipients: usize,
    now: DateTime<Utc>,
) -> ApiResult<Option<ConferenceRecipients>> {
    let Some(conference) = conference_sessions::Entity::find_by_id(conference_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(space) = meeting_spaces::Entity::find_by_id(conference.meeting_space_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let announced = announced_start.or(conference.scheduled_start_at);
    let still_true = (kind == NoticeKind::Cancelled || announced.is_some())
        && notifications::notice_still_true(kind, &space, &conference, announced, now).is_ok();
    let (rows, truncated) = notifications::conference_recipients(
        db,
        space.id,
        conference.id,
        max_recipients.min(MAX_RECIPIENTS),
        None,
    )
    .await?;
    if truncated {
        tracing::warn!(%conference_id, "X2 notification recipients truncated");
    }
    let recipients = rows
        .into_iter()
        .map(|r| match r.user_id {
            // Account addresses are resolved through X1 (the account's own address).
            Some(id) => RecipientRef {
                reference: format!("cfmeet-user:{id}"),
                email: None,
            },
            None => RecipientRef {
                reference: email_principal_ref(&r.email),
                email: Some(r.email),
            },
        })
        .collect();
    Ok(Some(ConferenceRecipients {
        conference_id: conference.id,
        still_true,
        presented_title: notifications::presented_title(&space),
        starts_at: conference
            .scheduled_start_at
            .or(announced_start)
            .unwrap_or(conference.created_at),
        time_zone: bounded(Some(space.timezone.clone()), 64),
        join_path: contract_join_path(notifications::join_path(&space)),
        ends_at: conference.scheduled_end_at,
        recipients,
    }))
}

async fn resolve_recipients(
    State(s): State<OwnerState>,
    Path(raw_id): Path<String>,
    request: Request,
) -> Response {
    let (parts, raw) = request.into_parts();
    let ctx = match authenticate(
        &s,
        &parts,
        InternalPermission::MeetingNotificationRecipientsRead,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(r) => return r,
    };
    let rid = ctx.request_id().as_str().to_string();
    let Some(conference_id) = Uuid::parse_str(&raw_id)
        .ok()
        .filter(|id| id.to_string() == raw_id)
    else {
        return validation("conferenceId must be a UUID.", &rid);
    };
    let req: RecipientsRequest = match body(raw, &rid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let max = s.app.meeting.core.config.notifications.max_recipients;
    match conference_recipients(
        &s.app.db,
        conference_id,
        req.kind.into(),
        req.starts_at,
        max,
        Utc::now(),
    )
    .await
    {
        Ok(Some(answer)) => reply(StatusCode::OK, json!(answer)),
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "The conference does not exist.",
            &rid,
        ),
        Err(e) => {
            tracing::error!("notification owner API: recipients lookup failed: {e}");
            unavailable(&rid)
        }
    }
}

// ---------------------------------------------------------------------------
// X6 claimCriticalSendLeases / X7 completeCriticalSends
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LeasesRequest {
    decision_ids: Vec<String>,
    lease_seconds: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompletionsRequest {
    decision_ids: Vec<String>,
}

/// The contract's decision-id list: 1–100 canonical (lower-case, hyphenated) UUIDs, no
/// duplicates.
fn decision_ids(raw: &[String]) -> Result<Vec<Uuid>, &'static str> {
    if raw.is_empty() || raw.len() > critical_notifications::MAX_BATCH {
        return Err("decisionIds must hold 1 to 100 decision ids.");
    }
    let mut ids = Vec::with_capacity(raw.len());
    for value in raw {
        let Some(id) = Uuid::parse_str(value)
            .ok()
            .filter(|id| &id.to_string() == value)
        else {
            return Err("decisionIds must be lower-case UUIDs.");
        };
        if ids.contains(&id) {
            return Err("decisionIds must not repeat a decision id.");
        }
        ids.push(id);
    }
    Ok(ids)
}

async fn claim_leases(State(s): State<OwnerState>, request: Request) -> Response {
    let (parts, raw) = request.into_parts();
    let ctx = match authenticate(&s, &parts, InternalPermission::AccountRouteLeaseClaim).await {
        Ok(ctx) => ctx,
        Err(r) => return r,
    };
    let rid = ctx.request_id().as_str().to_string();
    let req: LeasesRequest = match body(raw, &rid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let ids = match decision_ids(&req.decision_ids) {
        Ok(ids) => ids,
        Err(message) => return validation(message, &rid),
    };
    if !(critical_notifications::MIN_LEASE_SECONDS..=critical_notifications::MAX_LEASE_SECONDS)
        .contains(&req.lease_seconds)
    {
        return validation("leaseSeconds must be 5 to 30.", &rid);
    }
    let lease = std::time::Duration::from_secs(u64::from(req.lease_seconds));
    let mut results = Vec::with_capacity(ids.len());
    let mut failed = 0;
    for id in ids {
        match critical_notifications::claim_lease(&s.app.db, id, lease).await {
            Ok(answer) => results.push(answer),
            // Nothing was granted for this decision (its transaction rolled back): the
            // service retries it; leases granted to the others are still reported.
            Err(e) => {
                tracing::error!("notification owner API: lease claim failed: {e}");
                failed += 1;
                results.push(critical_notifications::LeaseAnswer {
                    decision_id: id,
                    outcome: "UNKNOWN",
                    lease_until: None,
                });
            }
        }
    }
    if failed == results.len() {
        return unavailable(&rid);
    }
    reply(StatusCode::OK, json!({ "results": results }))
}

async fn complete_sends(State(s): State<OwnerState>, request: Request) -> Response {
    let (parts, raw) = request.into_parts();
    let ctx = match authenticate(&s, &parts, InternalPermission::AccountRouteLeaseClaim).await {
        Ok(ctx) => ctx,
        Err(r) => return r,
    };
    let rid = ctx.request_id().as_str().to_string();
    let req: CompletionsRequest = match body(raw, &rid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let ids = match decision_ids(&req.decision_ids) {
        Ok(ids) => ids,
        Err(message) => return validation(message, &rid),
    };
    let mut results = Vec::with_capacity(ids.len());
    let mut failed = false;
    for id in ids {
        match critical_notifications::complete_send(&s.app.db, id).await {
            Ok(answer) => results.push(answer),
            Err(e) => {
                tracing::error!("notification owner API: completion failed: {e}");
                failed = true;
            }
        }
    }
    // Completions already recorded stay (a retry answers ALREADY_COMPLETED); any failure
    // is reported so the service counts and retries it.
    if failed {
        return unavailable(&rid);
    }
    reply(StatusCode::OK, json!({ "results": results }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_refs_are_canonical() {
        let id = Uuid::new_v4();
        assert_eq!(parse_account_ref(&format!("cfmeet-user:{id}")), Some(id));
        assert_eq!(
            parse_account_ref(&format!(
                "cfmeet-user:{}",
                id.to_string().to_ascii_uppercase()
            )),
            None
        );
        assert_eq!(
            parse_account_ref(&format!("cfmeet-user:{}", id.simple())),
            None
        );
        assert_eq!(
            parse_account_ref(&format!("meeting-email-principal:{id}")),
            None
        );
    }

    #[test]
    fn email_principals_are_stable_per_normalized_address() {
        let a = email_principal_ref("Ada@Example.com ");
        assert_eq!(a, email_principal_ref("ada@example.com"));
        assert_ne!(a, email_principal_ref("bob@example.com"));
        let id = a.strip_prefix("meeting-email-principal:").unwrap();
        assert_eq!(Uuid::parse_str(id).unwrap().to_string(), id);
    }

    #[test]
    fn join_paths_follow_the_contract_grammar() {
        assert_eq!(
            contract_join_path("/abc-defg-hij".into()).as_deref(),
            Some("/abc-defg-hij")
        );
        assert_eq!(contract_join_path("/".into()), None);
        assert_eq!(contract_join_path("/a b".into()), None);
        assert_eq!(contract_join_path("abc".into()), None);
    }

    #[test]
    fn statuses_and_device_states_map_to_the_contract() {
        assert_eq!(contact_status("active", "user"), "ACTIVE");
        assert_eq!(contact_status("pending_verification", "user"), "ACTIVE");
        assert_eq!(contact_status("inactive", "admin"), "DEACTIVATED");
        assert_eq!(contact_status("suspended", "user"), "SUSPENDED");
        assert_eq!(contact_status("active", "guest"), "UNKNOWN");
        assert_eq!(device_state("ACTIVE"), "ACTIVE");
        assert_eq!(device_state("REPLACEMENT_PENDING"), "PENDING");
        assert_eq!(device_state("REVOKED"), "REVOKED");
    }
}
