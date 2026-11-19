//! CF Meet Apps — OAuth internal routes (ADR 0064; contract
//! `docs/platform/integrate-internal-contracts.md` §2). Behind
//! `PLATFORM_ACCESS_OAUTH_ENABLED`: off, every route answers exactly like an unknown route
//! (`404 NOT_FOUND`), before any body is read.
//!
//! | Route | Permission | Caller |
//! |---|---|---|
//! | `PUT /oauth/clients/{applicationId}` | `platform-oauth.manage` | Developer Console |
//! | `GET /oauth/clients/{applicationId}` | `platform-oauth.manage` | Developer Console |
//! | `POST /oauth/clients/{applicationId}/secret` | `platform-oauth.manage` | Developer Console |
//! | `GET /oauth/clients/{applicationId}/grants?cursor&limit` | `platform-oauth.manage` | Developer Console |
//! | `POST /oauth/authorization-requests/validate` | `platform-oauth.authorize` | Meeting Service |
//! | `POST /oauth/authorizations` | `platform-oauth.authorize` | Meeting Service |
//! | `GET /oauth/grants?accountId=…` / `?organizationId=…` | `platform-oauth.authorize` | Meeting Service |
//! | `DELETE /oauth/grants/{grantId}[?accountId=…|organizationId=…]` | `platform-oauth.authorize` | Meeting Service |
//! | `POST /oauth/token` | `platform-oauth.token` | Developer Console |
//! | `POST /oauth/revoke` | `platform-oauth.token` | Developer Console |
//! | `POST /oauth/introspect` | `platform-oauth.token` | Developer Console |
//!
//! USER authorizations target CF Meet's FIRST_PARTY personal communications environment:
//! the one named by the Meeting Service's signed delegated context (the same scope it
//! uses for `/principals/ensure`, re-validated here), or, without a context,
//! `PLATFORM_ACCESS_OAUTH_PERSONAL_ENVIRONMENT_ID`.

use std::collections::HashMap;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::json;
use uuid::Uuid;

use super::{
    error::{AccessError, AccessResult, ErrorCode},
    internal::trusted_scope,
    middleware::InternalCaller,
};
use crate::{
    identity::{
        authentication::{AuthenticationError, AuthorityMode},
        oauth::{
            self, revoke_reason, AuthorizationRequest, ClientInput, GrantFilter, OAuthError,
            PairwiseKey, TargetScope, TokenRequest,
        },
        oauth_scopes::Target,
    },
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/oauth/clients/:application_id",
            put(put_client).get(get_client),
        )
        .route("/oauth/clients/:application_id/secret", post(issue_secret))
        .route("/oauth/clients/:application_id/grants", get(app_grants))
        .route(
            "/oauth/authorization-requests/validate",
            post(validate_request),
        )
        .route("/oauth/authorizations", post(authorize))
        .route("/oauth/grants", get(list_grants))
        .route("/oauth/grants/:grant_id", delete(revoke_grant))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke_token))
        .route("/oauth/introspect", post(introspect))
}

fn gate(state: &AppState) -> AccessResult<()> {
    if state.build.oauth_enabled {
        Ok(())
    } else {
        Err(AccessError::Refused(ErrorCode::NotFound))
    }
}

fn body<T: DeserializeOwned>(bytes: &Bytes) -> AccessResult<T> {
    serde_json::from_slice(bytes).map_err(|_| AccessError::Refused(ErrorCode::ValidationError))
}

fn path_id(raw: &str) -> AccessResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| AccessError::Refused(ErrorCode::NotFound))
}

