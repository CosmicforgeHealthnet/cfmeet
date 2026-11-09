//! Account-authenticated ingress and Account-owned lifecycle adapter. No Hub database.
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    models::users,
    services::actor::{ActorContext, ActorKind, RegisteredActor},
    state::AppState,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, post},
    Json, Router,
};
use cf_communications_policy::{Dispatch, Scope, VerifiedAccount};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind, KeyId,
    OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use sea_orm::{ConnectionTrait, DbBackend, EntityTrait, Statement, TransactionTrait};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

pub struct Bridge {
    pub client: Arc<InternalServiceClient>,
    pub verifier: Arc<ServiceTokenVerifier>,
    pub scope: DelegatedPlatformContext,
}
fn unavailable() -> ApiError {
    ApiError::domain(
        ErrorCode::ServiceUnavailable,
        "Personal communications is temporarily unavailable",
    )
}
impl Bridge {
    pub fn from_env() -> Result<Option<Arc<Self>>, String> {
        let Some(url) = std::env::var("CF_COMMUNICATIONS_BASE_URL")
            .ok()
            .filter(|v| !v.is_empty())
        else {
            return Ok(None);
        };
        let required = |k: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{k} is required with Communications"))
        };
        let runtime = RuntimeEnvironment::parse(&required("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| "invalid Communications runtime")?;
        let key = ServiceSigningKey::from_pkcs8_pem_file(
            KeyId::parse(&required("CF_SERVICE_AUTH_KEY_ID")?)
                .map_err(|_| "invalid service key id")?,
            &PathBuf::from(required("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?),
        )
        .map_err(|_| "Communications signer is unavailable")?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            runtime,
            key,
            TokenTtl::seconds(60).map_err(|_| "invalid TTL")?,
            Arc::new(SystemClock),
        ));
        let mut targets = InternalTargets::new(runtime);
        targets
            .insert(ServiceId::communications(), &url)
            .map_err(|_| "invalid Communications URL")?;
        targets
            .insert(
                ServiceId::platform_access(),
                &required("CF_PLATFORM_ACCESS_BASE_URL")?,
            )
            .map_err(|_| "invalid Platform Access URL")?;
        let client = Arc::new(
            InternalServiceClient::new(signer, targets, Duration::from_secs(5))
                .map_err(|_| "invalid Communications client")?,
        );
        let policy = TrustPolicy::from_file(
            &PathBuf::from(required("CF_SERVICE_TRUST_BUNDLE_FILE")?),
            &ServiceId::meeting_service(),
            runtime,
        )
        .map_err(|_| "invalid Account trust bundle")?;
        let verifier = Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)));
        let id =
            |name: &str| Uuid::parse_str(&required(name)?).map_err(|_| format!("invalid {name}"));
        let kind = match required("CFMEET_COMMUNICATION_ENVIRONMENT_KIND")?.as_str() {
            "PRODUCTION" => ApplicationEnvironmentKind::Production,
            "TEST" => ApplicationEnvironmentKind::Test,
            "SANDBOX" => ApplicationEnvironmentKind::Sandbox,
            _ => return Err("invalid application environment kind".into()),
        };
        Ok(Some(Arc::new(Self {
            client,
            verifier,
            scope: DelegatedPlatformContext::new(
                id("CFMEET_COMMUNICATION_TENANT_ID")?,
                id("CFMEET_COMMUNICATION_APPLICATION_ID")?,
                id("CFMEET_COMMUNICATION_ENVIRONMENT_ID")?,
                kind,
                id("CFMEET_COMMUNICATION_SERVICE_ACCOUNT_ID")?,
            ),
        })))
    }
    async fn principal(&self, state: &AppState, user: Uuid) -> ApiResult<Uuid> {
        let existing=state.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT principal_id FROM communication_principal_links WHERE subject_kind='USER' AND subject_id=$1 AND environment_id=$2",
            [user.into(),self.scope.application_environment_id().into()])).await?;
        if let Some(row) = existing {
            return row.try_get("", "principal_id").map_err(Into::into);
        }
        let req = OutboundServiceRequest::new_operation(ServiceId::platform_access())
            .with_delegated_context(self.scope.clone());
        let r = self
            .client
            .post_json(
                &req,
                "/internal/v1/principals/ensure",
                &json!({"kind":"NATIVE_CF_USER","externalReference":format!("cfmeet-user:{user}")}),
            )
            .await
            .map_err(|_| unavailable())?;
        if !(200..300).contains(&r.status()) {
            return Err(unavailable());
        }
        let v: Value = r.json().map_err(|_| unavailable())?;
        let principal = v["principal"]["id"]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or_else(unavailable)?;
        state.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO communication_principal_links(subject_kind,subject_id,environment_id,principal_id,principal_kind) VALUES('USER',$1,$2,$3,'NATIVE_CF_USER') ON CONFLICT DO NOTHING",
            [user.into(),self.scope.application_environment_id().into(),principal.into()])).await?;
        // Read the committed mapping: a concurrent initializer cannot change identity.
        let row=state.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT principal_id FROM communication_principal_links WHERE subject_kind='USER' AND subject_id=$1 AND environment_id=$2",
            [user.into(),self.scope.application_environment_id().into()])).await?.ok_or_else(unavailable)?;
        row.try_get("", "principal_id").map_err(Into::into)
    }
}
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/", any(root))
        .route("/*path", any(relay))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(axum::middleware::map_response(public_error_envelope))
}
async fn public_error_envelope(response: Response) -> Response {
    if response.status().is_success() {
        return response;
    }
    let status = response.status();
    let (parts, body) = response.into_parts();
    if let Ok(bytes) = axum::body::to_bytes(body, 16384).await {
        if serde_json::from_slice::<Value>(&bytes)
            .ok()
            .is_some_and(|v| v["schemaVersion"] == 1 && v["error"]["code"].is_string())
        {
            return Response::from_parts(parts, axum::body::Body::from(bytes));
        }
    }
    let (status, code, message) = match status.as_u16() {
        401 => (
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_REQUIRED",
            "A registered account session is required",
        ),
        403 => (
            StatusCode::FORBIDDEN,
            "PERMISSION_DENIED",
            "The operation is not permitted",
        ),
        404 => (
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "The resource is unavailable",
        ),
        409 => (
            StatusCode::CONFLICT,
            "VERSION_CONFLICT",
            "The resource changed",
        ),
        429 => (
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Too many requests",
        ),
        500..=599 => (
            StatusCode::SERVICE_UNAVAILABLE,
            "COMMUNICATIONS_UNAVAILABLE",
            "Communications is temporarily unavailable",
        ),
        _ => (
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "The request is invalid",
        ),
    };
    let mut response = (
        status,
        Json(json!({"schemaVersion":1,"error":{"code":code,"message":message}})),
    )
        .into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    if status == StatusCode::TOO_MANY_REQUESTS {
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("60"));
    }
    response
}
async fn root() -> ApiError {
    ApiError::domain(
        ErrorCode::EndpointNotFound,
        "Communications endpoint not found",
    )
}
async fn relay(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(path): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult<Response> {
    let user = actor.require_registered()?;
    if !user.revocation_checked {
        return Err(unavailable());
    }
    // ACC-016 (owner decision, off by default): unverified accounts stay out of the Hub.
    // Answered in the Communications error envelope so the code survives
    // `public_error_envelope` (which maps foreign 403s to PERMISSION_DENIED).
    if state
        .auth_policy
        .require_verified_email_for_communications(
            state.require_email_verification,
            user.verified_email.as_deref(),
        )
        .is_err()
    {
        let mut response = (
            StatusCode::FORBIDDEN,
            Json(json!({"schemaVersion":1,"error":{
                "code":"EMAIL_VERIFICATION_REQUIRED",
                "message":"Verify your email address to use communications"}})),
        )
            .into_response();
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
        return Ok(response);
    }
    let bridge = state.communications.as_ref().ok_or_else(unavailable)?;
    if !matches!(
        method,
        Method::GET | Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return Err(ApiError::domain(
            ErrorCode::EndpointNotFound,
            "Communications endpoint not found",
        ));
    }
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::Unauthorized("Sign in to continue".into()))?;
    let claims = state.jwt_service.verify_token(token)?;
    let principal = bridge.principal(&state, user.user_id).await?;
    // Section 14 (ACC-008, ADR 0057 A1): device links are bound to this sign-in, and a
    // device revocation hands back the sign-in it was bound to. Only those two operations
    // carry the session reference.
    let device_op = DeviceOperation::of(&method, &path);
    let account_session_ref = match (device_op, claims.session_id()) {
        (Some(_), Some(session)) => Some(crate::services::account_security::session_reference(
            bridge.scope.application_environment_id(),
            session,
        )),
        _ => None,
    };
    let body =
        if bytes.is_empty() {
            None
        } else {
            Some(serde_json::from_slice::<Value>(&bytes).map_err(|_| {
                ApiError::domain(ErrorCode::ValidationError, "Invalid JSON request")
            })?)
        };
    let idempotency_key = headers
        .get("idempotency-key")
        .map(|v| {
            v.to_str()
                .ok()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| {
                    ApiError::domain(ErrorCode::ValidationError, "Idempotency-Key must be a UUID")
                })
        })
        .transpose()?;
    // Blueprint Section 9: the Account profile is authoritative; the Hub profile's display
    // name and avatar reference are refreshed from it on every relayed request.
    let avatar_reference = users::Entity::find_by_id(user.user_id)
        .one(&state.db)
        .await?
        .and_then(|u| u.avatar_url);
    let request = Dispatch {
        method: method.as_str().to_owned(),
        path,
        query,
        body,
        idempotency_key,
        account: VerifiedAccount {
            account_reference: format!("cfmeet-user:{}", user.user_id),
            principal_id: principal,
            display_name: user.display_name.clone(),
            avatar_reference,
            authenticated_at: claims.auth_time,
            account_session_ref,
        },
    };
    let scope = bridge.scope.clone().with_initiating_principal(
        principal,
        cf_service_auth::DelegatedPrincipalKind::NativeCfUser,
    );
    let outbound = OutboundServiceRequest::new_operation(ServiceId::communications())
        .with_delegated_context(scope);
    let r = bridge
        .client
        .exchange_json(
            &outbound,
            Method::POST,
            "/internal/v1/communications/dispatch",
            Some(&request),
        )
        .await
        .map_err(|_| unavailable())?;
    let status = StatusCode::from_u16(r.status()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut value: Value = r.json().map_err(|_| unavailable())?;
    if value["schemaVersion"] != 1 {
        return Err(unavailable());
    }
    // The bound sign-in of a revoked device ends here, at its owner. The reference is
    // consumed, never relayed. A failure answers 503 so the client retries the
    // (idempotent) revocation, which hands the reference back again.
    if let Some(bound) = value
        .as_object_mut()
        .and_then(|o| o.remove("boundAccountSessionRef"))
    {
        if device_op == Some(DeviceOperation::Revoke) && status == StatusCode::OK {
            if let Some(reference) = bound.as_str() {
                end_bound_session(&state, bridge, user.user_id, reference).await?;
            }
        }
    }
    let retry_after = r.header("retry-after").and_then(|v| v.parse::<u32>().ok());
    let mut response = (status, Json(value)).into_response();
    if let Some(seconds) = retry_after {
        response.headers_mut().insert(
            "retry-after",
            HeaderValue::from_str(&seconds.to_string()).map_err(|_| unavailable())?,
        );
    }
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
}

