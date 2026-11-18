//! Partner Users API and user-token minting (ADR 0062; contract
//! `docs/platform/build-internal-contracts.md` §2.1). Developer Console only
//! (`platform-end-users.delegate`); behind `PLATFORM_ACCESS_END_USERS_ENABLED` (minting
//! also behind `PLATFORM_ACCESS_USER_TOKENS_ENABLED`). With a flag off every route
//! answers exactly like an unknown route (`404 NOT_FOUND`).
//!
//! | Route | Credential scope | Result |
//! |---|---|---|
//! | `POST /end-users` | `principals.write` | `201 {user}` new · `200 {user}` existing |
//! | `GET /end-users?externalId=&cursor=&limit=` | `principals.read` | `{items, nextCursor}` |
//! | `GET /end-users/{userId}` | `principals.read` | `{user}` |
//! | `PATCH /end-users/{userId}` | `principals.write` | `{user}` |
//! | `DELETE /end-users/{userId}` | `principals.write` | `204` (tombstone) |
//! | `POST /end-users/{userId}/revoke-sessions` | `principals.write` | `{tokensNotBefore}` |
//! | `POST /end-users/{userId}/tokens` | `user_tokens.issue` + grant scopes | `201 {token, tokenId, expiresAt}` |
//! | `POST /end-users/tokens` | `principals.write` + `user_tokens.issue` + grant scopes | `201 {user, token, tokenId, expiresAt}` |
//! | `GET /delegation/environments/{environmentId}/end-users` | — (`platform-identity.delegate`, console read) | `{items, nextCursor}` |
//!
//! Every partner request carries `CF-Forwarded-Credential-Id`: the API credential the
//! gateway authenticated. It is re-verified LIVE here (revocation, expiry, suspension,
//! service account, current scopes) and is the ONLY source of the environment; its tenant
//! must be EXTERNAL (`POLICY_VIOLATION`) and its chain ACTIVE (`INVALID_STATE`). A user of
//! another environment is `NOT_FOUND`.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use uuid::Uuid;

use super::{
    delegation::read_only,
    error::{AccessError, AccessResult, ErrorCode},
    middleware::InternalCaller,
};
use crate::{
    domain::{EnvironmentId, PrincipalStatus, TenantClassification},
    identity::{
        authentication::{AuthenticationError, AuthorityChain, AuthorityMode},
        end_users::{self, EndUserError, EndUserRow, EndUserScope, EndUserView, ProfileInput},
        operator::OperatorContext,
        scopes::names,
        user_tokens::{
            Grants, UserTokenClaims, DEFAULT_TTL_SECONDS, MAX_TOKEN_BYTES, MAX_TTL_SECONDS,
            MIN_TTL_SECONDS,
        },
    },
    state::AppState,
};

/// The credential the gateway authenticated, forwarded by the Developer Console.
pub const FORWARDED_CREDENTIAL_HEADER: &str = "cf-forwarded-credential-id";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/end-users", post(create_user).get(list_users))
        .route("/end-users/tokens", post(upsert_and_mint))
        .route(
            "/end-users/:user_id",
            get(get_user).patch(update_user).delete(delete_user),
        )
        .route("/end-users/:user_id/revoke-sessions", post(revoke_sessions))
        .route("/end-users/:user_id/tokens", post(mint_for_user))
        .route(
            "/delegation/environments/:environment_id/end-users",
            get(console_list_users),
        )
}

fn disabled() -> AccessError {
    AccessError::Refused(ErrorCode::NotFound)
}

fn internal(err: sea_orm::DbErr) -> AccessError {
    AccessError::service(err.into())
}

fn user_err(err: EndUserError) -> AccessError {
    match err {
        EndUserError::LimitExceeded => {
            crate::metrics::inc("platform_access_end_user_limit_exceeded_total", &[]);
            AccessError::Refused(ErrorCode::LimitExceeded)
        }
        EndUserError::Platform(e) => AccessError::operator(e),
    }
}

/// The re-verified forwarded credential and the scope it acts in.
struct Partner {
    chain: AuthorityChain,
    scope: EndUserScope,
    op: OperatorContext,
}

