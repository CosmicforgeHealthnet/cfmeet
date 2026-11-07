//! Internal service authentication, replay refusal and actor propagation (contract §1).
//!
//! Every `/internal/v1/**` request (unknown paths included):
//! 1. carries a valid `X-CF-Service-Authorization` token for `cfmeet.organizations`,
//!    verified against the receiver-authoritative trust bundle (else 401);
//! 2. is the first use of that token: its `(issuer, jti)` is recorded in
//!    `organization_service_nonces` (`INSERT … ON CONFLICT DO NOTHING`; a replay inserts
//!    nothing and is refused 401);
//! 3. may name the acting user (`X-CF-Acting-User`, `X-CF-Acting-User-Verified-Email`) only
//!    if the caller holds `organization.act-for-user` (else 403 `ACTOR_NOT_ALLOWED`).
//!
//! Each handler then requires its route permission ([`Caller::require`]). Spans carry the
//! matched route pattern, never the path; tokens and emails are never logged.

use axum::{
    async_trait,
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{request::Parts, HeaderMap, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{headers, InternalPermission, InternalServiceContext, RequestId};
use tracing::Instrument;
use uuid::Uuid;

use crate::{
    db,
    error::{ApiError, ErrorCode, REQUEST_ID},
    state::AppState,
    store::{ActorKind, OpContext},
    validation::normalize_email,
};

pub const ACTING_USER: &str = "x-cf-acting-user";
pub const ACTING_USER_VERIFIED_EMAIL: &str = "x-cf-acting-user-verified-email";

fn route_pattern(request: &Request) -> String {
    request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "<unmatched>".into())
}

/// The authenticated internal caller of the current request.
#[derive(Debug, Clone)]
pub struct Caller {
    pub context: InternalServiceContext,
    /// `X-CF-Acting-User` (only ever set for callers holding `organization.act-for-user`).
    pub acting_user: Option<Uuid>,
    /// `X-CF-Acting-User-Verified-Email`, normalized (same rule).
    pub verified_email: Option<String>,
}

impl Caller {
    pub fn service(&self) -> &str {
        self.context.authenticated_service().as_str()
    }

    pub fn has(&self, permission: InternalPermission) -> bool {
        self.context.has_permission(permission)
    }

    /// Fails with INTERNAL_PERMISSION_DENIED unless the trust bundle grants `permission`.
    pub fn require(&self, permission: InternalPermission) -> Result<(), ApiError> {
        self.context
            .require_permission(permission)
            .map_err(|denied| {
                tracing::warn!(caller = %denied.service, permission = denied.permission.as_str(),
                "internal permission denied");
                ApiError::new(ErrorCode::InternalPermissionDenied)
            })
    }

    /// The acting user, required by operations performed for a human.
    pub fn user(&self) -> Result<Uuid, ApiError> {
        self.acting_user
            .ok_or_else(|| ApiError::validation("X-CF-Acting-User is required for this operation"))
    }

    fn base(&self, kind: ActorKind) -> OpContext {
        OpContext {
            kind,
            user_id: self.acting_user,
            service: self.service().to_string(),
            request_id: Some(self.context.request_id().as_str().to_string()),
            correlation_id: Some(self.context.correlation_id().as_str().to_string()),
        }
    }

    /// The actor of a change performed for the acting user.
    pub fn as_user(&self) -> OpContext {
        self.base(ActorKind::User)
    }

    /// The actor of an operator change (the acting platform administrator, if relayed).
    pub fn as_operator(&self) -> OpContext {
        self.base(ActorKind::Operator)
    }
}

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Caller>()
            .cloned()
            .ok_or(ApiError::new(ErrorCode::InternalAuthenticationFailed))
    }
}

fn actor_headers(headers: &HeaderMap) -> Result<(Option<Uuid>, Option<String>, bool), ApiError> {
    let user = headers.get(ACTING_USER);
    let email = headers.get(ACTING_USER_VERIFIED_EMAIL);
    let present = user.is_some() || email.is_some();
    let user = match user {
        None => None,
        Some(v) => Some(
            v.to_str()
                .ok()
                .and_then(|s| Uuid::try_parse(s.trim()).ok())
                .ok_or_else(|| ApiError::validation("X-CF-Acting-User must be a UUID"))?,
        ),
    };
    let email = match email {
        None => None,
        Some(v) => Some(v.to_str().ok().and_then(normalize_email).ok_or_else(|| {
            ApiError::validation("X-CF-Acting-User-Verified-Email must be an email address")
        })?),
    };
    Ok((user, email, present))
}

/// Records the token's first use; `false` for a replay.
async fn first_use(state: &AppState, ctx: &InternalServiceContext) -> Result<bool, ApiError> {
    let inserted = db::exec(
        &state.db,
        "INSERT INTO organization_service_nonces (issuer, token_id, expires_at) VALUES ($1, $2, to_timestamp($3::bigint) + interval '60 seconds') ON CONFLICT DO NOTHING",
        vec![
            ctx.authenticated_service().as_str().into(),
            ctx.token_id().as_str().into(),
            ctx.expires_at().into(),
        ],
    )
    .await?;
    Ok(inserted == 1)
}

pub async fn require_internal_service(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let route = route_pattern(&request);
    let ctx = match state.verifier.verify_headers(request.headers()) {
        Ok(ctx) => ctx,
        Err(error) => {
            let d = &error.diagnostics;
            tracing::warn!(
                reason = error.failure.as_str(),
                kid = d.kid.as_ref().map(|k| k.as_str()).unwrap_or(""),
                issuer = d.issuer.as_ref().map(|i| i.as_str()).unwrap_or(""),
                route = %route,
                "internal service authentication rejected"
            );
            return REQUEST_ID
                .scope(RequestId::generate().as_str().to_string(), async {
                    ApiError::new(ErrorCode::InternalAuthenticationFailed).into_response()
                })
                .await;
        }
    };
    let request_id = ctx.request_id().as_str().to_string();
    let span = tracing::info_span!("internal_request", request_id = %request_id,
        correlation_id = %ctx.correlation_id(), caller = %ctx.authenticated_service(),
        method = %request.method(), route = %route);
    // Parsed before any await: the request body is not `Sync`.
    let actor = actor_headers(request.headers());
    let outcome: Result<Caller, ApiError> = async {
        if !first_use(&state, &ctx).await? {
            tracing::warn!("replayed service token refused");
            return Err(ApiError::new(ErrorCode::InternalAuthenticationFailed));
        }
        let (acting_user, verified_email, present) = actor?;
        if present && !ctx.has_permission(InternalPermission::OrganizationActForUser) {
            tracing::warn!(
                "acting-user header from a caller without organization.act-for-user refused"
            );
            return Err(ApiError::new(ErrorCode::ActorNotAllowed));
        }
        Ok(Caller {
            context: ctx.clone(),
            acting_user,
            verified_email,
        })
    }
    .instrument(span.clone())
    .await;
    REQUEST_ID
        .scope(request_id.clone(), async move {
            let mut response = match outcome {
                Ok(caller) => {
                    request.extensions_mut().insert(caller);
                    next.run(request).instrument(span).await
                }
                Err(e) => e.into_response(),
            };
            if let Ok(value) = HeaderValue::from_str(&request_id) {
                response.headers_mut().insert(headers::REQUEST_ID, value);
            }
            response
        })
        .await
}

/// Public probes: a generated request id for the error envelope.
pub async fn request_context(request: Request, next: Next) -> Response {
    let id = RequestId::generate().as_str().to_string();
    REQUEST_ID.scope(id, next.run(request)).await
}