/// The two Hub device operations that carry the Account session (Section 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceOperation {
    Link,
    Revoke,
}
impl DeviceOperation {
    fn of(method: &Method, path: &str) -> Option<Self> {
        if method != Method::POST {
            return None;
        }
        match path
            .trim_matches('/')
            .split('/')
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["devices", "link"] => Some(Self::Link),
            ["devices", id, "revoke"] if Uuid::parse_str(id).is_ok() => Some(Self::Revoke),
            _ => None,
        }
    }
}

/// End the user's open sign-in bound to a revoked device, with everything bound to it:
/// its session devices (Messaging) and, through the durable account outbox, the other Hub
/// devices it linked. Already-ended sessions are not an error.
async fn end_bound_session(
    state: &AppState,
    bridge: &Bridge,
    user_id: Uuid,
    reference: &str,
) -> ApiResult<()> {
    use crate::services::account_security::{
        self as security, Outcome, RevokeReason, SecurityEvent,
    };
    if !cf_communications_policy::valid_account_session_ref(reference) {
        return Err(unavailable());
    }
    let Some(session) = security::open_session_by_reference(
        &state.db,
        user_id,
        bridge.scope.application_environment_id(),
        reference,
    )
    .await?
    else {
        return Ok(());
    };
    if security::revoke_session(&state.db, user_id, session, RevokeReason::UserRevoked).await? {
        security::record_event_best_effort(
            &state.db,
            SecurityEvent::new("SESSION_REVOKED", Outcome::Success)
                .by_user(user_id)
                .session(session)
                .detail(json!({"trigger":"DEVICE_REVOKED"})),
        )
        .await;
    }
    session_ended(state, user_id, session).await
}