impl Partner {
    /// `403 SCOPE_REQUIRED` unless the credential's CURRENT scopes hold every `scope`.
    fn require<'a>(&self, scopes: impl IntoIterator<Item = &'a str>) -> AccessResult<()> {
        for scope in scopes {
            if !self.chain.scopes.iter().any(|s| s == scope) {
                crate::metrics::inc("platform_access_end_user_scope_required_total", &[]);
                return Err(AccessError::Operator(
                    ErrorCode::ScopeRequired,
                    format!("the credential lacks the {scope} scope"),
                ));
            }
        }
        Ok(())
    }

    fn cap(&self, state: &AppState) -> i64 {
        state.build.end_user_caps.for_kind(self.scope.kind)
    }
}

async fn partner(
    state: &AppState,
    caller: &InternalCaller,
    headers: &HeaderMap,
) -> AccessResult<Partner> {
    if !state.build.end_users_enabled {
        return Err(disabled());
    }
    caller.require_permission(InternalPermission::PlatformEndUsersDelegate)?;
    let credential_id = headers
        .get(FORWARDED_CREDENTIAL_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v.trim()).ok())
        .ok_or(AccessError::Refused(ErrorCode::DelegatedContextRequired))?;
    let invalid = || {
        crate::metrics::inc("platform_access_delegated_context_rejected_total", &[]);
        AccessError::Refused(ErrorCode::DelegatedContextInvalid)
    };
    let delegated = caller.context().delegated_platform_context();
    if delegated
        .and_then(|ctx| ctx.initiating_credential_id())
        .is_some_and(|c| c != credential_id)
    {
        return Err(invalid());
    }
    let chain = match state.authenticator.authority_of(credential_id).await {
        Ok(chain) => chain,
        Err(AuthenticationError::Rejected { reason, .. }) => {
            tracing::info!(
                caller = caller.service(),
                reason = reason.as_str(),
                "forwarded credential no longer authenticates"
            );
            return Err(invalid());
        }
        Err(AuthenticationError::Unavailable { summary }) => {
            tracing::error!(%summary, "credential authority unavailable");
            return Err(AccessError::Refused(ErrorCode::ServiceUnavailable));
        }
    };
    if delegated.is_some_and(|ctx| {
        ctx.tenant_id() != chain.tenant.id
            || ctx.application_id() != chain.application.id
            || ctx.application_environment_id() != chain.environment.id
    }) {
        return Err(invalid());
    }
    if chain.tenant.classification != TenantClassification::External {
        return Err(AccessError::Operator(
            ErrorCode::PolicyViolation,
            "partner users exist only in EXTERNAL tenants".to_string(),
        ));
    }
    if chain.authority_mode != AuthorityMode::Full {
        return Err(AccessError::Operator(
            ErrorCode::InvalidState,
            "the credential's tenant, application or environment is suspended".to_string(),
        ));
    }
    let op = OperatorContext::new(&format!("devplat.s-{}", chain.service_account.id.simple()))
        .map_err(|_| AccessError::Refused(ErrorCode::InternalError))?;
    let scope = EndUserScope {
        tenant_id: chain.tenant.id,
        application_id: chain.application.id,
        environment_id: chain.environment.id,
        kind: chain.environment.kind,
    };
    Ok(Partner { chain, scope, op })
}

/// Absent → `None`; `null` → `Some(None)`; a value → `Some(Some(v))`.
fn nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

fn user_json(row: &EndUserRow) -> serde_json::Value {
    json!({ "user": EndUserView::from(row) })
}

