//! Platform credential authentication middleware.
//!
//! Accepts only `Authorization: Bearer <credential>`. A request that also carries an
//! internal service token (`X-CF-Service-Authorization`) is rejected: customer routes
//! never accept service credentials and the two mechanisms never fall through into each
//! other. On success a
//! [`PlatformAuthContext`] built from persisted state is inserted into the request
//! extensions (handlers extract it), the request-rate limit is enforced, and the
//! platform identifiers are recorded on the request span. On failure the client receives
//! the normalized `AUTHENTICATION_FAILED` response; the specific reason is logged for
//! operators together with the non-secret public credential id. The raw header value is
//! never logged.

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap},
    middleware::Next,
    response::{IntoResponse, Response},
};
use zeroize::Zeroizing;

use crate::{
    domain::RequestContext,
    error::CallingError,
    platform::{
        authentication::{AuthFailureReason, AuthenticationError},
        error::summarize_db_error,
        limits::RateDecision,
    },
    state::AppState,
};

/// Header values longer than this are rejected without inspection.
const MAX_AUTHORIZATION_LEN: usize = 256;

fn bearer_credential(headers: &HeaderMap) -> Option<Zeroizing<String>> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || value.len() > MAX_AUTHORIZATION_LEN {
        return None;
    }
    let value = value.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let credential = credential.trim();
    (!credential.is_empty()).then(|| Zeroizing::new(credential.to_string()))
}

fn reject(reason: AuthFailureReason, public_id: Option<&str>) -> Response {
    tracing::info!(
        reason = reason.as_str(),
        public_id = public_id.unwrap_or(""),
        "platform authentication rejected"
    );
    CallingError::AuthenticationFailed.into_response()
}

pub async fn require_platform_credential(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_context = request
        .extensions()
        .get::<RequestContext>()
        .cloned()
        .unwrap_or_else(RequestContext::generate);
    if request
        .headers()
        .contains_key(&cf_service_auth::headers::SERVICE_AUTHORIZATION)
    {
        return reject(AuthFailureReason::ServiceCredentialPresented, None);
    }
    let Some(credential) = bearer_credential(request.headers()) else {
        return reject(AuthFailureReason::MissingCredential, None);
    };

    let ctx = match state
        .authenticator
        .authenticate(&credential, request_context)
        .await
    {
        Ok(ctx) => ctx,
        Err(AuthenticationError::Rejected { reason, public_id }) => {
            return reject(reason, public_id.as_deref());
        }
        Err(AuthenticationError::Unavailable { summary }) => {
            tracing::error!(%summary, "credential authority unavailable; request refused");
            return CallingError::ServiceUnavailable.into_response();
        }
    };
    drop(credential);

    let span = tracing::Span::current();
    span.record("tenant_id", tracing::field::display(ctx.tenant_id()));
    span.record(
        "application_id",
        tracing::field::display(ctx.application_id()),
    );
    span.record(
        "environment_id",
        tracing::field::display(ctx.environment_id()),
    );
    span.record(
        "service_account_id",
        tracing::field::display(ctx.service_account_id()),
    );
    span.record(
        "credential_id",
        tracing::field::display(ctx.credential_id()),
    );

    match state.rate_limiter.check(&ctx).await {
        Ok(RateDecision::Allowed) => {}
        Ok(RateDecision::Limited { retry_after_secs }) => {
            return CallingError::RateLimited { retry_after_secs }.into_response();
        }
        Err(err) => {
            tracing::error!(summary = %summarize_db_error(&err), "rate limiter unavailable; request refused");
            return CallingError::ServiceUnavailable.into_response();
        }
    }

    request.extensions_mut().insert(ctx);
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for v in values {
            map.append(header::AUTHORIZATION, HeaderValue::from_str(v).unwrap());
        }
        map
    }

    #[test]
    fn extracts_bearer_credentials_only() {
        assert_eq!(
            bearer_credential(&headers(&["Bearer abc"]))
                .as_deref()
                .map(String::as_str),
            Some("abc")
        );
        assert_eq!(
            bearer_credential(&headers(&["bearer   abc  "]))
                .as_deref()
                .map(String::as_str),
            Some("abc")
        );
        assert!(bearer_credential(&headers(&[])).is_none());
        assert!(bearer_credential(&headers(&["Basic abc"])).is_none());
        assert!(bearer_credential(&headers(&["Bearer"])).is_none());
        assert!(bearer_credential(&headers(&["Bearer "])).is_none());
        assert!(bearer_credential(&headers(&["Bearer a", "Bearer b"])).is_none());
        let long = format!("Bearer {}", "a".repeat(MAX_AUTHORIZATION_LEN));
        assert!(bearer_credential(&headers(&[&long])).is_none());
    }
}
