//! Account-authenticated ingress and Account-owned lifecycle adapter. No Hub database.
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    models::users,
    services::actor::ActorContext,
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
    ApplicationEnvironmentKind, DelegatedPlatformContext, KeyId, OutboundServiceRequest,
    RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier,
    SystemClock, TokenTtl, TrustPolicy,
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
            avatar_reference: None,
            authenticated_at: claims.auth_time,
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
        .post_json(&outbound, "/internal/v1/communications/dispatch", &request)
        .await
        .map_err(|_| unavailable())?;
    let status = StatusCode::from_u16(r.status()).unwrap_or(StatusCode::BAD_GATEWAY);
    let value: Value = r.json().map_err(|_| unavailable())?;
    let mut response = (status, Json(value)).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
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
}
fn internal_scope<'a>(state: &'a AppState, headers: &HeaderMap) -> ApiResult<&'a Bridge> {
    let bridge = state.communications.as_deref().ok_or_else(unavailable)?;
    let ctx = bridge
        .verifier
        .verify_headers(headers)
        .map_err(|_| ApiError::Unauthorized("Internal authentication failed".into()))?;
    ctx.require_permission(cf_service_auth::InternalPermission::CommunicationsAccountRead)
        .map_err(|_| ApiError::Forbidden("Internal permission denied".into()))?;
    let scope = ctx
        .delegated_platform_context()
        .ok_or_else(|| ApiError::Forbidden("Delegated scope required".into()))?;
    if Scope::of(scope) != Scope::of(&bridge.scope) {
        return Err(ApiError::domain(ErrorCode::EndpointNotFound, "Not found"));
    }
    Ok(bridge)
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
    internal_scope(&state, &headers)?;
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
    let version: i64 = row.try_get("", "version")?;
    let cutoff: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("", "credentials_not_before")?;
    Ok(Json(
        json!({"active":!matches!(user.status,users::UserStatus::Inactive|users::UserStatus::Suspended),"version":version,"state":if user.status==users::UserStatus::Inactive {"DEACTIVATED"} else if user.status==users::UserStatus::Suspended {"SUSPENDED"} else {"ACTIVE"},"credentialsNotBefore":cutoff}),
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
    internal_scope(&state, &headers)?;
    if !(1..=100).contains(&body.limit) {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "limit must be 1-100",
        ));
    }
    let records=state.db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT id,username FROM users WHERE id>$1 AND status::text<>'inactive' ORDER BY id LIMIT $2",[body.cursor.unwrap_or(Uuid::nil()).into(),(body.limit as i64+1).into()])).await?;
    let more = records.len() > body.limit as usize;
    let mut accounts = Vec::new();
    let mut last = None;
    for record in records.into_iter().take(body.limit as usize) {
        let id: Uuid = record.try_get("", "id")?;
        let name: String = record.try_get("", "username")?;
        accounts.push(json!({"accountReference":format!("cfmeet-user:{id}"),"displayName":name}));
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
        let ok = bridge
            .client
            .post_json(&req, "/internal/v1/communications/account-events", &payload)
            .await
            .is_ok_and(|r| r.status() == 200);
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