fn no_store(status: StatusCode, body: serde_json::Value) -> Response {
    let mut r = (status, Json(body)).into_response();
    r.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    r
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBody {
    external_id: String,
    #[serde(default, deserialize_with = "nullable")]
    display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    image_url: Option<Option<String>>,
}

async fn create_user(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> AccessResult<Response> {
    let p = partner(&state, &caller, &headers).await?;
    p.require([names::PRINCIPALS_WRITE])?;
    let (row, created) = state
        .platform
        .upsert_end_user(
            &p.op,
            &p.scope,
            p.cap(&state),
            &body.external_id,
            ProfileInput {
                display_name: body.display_name,
                image_url: body.image_url,
            },
        )
        .await
        .map_err(user_err)?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(user_json(&row))).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListQuery {
    #[serde(default)]
    external_id: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
}

fn page_json(page: end_users::EndUserPage) -> serde_json::Value {
    json!({
        "items": page.items.iter().map(EndUserView::from).collect::<Vec<_>>(),
        "nextCursor": page.next_cursor,
    })
}

fn parse_cursor(cursor: Option<&str>) -> AccessResult<Option<Uuid>> {
    cursor
        .filter(|c| !c.is_empty())
        .map(|c| {
            Uuid::parse_str(c).map_err(|_| {
                AccessError::Operator(ErrorCode::ValidationError, "cursor is invalid".to_string())
            })
        })
        .transpose()
}

async fn list_users(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> AccessResult<impl IntoResponse> {
    let p = partner(&state, &caller, &headers).await?;
    p.require([names::PRINCIPALS_READ])?;
    let page = end_users::list(
        state.platform.db(),
        p.scope.tenant_id,
        p.scope.application_id,
        p.scope.environment_id,
        q.external_id.as_deref().filter(|v| !v.is_empty()),
        parse_cursor(q.cursor.as_deref())?,
        q.limit,
    )
    .await
    .map_err(AccessError::operator)?;
    Ok(Json(page_json(page)))
}

async fn scoped_user(state: &AppState, p: &Partner, id: Uuid) -> AccessResult<EndUserRow> {
    end_users::in_scope(state.platform.db(), &p.scope, id)
        .await
        .map_err(internal)?
        .ok_or(AccessError::Operator(
            ErrorCode::NotFound,
            "user not found".to_string(),
        ))
}

async fn get_user(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    let p = partner(&state, &caller, &headers).await?;
    p.require([names::PRINCIPALS_READ])?;
    Ok(Json(user_json(&scoped_user(&state, &p, user_id).await?)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PatchBody {
    #[serde(default, deserialize_with = "nullable")]
    display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    image_url: Option<Option<String>>,
    #[serde(default)]
    status: Option<PrincipalStatus>,
}

async fn update_user(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Json(body): Json<PatchBody>,
) -> AccessResult<impl IntoResponse> {
    let p = partner(&state, &caller, &headers).await?;
    p.require([names::PRINCIPALS_WRITE])?;
    let row = state
        .platform
        .update_end_user(
            &p.op,
            &p.scope,
            p.cap(&state),
            user_id,
            ProfileInput {
                display_name: body.display_name,
                image_url: body.image_url,
            },
            body.status,
        )
        .await
        .map_err(user_err)?;
    Ok(Json(user_json(&row)))
}

async fn delete_user(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> AccessResult<StatusCode> {
    let p = partner(&state, &caller, &headers).await?;
    p.require([names::PRINCIPALS_WRITE])?;
    state
        .platform
        .delete_end_user(&p.op, &p.scope, user_id)
        .await
        .map_err(user_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_sessions(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    let p = partner(&state, &caller, &headers).await?;
    p.require([names::PRINCIPALS_WRITE])?;
    let tokens_not_before = state
        .platform
        .revoke_end_user_sessions(&p.op, &p.scope, user_id)
        .await
        .map_err(user_err)?;
    Ok(Json(json!({ "tokensNotBefore": tokens_not_before })))
}

// ---------------------------------------------------------------------------
// User tokens
// ---------------------------------------------------------------------------

fn validated_grants(grants: Grants) -> AccessResult<Grants> {
    grants
        .validated()
        .map_err(|m| AccessError::Operator(ErrorCode::ValidationError, m))
}

fn ttl(ttl_seconds: Option<i64>) -> AccessResult<i64> {
    let ttl = ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS);
    if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl) {
        return Err(AccessError::Operator(
            ErrorCode::ValidationError,
            format!("ttlSeconds must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS}"),
        ));
    }
    Ok(ttl)
}

struct Minted {
    token: String,
    token_id: Uuid,
    expires_at: DateTime<Utc>,
}

/// Mints a token for the ACTIVE user `user` (rate-limited per user and hour).
async fn mint(
    state: &AppState,
    p: &Partner,
    user: &EndUserRow,
    grants: Grants,
    ttl: i64,
) -> AccessResult<Minted> {
    let keys = state.build.user_token_keys.as_ref().ok_or_else(disabled)?;
    if user.status != PrincipalStatus::Active {
        return Err(AccessError::Operator(
            ErrorCode::InvalidState,
            "tokens are minted only for ACTIVE users".to_string(),
        ));
    }
    let now = state.platform.clock().now();
    if !end_users::note_mint(state.platform.db(), user.id, now)
        .await
        .map_err(internal)?
    {
        crate::metrics::inc("platform_access_user_token_mint_rate_limited_total", &[]);
        return Err(AccessError::Refused(ErrorCode::RateLimited));
    }
    // A token minted right after `revoke-sessions` starts at the new epoch.
    let issued_at = user
        .tokens_not_before
        .map(|t| t.timestamp())
        .unwrap_or(i64::MIN)
        .max(now.timestamp());
    let token_id = Uuid::new_v4();
    let claims = UserTokenClaims {
        principal_id: user.id,
        tenant_id: p.scope.tenant_id,
        application_id: p.scope.application_id,
        environment_id: p.scope.environment_id,
        environment_kind: p.scope.kind,
        service_account_id: p.chain.service_account.id,
        credential_id: p.chain.credential.id,
        token_id,
        issued_at,
        not_before: issued_at,
        expires_at: issued_at + ttl,
        grants,
    };
    let token = keys.sign(&claims);
    if token.len() > MAX_TOKEN_BYTES {
        return Err(AccessError::Refused(ErrorCode::InternalError));
    }
    crate::metrics::inc(
        "platform_access_user_tokens_minted_total",
        &[("environment_kind", p.scope.kind.as_str())],
    );
    Ok(Minted {
        token,
        token_id,
        expires_at: DateTime::<Utc>::from_timestamp(claims.expires_at, 0)
            .ok_or(AccessError::Refused(ErrorCode::InternalError))?,
    })
}

fn minting_partner_enabled(state: &AppState) -> AccessResult<()> {
    if state.build.minting_enabled() {
        Ok(())
    } else {
        Err(disabled())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MintBody {
    #[serde(default)]
    grants: Grants,
    #[serde(default)]
    ttl_seconds: Option<i64>,
}

async fn mint_for_user(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Json(body): Json<MintBody>,
) -> AccessResult<Response> {
    minting_partner_enabled(&state)?;
    let p = partner(&state, &caller, &headers).await?;
    let grants = validated_grants(body.grants)?;
    let ttl = ttl(body.ttl_seconds)?;
    p.require(std::iter::once(names::USER_TOKENS_ISSUE).chain(grants.required_scopes()))?;
    let user = scoped_user(&state, &p, user_id).await?;
    let m = mint(&state, &p, &user, grants, ttl).await?;
    Ok(no_store(
        StatusCode::CREATED,
        json!({ "token": m.token, "tokenId": m.token_id, "expiresAt": m.expires_at }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpsertMintBody {
    external_id: String,
    #[serde(default, deserialize_with = "nullable")]
    display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    image_url: Option<Option<String>>,
    #[serde(default)]
    grants: Grants,
    #[serde(default)]
    ttl_seconds: Option<i64>,
}

async fn upsert_and_mint(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(body): Json<UpsertMintBody>,
) -> AccessResult<Response> {
    minting_partner_enabled(&state)?;
    let p = partner(&state, &caller, &headers).await?;
    let grants = validated_grants(body.grants)?;
    let ttl = ttl(body.ttl_seconds)?;
    p.require(
        [names::PRINCIPALS_WRITE, names::USER_TOKENS_ISSUE]
            .into_iter()
            .chain(grants.required_scopes()),
    )?;
    let (user, _) = state
        .platform
        .upsert_end_user(
            &p.op,
            &p.scope,
            p.cap(&state),
            &body.external_id,
            ProfileInput {
                display_name: body.display_name,
                image_url: body.image_url,
            },
        )
        .await
        .map_err(user_err)?;
    let m = mint(&state, &p, &user, grants, ttl).await?;
    Ok(no_store(
        StatusCode::CREATED,
        json!({
            "user": EndUserView::from(&user),
            "token": m.token,
            "tokenId": m.token_id,
            "expiresAt": m.expires_at,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Console read (human members, `platform-identity.delegate`)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsoleQuery {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
}

async fn console_list_users(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(environment_id): Path<Uuid>,
    Query(q): Query<ConsoleQuery>,
) -> AccessResult<impl IntoResponse> {
    if !state.build.end_users_enabled {
        return Err(disabled());
    }
    read_only(&caller)?;
    let environment = state
        .platform
        .environment(EnvironmentId::from_uuid(environment_id))
        .await
        .map_err(AccessError::operator)?;
    let page = end_users::list(
        state.platform.db(),
        environment.tenant_id.as_uuid(),
        environment.application_id.as_uuid(),
        environment.id.as_uuid(),
        None,
        parse_cursor(q.cursor.as_deref())?,
        q.limit,
    )
    .await
    .map_err(AccessError::operator)?;
    Ok(Json(page_json(page)))
}