fn oauth_err(err: OAuthError) -> AccessError {
    match err {
        OAuthError::InvalidClient => AccessError::Refused(ErrorCode::InvalidClient),
        OAuthError::InvalidRedirectUri => AccessError::Refused(ErrorCode::InvalidRedirectUri),
        OAuthError::InvalidScope => AccessError::Refused(ErrorCode::InvalidScope),
        OAuthError::PkceRequired => AccessError::Refused(ErrorCode::PkceRequired),
        OAuthError::AppNotAvailable => AccessError::Refused(ErrorCode::AppNotAvailable),
        OAuthError::TokenInvalidClient => AccessError::Refused(ErrorCode::OauthInvalidClient),
        OAuthError::InvalidGrant => AccessError::Refused(ErrorCode::OauthInvalidGrant),
        OAuthError::UnsupportedGrantType => {
            AccessError::Refused(ErrorCode::OauthUnsupportedGrantType)
        }
        OAuthError::InvalidRequest => AccessError::Refused(ErrorCode::OauthInvalidRequest),
        OAuthError::Platform(e) => AccessError::operator(e),
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

// ---------------------------------------------------------------------------
// Client profile (`platform-oauth.manage`)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientBody {
    name: String,
    /// Optional override of the developer tenant's display name.
    #[serde(default)]
    developer_name: Option<String>,
    /// Optional https logo URL (≤ 512 characters).
    #[serde(default)]
    logo_url: Option<String>,
    /// Optional in-meeting surface.
    #[serde(default)]
    meeting_surface: Option<SurfaceBody>,
    redirect_uris: Vec<String>,
    #[serde(default)]
    user_scopes: Vec<String>,
    #[serde(default)]
    bot_scopes: Vec<String>,
    distribution: String,
    status: String,
    #[serde(default)]
    test_account_ids: Vec<Uuid>,
    /// PRIVATE distribution: the developer workspace members' accounts, synced by the
    /// Developer Console (Platform Access does not know workspace membership).
    #[serde(default)]
    allowed_account_ids: Vec<Uuid>,
    /// PRIVATE distribution: organizations owned by a workspace member, synced likewise.
    #[serde(default)]
    allowed_organization_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SurfaceBody {
    url: String,
    #[serde(default)]
    origins: Vec<String>,
}

async fn put_client(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(application_id): Path<String>,
    bytes: Bytes,
) -> AccessResult<Response> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthManage)?;
    let application_id = path_id(&application_id)?;
    let b: ClientBody = body(&bytes)?;
    let (client, created, revoked) = state
        .platform
        .put_oauth_client(
            application_id,
            ClientInput {
                name: b.name,
                developer_name: b.developer_name,
                logo_url: b.logo_url,
                meeting_surface: b.meeting_surface.map(|m| (m.url, m.origins)),
                redirect_uris: b.redirect_uris,
                user_scopes: b.user_scopes,
                bot_scopes: b.bot_scopes,
                distribution: b.distribution,
                status: b.status,
                test_account_ids: b.test_account_ids,
                allowed_account_ids: b.allowed_account_ids,
                allowed_organization_ids: b.allowed_organization_ids,
            },
        )
        .await
        .map_err(AccessError::operator)?;
    crate::metrics::inc(
        "platform_access_oauth_clients_set_total",
        &[("status", client.status.as_str())],
    );
    let mut view = client.to_json();
    view["revokedGrants"] = json!(revoked);
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(view)).into_response())
}

async fn get_client(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(application_id): Path<String>,
) -> AccessResult<impl IntoResponse> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthManage)?;
    let client = state
        .platform
        .oauth_client(path_id(&application_id)?)
        .await
        .map_err(AccessError::service)?
        .ok_or(AccessError::Refused(ErrorCode::NotFound))?;
    Ok(Json(client.to_json()))
}

async fn issue_secret(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(application_id): Path<String>,
) -> AccessResult<Response> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthManage)?;
    let (secret, secret_id, issued_at) = state
        .platform
        .issue_oauth_client_secret(path_id(&application_id)?)
        .await
        .map_err(AccessError::service)?;
    let response = (
        StatusCode::CREATED,
        Json(json!({
            "clientSecret": secret.expose_once(),
            "secretId": secret_id,
            "issuedAt": issued_at,
        })),
    )
        .into_response();
    Ok(no_store(response))
}

