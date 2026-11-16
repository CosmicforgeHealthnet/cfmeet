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
//!
//! Both permissions are registry-grantable to `cfmeet.notifications` only, and the
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
    extract::{OriginalUri, Request, State},
    http::{request::Parts, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use cf_service_auth::{
    InternalPermission, InternalServiceContext, RequestId, RuntimeEnvironment, ServiceId,
    ServiceTokenVerifier, SystemClock, TrustPolicy,
};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{config::env_bool, error::ApiResult, state::AppState};

/// X1 `resolveAccountContacts`.
pub const CONTACTS_PATH: &str = "/internal/v1/accounts/notification-contacts/resolve";

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