/// Section 14 (ACC-010): an account-wide credential end (password change or reset,
/// deactivation, suspension, a discarded password). The database triggers already
/// advanced both epochs, closed every session and queued the Hub event (which reaches
/// Calling and Messaging even without a Hub profile, and Platform Access); this adds the
/// durable Messaging revocation of the web bridge principal and delivers right away.
/// Never fails the caller: every queue is durable and retried by its worker.
pub async fn credentials_ended(state: &AppState, user_id: Uuid, reason: &'static str) {
    if let Some(um) = state.meeting.core.unified.as_ref() {
        um.revoke_sessions_for_user(user_id, reason).await;
    }
    let _ = deliver_due(state).await;
}

/// One account session ended (single sign-out, session revoke, bound device revoked):
/// its MANAGED-realtime session devices end at once (best effort, bounded by their token
/// lifetime) and the Hub devices it linked end through the durable account outbox.
pub async fn session_ended(state: &AppState, user_id: Uuid, session_id: Uuid) -> ApiResult<()> {
    if let Some(um) = state.meeting.core.unified.as_ref() {
        um.revoke_session_devices_for(user_id, session_id).await;
    }
    record_session_ended(state, user_id, session_id).await?;
    let _ = deliver_due(state).await;
    Ok(())
}

/// Queue ACCOUNT_SESSION_ENDED for the Hub (Section 14, ACC-008). The account epoch and
/// version are unchanged: one sign-in ending is not an account-wide revocation.
pub async fn record_session_ended(
    state: &AppState,
    user_id: Uuid,
    session_id: Uuid,
) -> ApiResult<()> {
    let Some(bridge) = state.communications.as_ref() else {
        return Ok(());
    };
    let txn = state.db.begin().await?;
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO account_communication_state(user_id,version) VALUES($1,1) ON CONFLICT DO NOTHING",[user_id.into()])).await?;
    let row = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT version FROM account_communication_state WHERE user_id=$1",
            [user_id.into()],
        ))
        .await?
        .ok_or_else(unavailable)?;
    let version: i64 = row.try_get("", "version")?;
    let event = Uuid::new_v4();
    let payload = json!({"eventId":event,"accountReference":format!("cfmeet-user:{user_id}"),"sourceVersion":version,
        "kind":"ACCOUNT_SESSION_ENDED","state":"ACCOUNT_CREATED","occurredAt":chrono::Utc::now(),
        "accountSessionRef":crate::services::account_security::session_reference(bridge.scope.application_environment_id(), session_id)});
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO account_communication_outbox(event_id,payload) VALUES($1,$2)",
        [event.into(), payload.into()],
    ))
    .await?;
    txn.commit().await?;
    Ok(())
}