async fn app_grants(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(application_id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> AccessResult<impl IntoResponse> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthManage)?;
    let application_id = path_id(&application_id)?;
    if state
        .platform
        .oauth_client(application_id)
        .await
        .map_err(AccessError::service)?
        .is_none()
    {
        return Err(AccessError::Refused(ErrorCode::NotFound));
    }
    let cursor = match q.get("cursor").filter(|c| !c.is_empty()) {
        Some(c) => {
            Some(Uuid::parse_str(c).map_err(|_| AccessError::Refused(ErrorCode::ValidationError))?)
        }
        None => None,
    };
    let limit = match q.get("limit") {
        Some(l) => l
            .parse::<u64>()
            .ok()
            .filter(|n| (1..=200).contains(n))
            .ok_or(AccessError::Refused(ErrorCode::ValidationError))?,
        None => 50,
    };
    let (items, next) = state
        .platform
        .list_oauth_grants(GrantFilter::App(application_id), cursor, limit)
        .await
        .map_err(AccessError::service)?;
    let key = pairwise(&state);
    let items: Vec<_> = items.into_iter().map(|g| g.with_sub(&key)).collect();
    Ok(Json(json!({ "items": items, "nextCursor": next })))
}

// ---------------------------------------------------------------------------
// Authorization (`platform-oauth.authorize`, Meeting Service)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuthorizationBody {
    client_id: String,
    redirect_uri: String,
    scope: String,
    #[serde(default)]
    code_challenge: Option<String>,
    #[serde(default)]
    code_challenge_method: Option<String>,
    target: String,
    #[serde(default)]
    organization_id: Option<Uuid>,
    account_id: Uuid,
    /// `/authorizations` only: must be `APPROVE`.
    #[serde(default)]
    decision: Option<String>,
}

impl AuthorizationBody {
    fn request(&self) -> AuthorizationRequest {
        AuthorizationRequest {
            client_id: self.client_id.clone(),
            redirect_uri: self.redirect_uri.clone(),
            scope: self.scope.clone(),
            code_challenge: self.code_challenge.clone(),
            code_challenge_method: self.code_challenge_method.clone(),
            target: self.target.clone(),
            organization_id: self.organization_id,
            account_id: self.account_id,
        }
    }
}

async fn validate_request(
    State(state): State<AppState>,
    caller: InternalCaller,
    bytes: Bytes,
) -> AccessResult<impl IntoResponse> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthAuthorize)?;
    let b: AuthorizationBody = body(&bytes)?;
    if b.decision.is_some() {
        return Err(AccessError::Refused(ErrorCode::ValidationError));
    }
    let validated = state
        .platform
        .validate_authorization(&b.request())
        .await
        .map_err(oauth_err)?;
    Ok(Json(validated.to_json()))
}

/// The USER target: the caller's signed delegated scope (re-validated), else the
/// configured fallback environment.
async fn personal_target(state: &AppState, caller: &InternalCaller) -> AccessResult<TargetScope> {
    let environment_id = if caller.context().delegated_platform_context().is_some() {
        trusted_scope(state, caller).await?.environment.id
    } else {
        state.build.oauth_personal_environment_id.ok_or_else(|| {
            AccessError::Operator(
                ErrorCode::InvalidState,
                "no personal communications environment: send the signed delegated context of the first-party scope".to_string(),
            )
        })?
    };
    state
        .platform
        .oauth_personal_target(environment_id)
        .await
        .map_err(AccessError::operator)
}

