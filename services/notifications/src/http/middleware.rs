//! Internal service authentication and replay refusal (Document 4 §3.3).
//!
//! Every `/internal/v1/**` request (unknown paths included), except the I12 Alertmanager
//! receiver:
//! 1. carries a valid `X-CF-Service-Authorization` token for `cfmeet.notifications`, bound
//!    to the request (method + path), verified against the receiver-authoritative trust
//!    bundle (else 401);
//! 2. is the first use of that token: `(issuer, jti)` is recorded in
//!    `notif_service_nonces` (a replay inserts nothing and is refused 401).
//!
//! Each handler then requires its route permission ([`Caller::require`]). Spans carry the
//! matched route pattern, never the path; tokens are never logged.

use axum::{
    async_trait,
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{request::Parts, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{headers, InternalPermission, InternalServiceContext, RequestId};
use tracing::Instrument;

use crate::{
    db,
    error::{ApiError, ErrorCode, REQUEST_ID},
    state::AppState,
};

/// The I12 receiver authenticates with its own static bearer (Alertmanager cannot sign
/// service tokens).
pub const OPERATOR_ALERTS_PATH: &str = "/internal/v1/operator-alerts";

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
    /// Operator attribution (ADR 0067): present only for `cfmeet.admin-bridge` requests that
    /// name the HQ staff member (`X-CF-Operator`); attribution headers from any other caller
    /// are refused before the handler runs.
    pub operator: Option<cf_service_auth::OperatorAttribution>,
}

impl Caller {
    pub fn service(&self) -> &str {
        self.context.authenticated_service().as_str()
    }

    pub fn require(&self, permission: InternalPermission) -> Result<(), ApiError> {
        self.context.require_permission(permission).map_err(|denied| {
            tracing::warn!(caller = %denied.service, permission = denied.permission.as_str(), "internal permission denied");
            ApiError::new(ErrorCode::Forbidden)
        })
    }

    /// The admin bridge's operator reads: `permission` (bridge-only) and a named operator.
    pub fn require_operator(&self, permission: InternalPermission) -> Result<(), ApiError> {
        self.require(permission)?;
        if self.operator.is_none() {
            tracing::warn!(caller = %self.service(), "operator read without X-CF-Operator refused");
            return Err(ApiError::new(ErrorCode::Forbidden));
        }
        Ok(())
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

async fn first_use(state: &AppState, ctx: &InternalServiceContext) -> Result<bool, ApiError> {
    let inserted = db::exec(
        &state.db,
        "INSERT INTO notif_service_nonces (issuer, jti, expires_at) VALUES ($1, $2, to_timestamp($3::bigint) + interval '60 seconds')
         ON CONFLICT DO NOTHING",
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
    let path = request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|u| u.0.path().to_owned())
        .unwrap_or_else(|| request.uri().path().to_owned());
    if path == OPERATOR_ALERTS_PATH {
        let id = RequestId::generate().as_str().to_string();
        return REQUEST_ID.scope(id, next.run(request)).await;
    }
    let method = request.method().clone();
    let ctx = match state
        .verifier
        .verify_request(&method, &path, request.headers())
        .await
    {
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
    // Parsed before the async block: it borrows only the headers, synchronously.
    let attribution = cf_service_auth::OperatorAttribution::from_headers(
        ctx.authenticated_service(),
        request.headers(),
    );
    let outcome: Result<Caller, ApiError> = async {
        if !first_use(&state, &ctx).await? {
            tracing::warn!("replayed service token refused");
            return Err(ApiError::new(ErrorCode::InternalAuthenticationFailed));
        }
        let operator = attribution.map_err(|e| {
            tracing::warn!(reason = %e, "operator attribution refused");
            ApiError::new(ErrorCode::Forbidden)
        })?;
        Ok(Caller {
            context: ctx.clone(),
            operator,
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

/// Public probes and the provider callback: a generated request id for the envelope.
pub async fn request_context(request: Request, next: Next) -> Response {
    let id = RequestId::generate().as_str().to_string();
    REQUEST_ID.scope(id, next.run(request)).await
}

#[allow(dead_code)]
fn _permission_names() -> [InternalPermission; 7] {
    [
        InternalPermission::NotificationsEventPublish,
        InternalPermission::NotificationsUserForward,
        InternalPermission::NotificationsCallPushDispatch,
        InternalPermission::NotificationsPushRegistrationSync,
        InternalPermission::NotificationsObservabilityRead,
        InternalPermission::NotificationsOperationsReplay,
        InternalPermission::NotificationsOperationsReroute,
    ]
}