/// ACC-010: mirror the account's CURRENT status onto its Platform Access principal (an
/// inactive account DEACTIVATED, a suspended one SUSPENDED, otherwise ACTIVE), so owners
/// that check Platform Access agree with Account. Reading the current status makes any
/// delivery order converge. `true` when done or nothing to do.
async fn mirror_principal_status(state: &AppState, bridge: &Bridge, user_id: Uuid) -> bool {
    let user = match users::Entity::find_by_id(user_id).one(&state.db).await {
        Ok(u) => u,
        Err(_) => return false,
    };
    let link = state.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT principal_id FROM communication_principal_links WHERE subject_kind='USER' AND subject_id=$1 AND environment_id=$2",
        [user_id.into(),bridge.scope.application_environment_id().into()])).await;
    match link {
        Ok(Some(_)) => {}
        Ok(None) => return true,
        Err(_) => return false,
    }
    let status = principal_status_for(user.as_ref().map(|u| &u.status));
    let req = OutboundServiceRequest::new_operation(ServiceId::platform_access())
        .with_delegated_context(bridge.scope.clone());
    match bridge
        .client
        .post_json(
            &req,
            "/internal/v1/principals/account-status",
            &json!({"externalReference":format!("cfmeet-user:{user_id}"),"status":status}),
        )
        .await
    {
        // 404: no principal to mirror (or a Platform Access image without the route);
        // 409 INVALID_STATE: a tombstoned principal stays DEACTIVATED for ever.
        Ok(r) => matches!(r.status(), 200 | 404 | 409),
        Err(_) => false,
    }
}