async fn authorize(
    State(state): State<AppState>,
    caller: InternalCaller,
    bytes: Bytes,
) -> AccessResult<Response> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthAuthorize)?;
    let b: AuthorizationBody = body(&bytes)?;
    if b.decision.as_deref() != Some("APPROVE") {
        return Err(AccessError::Operator(
            ErrorCode::ValidationError,
            "decision must be APPROVE (a denial never reaches Platform Access)".to_string(),
        ));
    }
    let request = b.request();
    let personal = if oauth::parse_target(&request.target) == Some(Target::User) {
        // Validate first so a bad request is reported as such, not as a scope problem.
        state
            .platform
            .validate_authorization(&request)
            .await
            .map_err(oauth_err)?;
        Some(personal_target(&state, &caller).await?)
    } else {
        None
    };
    let user = personal.is_some();
    let (code, grant_id) = state
        .platform
        .authorize(&request, personal)
        .await
        .map_err(oauth_err)?;
    crate::metrics::inc(
        "platform_access_oauth_authorizations_total",
        &[("target", if user { "USER" } else { "ORGANIZATION" })],
    );
    let response = (
        StatusCode::CREATED,
        Json(json!({
            "code": code.expose_once(),
            "expiresIn": oauth::CODE_TTL_SECONDS,
            "grantId": grant_id,
        })),
    )
        .into_response();
    Ok(no_store(response))
}

fn owner_filter(q: &HashMap<String, String>) -> AccessResult<Option<GrantFilter>> {
    let parse = |v: &String| {
        Uuid::parse_str(v).map_err(|_| AccessError::Refused(ErrorCode::ValidationError))
    };
    match (q.get("accountId"), q.get("organizationId")) {
        (Some(a), None) => Ok(Some(GrantFilter::Account(parse(a)?))),
        (None, Some(o)) => Ok(Some(GrantFilter::Organization(parse(o)?))),
        (None, None) => Ok(None),
        _ => Err(AccessError::Refused(ErrorCode::ValidationError)),
    }
}

async fn list_grants(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<HashMap<String, String>>,
) -> AccessResult<impl IntoResponse> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthAuthorize)?;
    let filter = owner_filter(&q)?.ok_or(AccessError::Refused(ErrorCode::ValidationError))?;
    let (items, _) = state
        .platform
        .list_oauth_grants(filter, None, 200)
        .await
        .map_err(AccessError::service)?;
    let key = pairwise(&state);
    let items: Vec<_> = items.into_iter().map(|g| g.with_sub(&key)).collect();
    Ok(Json(json!({ "items": items })))
}

async fn revoke_grant(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(grant_id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> AccessResult<impl IntoResponse> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthAuthorize)?;
    let grant_id = path_id(&grant_id)?;
    let owner = owner_filter(&q)?;
    let grant = state
        .platform
        .oauth_grant(grant_id)
        .await
        .map_err(AccessError::service)?
        .ok_or(AccessError::Refused(ErrorCode::NotFound))?;
    // The optional owner scope: another owner's grant is reported like a missing one.
    let owned = match owner {
        None => true,
        Some(GrantFilter::Account(a)) => grant.account_id == Some(a),
        Some(GrantFilter::Organization(o)) => grant.organization_id == Some(o),
        Some(GrantFilter::App(_)) => false,
    };
    if !owned {
        return Err(AccessError::Refused(ErrorCode::NotFound));
    }
    let grant = state
        .platform
        .revoke_oauth_grant(grant_id, revoke_reason::UNINSTALLED)
        .await
        .map_err(AccessError::service)?
        .ok_or(AccessError::Refused(ErrorCode::NotFound))?
        .with_sub(&pairwise(&state));
    Ok(Json(json!({ "grant": grant })))
}

// ---------------------------------------------------------------------------
// Token endpoint backend (`platform-oauth.token`, Developer Console)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TokenBody {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    grant_type: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

async fn token(
    State(state): State<AppState>,
    caller: InternalCaller,
    bytes: Bytes,
) -> AccessResult<Response> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthToken)?;
    let b: TokenBody = serde_json::from_slice(&bytes)
        .map_err(|_| AccessError::Refused(ErrorCode::OauthInvalidRequest))?;
    let issued = state
        .platform
        .oauth_token(TokenRequest {
            client_id: b.client_id,
            client_secret: b.client_secret,
            grant_type: b.grant_type,
            code: b.code,
            redirect_uri: b.redirect_uri,
            code_verifier: b.code_verifier,
            refresh_token: b.refresh_token,
        })
        .await
        .map_err(|e| {
            let err = oauth_err(e);
            crate::metrics::inc(
                "platform_access_oauth_token_refused_total",
                &[("code", err.code().as_str())],
            );
            err
        })?;
    let mut grant = json!({ "id": issued.grant.id, "target": issued.grant.target });
    if let Some(org) = issued.grant.organization_id {
        grant["organizationId"] = json!(org);
    }
    let mut out = json!({
        "accessToken": issued.access.expose_once(),
        "tokenType": "Bearer",
        "expiresIn": oauth::ACCESS_TTL_SECONDS,
        "refreshToken": issued.refresh.expose_once(),
        "scope": issued.grant.scopes.join(" "),
        "grant": grant,
    });
    if let Some(bot) = &issued.bot {
        out["botToken"] = json!(bot.expose_once());
    }
    Ok(no_store(Json(out).into_response()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevokeBody {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    client_secret: String,
    #[serde(default)]
    token: String,
}

/// RFC 7009: always `200` (an unknown, foreign or malformed token changes nothing).
async fn revoke_token(
    State(state): State<AppState>,
    caller: InternalCaller,
    bytes: Bytes,
) -> AccessResult<impl IntoResponse> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthToken)?;
    if let Ok(b) = serde_json::from_slice::<RevokeBody>(&bytes) {
        if b.token.len() <= 256 {
            state
                .platform
                .revoke_oauth_token(&b.client_id, &b.client_secret, b.token.trim())
                .await
                .map_err(AccessError::service)?;
        }
    }
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntrospectBody {
    token: String,
}

fn pairwise(state: &AppState) -> std::sync::Arc<PairwiseKey> {
    state
        .build
        .oauth_pairwise_key
        .clone()
        .unwrap_or_else(|| std::sync::Arc::new(PairwiseKey::derived(state.platform.keyring())))
}

async fn introspect(
    State(state): State<AppState>,
    caller: InternalCaller,
    bytes: Bytes,
) -> AccessResult<impl IntoResponse> {
    gate(&state)?;
    caller.require_permission(InternalPermission::PlatformOauthToken)?;
    let b: IntrospectBody = body(&bytes)?;
    let inactive = || Json(json!({ "active": false }));
    let presented = b.token.trim();
    if presented.len() > 256 {
        return Ok(inactive());
    }
    let chain = match state.authenticator.authenticate_oauth(presented).await {
        Ok(chain) if chain.authority_mode == AuthorityMode::Full => chain,
        Ok(_) | Err(AuthenticationError::Rejected { .. }) => return Ok(inactive()),
        Err(AuthenticationError::Unavailable { summary }) => {
            tracing::error!(%summary, "credential authority unavailable");
            return Err(AccessError::Refused(ErrorCode::ServiceUnavailable));
        }
    };
    let Some(grant_id) = chain.grant_id else {
        return Ok(inactive());
    };
    let Some(grant) = state
        .platform
        .oauth_grant(grant_id)
        .await
        .map_err(AccessError::service)?
        .filter(|g| g.active())
    else {
        return Ok(inactive());
    };
    let mut out = json!({
        "active": true,
        "grantId": grant.id,
        "appId": grant.app_id,
        "target": grant.target,
        "principalId": grant.principal_id,
        "scopes": grant.scopes,
        "sub": pairwise(&state).sub(grant.app_id, &oauth::pairwise_subject(&grant)),
        "credentialKind": chain.credential_kind.map(|k| k.as_str()),
        "expiresAt": chain.credential.expires_at,
    });
    if let Some(a) = grant.account_id {
        out["accountId"] = json!(a);
    }
    if let Some(o) = grant.organization_id {
        out["organizationId"] = json!(o);
    }
    Ok(Json(out))
}