/// Platform Access principal status for an account status (a missing account is gone).
fn principal_status_for(status: Option<&users::UserStatus>) -> &'static str {
    match status {
        None | Some(users::UserStatus::Inactive) => "DEACTIVATED",
        Some(users::UserStatus::Suspended) => "SUSPENDED",
        Some(_) => "ACTIVE",
    }
}

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/internal/v1/accounts/communications/status",
            post(account_status),
        )
        .route(
            "/internal/v1/accounts/communications/backfill",
            post(backfill),
        )
        .route(INSTANT_MEETING_PATH, post(instant_meeting))
        .route(RESOLVE_MEETING_PATH, post(resolve_meeting))
}
/// MT-1 (Sections 9–23): the Hub starts an instant meeting owned by the delegated user.
pub const INSTANT_MEETING_PATH: &str = "/internal/v1/communications/meetings/instant";
/// MT-2 (Sections 9–23): the Hub resolves a meeting code to public references.
pub const RESOLVE_MEETING_PATH: &str = "/internal/v1/communications/meetings/resolve";
fn internal_scope<'a>(state: &'a AppState, headers: &HeaderMap) -> ApiResult<&'a Bridge> {
    verified_scope(
        state,
        headers,
        cf_service_auth::InternalPermission::CommunicationsAccountRead,
    )
    .map(|(bridge, _)| bridge)
}
/// Signed caller with `permission` in this receiver's trust policy, delegated to the
/// governed personal scope (any other scope is 404). Returns the delegated initiating
/// principal when the hop carries one.
fn verified_scope<'a>(
    state: &'a AppState,
    headers: &HeaderMap,
    permission: cf_service_auth::InternalPermission,
) -> ApiResult<(&'a Bridge, Option<(Uuid, DelegatedPrincipalKind)>)> {
    let bridge = state.communications.as_deref().ok_or_else(unavailable)?;
    let ctx = bridge
        .verifier
        .verify_headers(headers)
        .map_err(|_| ApiError::Unauthorized("Internal authentication failed".into()))?;
    ctx.require_permission(permission)
        .map_err(|_| ApiError::Forbidden("Internal permission denied".into()))?;
    let scope = ctx
        .delegated_platform_context()
        .ok_or_else(|| ApiError::Forbidden("Delegated scope required".into()))?;
    if Scope::of(scope) != Scope::of(&bridge.scope) {
        return Err(ApiError::domain(ErrorCode::EndpointNotFound, "Not found"));
    }
    let initiating = scope
        .initiating_principal()
        .map(|p| (p.principal_id, p.principal_kind));
    Ok((bridge, initiating))
}
fn denied() -> ApiError {
    ApiError::domain(
        ErrorCode::PermissionDenied,
        "The operation is not permitted",
    )
}
fn account_id(reference: &str) -> ApiResult<Uuid> {
    reference
        .strip_prefix("cfmeet-user:")
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| ApiError::domain(ErrorCode::ValidationError, "Invalid account reference"))
}
/// The registered actor the Hub acts for: the delegated initiating principal must be a
/// native CF user equal to `principal_id`, and `account_reference` must be the active,
/// non-guest account Account maps to that principal (`communication_principal_links`).
/// Anything else is 403 — the Hub never acts for another user.
async fn delegated_actor(
    state: &AppState,
    headers: &HeaderMap,
    permission: cf_service_auth::InternalPermission,
    principal_id: Uuid,
    account_reference: &str,
) -> ApiResult<ActorContext> {
    let (bridge, initiating) = verified_scope(state, headers, permission)?;
    if initiating != Some((principal_id, DelegatedPrincipalKind::NativeCfUser)) {
        return Err(denied());
    }
    let user_id = account_id(account_reference)?;
    let link = state.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT principal_id FROM communication_principal_links WHERE subject_kind='USER' AND subject_id=$1 AND environment_id=$2",
        [user_id.into(),bridge.scope.application_environment_id().into()])).await?;
    let linked = link
        .map(|r| r.try_get::<Uuid>("", "principal_id"))
        .transpose()?;
    if linked != Some(principal_id) {
        return Err(denied());
    }
    let user = users::Entity::find_by_id(user_id)
        .one(&state.db)
        .await?
        .filter(|u| {
            !matches!(
                u.status,
                users::UserStatus::Inactive | users::UserStatus::Suspended
            ) && u.role != users::UserRole::Guest
        })
        .ok_or_else(denied)?;
    // Same identity rule as `resolve_actor` for a verified session of this account.
    let email_verified = user.email_verified(state.require_email_verification);
    let verified_organizations = state
        .organization_directory
        .organizations_for_user(user.id)
        .await;
    Ok(ActorContext {
        kind: ActorKind::RegisteredUser(RegisteredActor {
            user_id: user.id,
            display_name: user.presentation_name(),
            verified_email: email_verified.then(|| user.email.to_ascii_lowercase()),
            platform_admin: user.role == users::UserRole::Admin,
            // The Hub reaches this route only for a session the Account relay verified
            // against the revocation store (`relay` refuses unchecked sessions).
            revocation_checked: true,
        }),
        verified_organizations,
        request_id: crate::middleware::request_id::current_request_id(),
        client_kind: None,
    })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstantMeeting {
    principal_id: Uuid,
    account_reference: String,
    idempotency_key: Uuid,
    title: Option<String>,
}
/// MT-1: the same domain path as `POST /api/v2/instant-meetings`, acting as the mapped
/// account (ownership, v2 enablement and space validation unchanged). Idempotent per
/// (account, key): a replay returns the first meeting and never creates another.
async fn instant_meeting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<InstantMeeting>,
) -> ApiResult<Json<Value>> {
    let actor = delegated_actor(
        &state,
        &headers,
        cf_service_auth::InternalPermission::MeetingCommunicationsCreate,
        body.principal_id,
        &body.account_reference,
    )
    .await?;
    if body.title.as_ref().is_some_and(|t| t.chars().count() > 120) {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "title must be at most 120 characters",
        ));
    }
    let user_id = actor.user_id().ok_or_else(denied)?;
    // Serialize concurrent requests for one (account, key) for the lifetime of this
    // transaction; the lock is released on commit, rollback or a dropped connection.
    let txn = state.db.begin().await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended('communication_instant_meetings:'||$1::text||':'||$2::text,0))",
        [user_id.into(), body.idempotency_key.into()],
    ))
    .await?;
    let existing = txn.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT meeting_space_id,conference_id FROM communication_instant_meetings WHERE user_id=$1 AND idempotency_key=$2",
        [user_id.into(),body.idempotency_key.into()])).await?;
    if let Some(row) = existing {
        txn.commit().await?;
        let space_id: Uuid = row.try_get("", "meeting_space_id")?;
        let conference_id: Uuid = row.try_get("", "conference_id")?;
        let space = crate::models::meeting_spaces::Entity::find_by_id(space_id)
            .one(&state.db)
            .await?
            .ok_or_else(crate::services::meeting_core::not_found_space)?;
        return Ok(Json(instant_result(&space, conference_id, true)));
    }
    let (space, conference) = state
        .meeting
        .spaces
        .create_instant(
            &actor,
            crate::dto::v2::InstantMeetingRequest {
                title: body.title,
                ..Default::default()
            },
        )
        .await?;
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO communication_instant_meetings(user_id,idempotency_key,meeting_space_id,conference_id) VALUES($1,$2,$3,$4)",
        [user_id.into(),body.idempotency_key.into(),space.id.into(),conference.id.into()])).await?;
    txn.commit().await?;
    Ok(Json(instant_result(&space, conference.id, false)))
}
fn instant_result(
    space: &crate::models::meeting_spaces::Model,
    conference_id: Uuid,
    replayed: bool,
) -> Value {
    json!({"schemaVersion":1,"meetingSpaceId":space.id,"conferenceId":conference_id,"meetingCode":space.meeting_code,"createdAt":space.created_at,"replayed":replayed})
}

/// The Hub's coarse view of a persistent space role: OWNER, HOST, PARTICIPANT (any other
/// membership role) or NONE.
fn public_role(role: Option<crate::domain::Role>) -> &'static str {
    match role {
        Some(crate::domain::Role::Owner) => "OWNER",
        Some(crate::domain::Role::Host) => "HOST",
        Some(_) => "PARTICIPANT",
        None => "NONE",
    }
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveMeeting {
    principal_id: Uuid,
    account_reference: String,
    meeting_code: String,
}
/// MT-2: resolve a meeting code for the delegated user. Public references only — never a
/// passcode, invitation, participant token or media credential. `shareable` only for the
/// owner or a host of the space.
async fn resolve_meeting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ResolveMeeting>,
) -> ApiResult<Json<Value>> {
    let actor = delegated_actor(
        &state,
        &headers,
        cf_service_auth::InternalPermission::MeetingCommunicationsRead,
        body.principal_id,
        &body.account_reference,
    )
    .await?;
    let code = body.meeting_code.trim();
    if code.len() > 64 {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "meetingCode is invalid",
        ));
    }
    // Codes only: space or legacy UUIDs are not shareable references.
    if !crate::services::secrets::is_v2_meeting_code(&code.to_ascii_lowercase())
        && !crate::services::secrets::is_legacy_meeting_code(&code.to_ascii_uppercase())
    {
        return Err(crate::services::meeting_core::not_found_space());
    }
    let resolution = state.meeting.compat.resolve(code).await?;
    let view = state
        .meeting
        .spaces
        .lookup_view_of(&actor, resolution.space)
        .await?;
    let response = crate::handlers::v2::views::space(&view);
    let role = public_role(view.viewer_role);
    let latest = view
        .latest_conference
        .as_ref()
        .map(|c| json!({"id":c.id,"status":c.status,"scheduledStartAt":c.scheduled_start_at}));
    Ok(Json(
        json!({"schemaVersion":1,"meetingSpaceId":response.id,"meetingCode":response.meeting_code,"title":response.title,"shareable":matches!(role,"OWNER"|"HOST"),"role":role,"latestConference":latest}),
    ))
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccountStatus {
    account_reference: String,
}
async fn account_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AccountStatus>,
) -> ApiResult<Json<Value>> {
    let bridge = internal_scope(&state, &headers)?;
    let id = body
        .account_reference
        .strip_prefix("cfmeet-user:")
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| ApiError::domain(ErrorCode::ValidationError, "Invalid account reference"))?;
    let Some(user) = users::Entity::find_by_id(id).one(&state.db).await? else {
        return Ok(Json(
            json!({"active":false,"version":0,"state":"DEACTIVATED"}),
        ));
    };
    state.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO account_communication_state(user_id,version) VALUES($1,1) ON CONFLICT DO NOTHING",[id.into()])).await?;
    let row=state.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT version,credentials_not_before FROM account_communication_state WHERE user_id=$1",[id.into()])).await?.ok_or_else(unavailable)?;
    let link=state.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT principal_id FROM communication_principal_links WHERE subject_kind='USER' AND subject_id=$1 AND environment_id=$2",[id.into(),bridge.scope.application_environment_id().into()])).await?;
    let principal = link
        .map(|r| r.try_get::<Uuid>("", "principal_id"))
        .transpose()?;
    let version: i64 = row.try_get("", "version")?;
    let cutoff: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("", "credentials_not_before")?;
    Ok(Json(
        json!({"active":user.role!=users::UserRole::Guest && !matches!(user.status,users::UserStatus::Inactive|users::UserStatus::Suspended),"version":version,"state":if user.status==users::UserStatus::Inactive {"DEACTIVATED"} else if user.status==users::UserStatus::Suspended {"SUSPENDED"} else {"ACTIVE"},"credentialsNotBefore":cutoff,"principalId":principal}),
    ))
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Backfill {
    limit: u64,
    cursor: Option<Uuid>,
}
async fn backfill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Backfill>,
) -> ApiResult<Json<Value>> {
    let bridge = internal_scope(&state, &headers)?;
    if !(1..=100).contains(&body.limit) {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "limit must be 1-100",
        ));
    }
    let records=state.db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT id,COALESCE(display_name,username) AS username FROM users WHERE id>$1 AND status::text NOT IN ('inactive','suspended') AND role::text<>'guest' ORDER BY id LIMIT $2",[body.cursor.unwrap_or(Uuid::nil()).into(),(body.limit as i64+1).into()])).await?;
    let more = records.len() > body.limit as usize;
    let mut accounts = Vec::new();
    let mut last = None;
    for record in records.into_iter().take(body.limit as usize) {
        let id: Uuid = record.try_get("", "id")?;
        let name: String = record.try_get("", "username")?;
        let principal = bridge.principal(&state, id).await?;
        accounts.push(json!({"accountReference":format!("cfmeet-user:{id}"),"displayName":name,"principalId":principal}));
        last = Some(id);
    }
    Ok(Json(
        json!({"accounts":accounts,"nextCursor":if more {last} else {None}}),
    ))
}

pub async fn record_logout(state: &AppState, user: Uuid) -> ApiResult<()> {
    if state.communications.is_none() {
        return Ok(());
    }
    let txn = state.db.begin().await?;
    let row=txn.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO account_communication_state(user_id,version,credentials_not_before) VALUES($1,1,now()) ON CONFLICT(user_id) DO UPDATE SET version=account_communication_state.version+1,credentials_not_before=now() RETURNING version,credentials_not_before",[user.into()])).await?.ok_or_else(unavailable)?;
    let version: i64 = row.try_get("", "version")?;
    let at: chrono::DateTime<chrono::Utc> = row.try_get("", "credentials_not_before")?;
    let payload = json!({"eventId":Uuid::new_v4(),"accountReference":format!("cfmeet-user:{user}"),"sourceVersion":version,"kind":"SESSION_REVOKED","state":"ACCOUNT_CREATED","occurredAt":at});
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO account_communication_outbox(event_id,payload) VALUES($1,$2)",
        [
            Uuid::parse_str(payload["eventId"].as_str().unwrap())
                .map_err(|_| unavailable())?
                .into(),
            payload.into(),
        ],
    ))
    .await?;
    txn.commit().await?;
    Ok(())
}
pub async fn deliver_due(state: &AppState) -> ApiResult<u64> {
    let Some(bridge) = &state.communications else {
        return Ok(0);
    };
    let lease = Uuid::new_v4();
    let records=state.db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "WITH due AS(SELECT event_id FROM account_communication_outbox WHERE delivered_at IS NULL AND next_attempt_at<=now() AND (lease_until IS NULL OR lease_until<now()) ORDER BY created_at LIMIT 20 FOR UPDATE SKIP LOCKED) UPDATE account_communication_outbox o SET lease_id=$1,lease_until=now()+interval '120 seconds',attempts=attempts+1 FROM due WHERE o.event_id=due.event_id RETURNING o.event_id,o.payload,o.attempts",[lease.into()])).await?;
    let mut count = 0;
    for row in records {
        let id: Uuid = row.try_get("", "event_id")?;
        let payload: Value = row.try_get("", "payload")?;
        let attempts: i32 = row.try_get("", "attempts")?;
        let req = OutboundServiceRequest::new_operation(ServiceId::communications())
            .with_delegated_context(bridge.scope.clone());
        let mut ok = bridge
            .client
            .post_json(&req, "/internal/v1/communications/account-events", &payload)
            .await
            .is_ok_and(|r| r.status() == 200);
        // ACC-010: the same durable event mirrors the account status onto Platform
        // Access. A failure retries the whole event; the Hub answers a retry as a
        // duplicate.
        if ok && payload["kind"] != "ACCOUNT_SESSION_ENDED" {
            if let Some(user_id) = payload["accountReference"]
                .as_str()
                .and_then(|r| r.strip_prefix("cfmeet-user:"))
                .and_then(|r| Uuid::parse_str(r).ok())
            {
                ok = mirror_principal_status(state, bridge, user_id).await;
            }
        }
        if ok {
            state.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE account_communication_outbox SET delivered_at=now(),lease_id=NULL,lease_until=NULL WHERE event_id=$1 AND lease_id=$2",[id.into(),lease.into()])).await?;
            count += 1;
        } else {
            state.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE account_communication_outbox SET lease_id=NULL,lease_until=NULL,next_attempt_at=now()+make_interval(secs=>$3) WHERE event_id=$1 AND lease_id=$2",[id.into(),lease.into(),2i32.pow(attempts.min(10) as u32).into()])).await?;
        }
    }
    Ok(count)
}

/// Refresh never changes the original authentication time or revives revoked credentials.
pub async fn require_fresh_credentials(
    state: &AppState,
    user: Uuid,
    authenticated_at: Option<i64>,
) -> ApiResult<()> {
    if state.communications.is_none() {
        return Ok(());
    }
    let row = state
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT credentials_not_before FROM account_communication_state WHERE user_id=$1",
            [user.into()],
        ))
        .await?;
    if let Some(row) = row {
        let cutoff: Option<chrono::DateTime<chrono::Utc>> =
            row.try_get("", "credentials_not_before")?;
        if cutoff.is_some_and(|t| authenticated_at.is_none_or(|a| a <= t.timestamp())) {
            return Err(ApiError::Unauthorized(
                "Credentials have been revoked".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Role;

    #[test]
    fn only_owner_and_host_are_distinguished_roles() {
        assert_eq!(public_role(Some(Role::Owner)), "OWNER");
        assert_eq!(public_role(Some(Role::Host)), "HOST");
        for r in [
            Role::CoHost,
            Role::Moderator,
            Role::Presenter,
            Role::Participant,
            Role::Viewer,
        ] {
            assert_eq!(public_role(Some(r)), "PARTICIPANT");
        }
        assert_eq!(public_role(None), "NONE");
    }

    #[test]
    fn only_device_link_and_revoke_carry_the_account_session() {
        let id = Uuid::new_v4();
        assert_eq!(
            DeviceOperation::of(&Method::POST, "devices/link"),
            Some(DeviceOperation::Link)
        );
        assert_eq!(
            DeviceOperation::of(&Method::POST, &format!("/devices/{id}/revoke")),
            Some(DeviceOperation::Revoke)
        );
        for (method, path) in [
            (Method::GET, "devices".to_owned()),
            (Method::GET, "devices/link".to_owned()),
            (Method::POST, "devices/not-a-uuid/revoke".to_owned()),
            (Method::PUT, format!("devices/{id}/push")),
            (Method::POST, "me/public-id/rotate".to_owned()),
        ] {
            assert_eq!(DeviceOperation::of(&method, &path), None, "{method} {path}");
        }
    }

    #[test]
    fn principal_status_mirrors_the_account_status() {
        use users::UserStatus;
        assert_eq!(principal_status_for(None), "DEACTIVATED");
        assert_eq!(
            principal_status_for(Some(&UserStatus::Inactive)),
            "DEACTIVATED"
        );
        assert_eq!(
            principal_status_for(Some(&UserStatus::Suspended)),
            "SUSPENDED"
        );
        assert_eq!(principal_status_for(Some(&UserStatus::Active)), "ACTIVE");
        assert_eq!(
            principal_status_for(Some(&UserStatus::PendingVerification)),
            "ACTIVE"
        );
    }

    #[test]
    fn session_references_are_domain_separated_per_environment() {
        let (env, other, session) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let r = crate::services::account_security::session_reference(env, session);
        assert!(cf_communications_policy::valid_account_session_ref(&r));
        assert_ne!(
            r,
            crate::services::account_security::session_reference(other, session)
        );
        assert_eq!(
            r,
            crate::services::account_security::session_reference(env, session)
        );
    }

    #[test]
    fn account_references_are_cfmeet_user_uuids() {
        let id = Uuid::new_v4();
        assert_eq!(account_id(&format!("cfmeet-user:{id}")).unwrap(), id);
        for bad in [id.to_string(), format!("user:{id}"), "cfmeet-user:x".into()] {
            assert!(account_id(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn mt_bodies_refuse_unknown_fields() {
        let p = Uuid::new_v4();
        let ok = json!({"principalId":p,"accountReference":"cfmeet-user:x","idempotencyKey":p});
        assert!(serde_json::from_value::<InstantMeeting>(ok.clone()).is_ok());
        let mut extra = ok;
        extra["ownerUserId"] = json!(p);
        assert!(serde_json::from_value::<InstantMeeting>(extra).is_err());
        let resolve =
            json!({"principalId":p,"accountReference":"r","meetingCode":"c","passcode":"1"});
        assert!(serde_json::from_value::<ResolveMeeting>(resolve).is_err());
    }
}
