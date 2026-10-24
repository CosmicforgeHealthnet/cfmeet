//! HTTP surface (contract `cfmeet.api-gateway.http-api`).
//!
//! * `GET /health`, `GET /ready` — probes (flags, route count, quota mode, configured
//!   upstreams and their reachability);
//! * `GET /openapi.json`, `GET /openapi/{domain}.json`, `GET /docs`, `GET /discovery` —
//!   public documentation and discovery ([`crate::docs`]);
//! * everything else — the customer ingress ([`ingress`]): default deny, authentication,
//!   quotas and forwarding to the owning service.
//!
//! Cross-cutting: request / correlation ids on every response, header count / size and
//! URI length limits, and CORS for the configured exact origins only. There is no
//! `/internal` surface, no provider callback and no operator route.

pub mod error;
pub mod ingress;

use std::time::Duration;

use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{from_fn, from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use self::error::{ApiError, ErrorCode};
use crate::{
    context::{scoped, RequestContext},
    state::AppState,
};

pub const CORS_ALLOWED_HEADERS: &str =
    "authorization, content-type, accept, idempotency-key, if-match, if-none-match, cf-acting-principal, x-participant-token, x-client-kind";
pub const CORS_EXPOSED_HEADERS: &str = "x-request-id, x-correlation-id, retry-after, idempotent-replayed, etag, location, x-cf-quota-limit-minute, x-cf-quota-remaining-minute, x-cf-quota-reset, x-cf-quota-limit-day, x-cf-quota-window, x-cf-quota-level";

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/openapi.json", get(crate::docs::aggregate))
        .route("/openapi/:file", get(crate::docs::domain_doc))
        .route("/docs", get(crate::docs::reference))
        .route("/discovery", get(crate::docs::discovery))
        .layer(from_fn(probe_context))
        .fallback(ingress::ingress)
        .layer(from_fn_with_state(state.clone(), cors))
        .layer(from_fn_with_state(state.clone(), limits))
        .with_state(state)
}

async fn probe_context(request: Request, next: Next) -> Response {
    let context = RequestContext::generate();
    let response = scoped(context.clone(), next.run(request)).await;
    ingress::with_ids(response, &context)
}

/// Header count / size and URI length limits, before anything else.
async fn limits(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let fail = |code| {
        let context = RequestContext::generate();
        let r = ApiError::Code(code).into_response();
        ingress::with_ids(r, &context)
    };
    if request.uri().to_string().len() > 4096 {
        return fail(ErrorCode::UriTooLong);
    }
    let headers = request.headers();
    let bytes: usize = headers
        .iter()
        .map(|(k, v)| k.as_str().len() + v.len())
        .sum();
    if headers.len() > state.max_headers || bytes > state.max_header_bytes {
        return fail(ErrorCode::HeadersTooLarge);
    }
    next.run(request).await
}

/// CORS for the configured exact origins (no wildcard, no credentials mode).
async fn cors(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let Some(origin) = origin else {
        return next.run(request).await;
    };
    let allowed = state.cors_allowed_origins.contains(&origin);
    let preflight = request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    if preflight {
        let context = RequestContext::generate();
        if !allowed {
            return ingress::with_ids(
                ApiError::Code(ErrorCode::OriginNotAllowed).into_response(),
                &context,
            );
        }
        let path = request.uri().path().to_string();
        let mut methods = state.routes.path_exists(&path);
        if matches!(path.as_str(), "/openapi.json" | "/discovery") || path.starts_with("/openapi/")
        {
            methods.push(Method::GET);
        }
        if methods.is_empty() {
            return ingress::with_ids(
                ApiError::Code(ErrorCode::NotFound).into_response(),
                &context,
            );
        }
        let allow = methods
            .iter()
            .map(Method::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let mut r = StatusCode::NO_CONTENT.into_response();
        let h = r.headers_mut();
        h.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(&origin).unwrap_or(HeaderValue::from_static("null")),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_str(&allow).unwrap_or(HeaderValue::from_static("GET")),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(CORS_ALLOWED_HEADERS),
        );
        h.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
        h.insert(header::VARY, HeaderValue::from_static("origin"));
        return ingress::with_ids(r, &context);
    }
    let mut response = next.run(request).await;
    if allowed {
        let h = response.headers_mut();
        if let Ok(v) = HeaderValue::from_str(&origin) {
            h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        }
        h.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static(CORS_EXPOSED_HEADERS),
        );
        h.append(header::VARY, HeaderValue::from_static("origin"));
    }
    response
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "api-gateway" }))
}

/// Readiness and service discovery: the gateway is ready to serve when its catalogue is
/// loaded; each upstream family reports `configured` / `reachable` / `unreachable` (a
/// domain outage never reroutes traffic to another service).
async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let mut upstreams = serde_json::Map::new();
    let mut needed: Vec<String> = state
        .routes
        .definitions()
        .map(|d| d.upstream.clone())
        .collect();
    needed.sort();
    needed.dedup();
    let probes = needed.iter().map(|service| {
        let state = state.clone();
        let service = service.clone();
        async move {
            let id = cf_service_auth::ServiceId::parse(&service).ok();
            let status = match id
                .as_ref()
                .and_then(|id| state.service_auth.upstream_base(id))
            {
                None => "not_configured",
                Some(base) => {
                    let url = format!("{}/health", base.as_str().trim_end_matches('/'));
                    match state
                        .http
                        .get(url)
                        .timeout(Duration::from_secs(1))
                        .send()
                        .await
                    {
                        Ok(r) if r.status().is_success() => "reachable",
                        _ => "unreachable",
                    }
                }
            };
            (service, status)
        }
    });
    for (service, status) in futures_util::future::join_all(probes).await {
        upstreams.insert(service, Value::from(status));
    }
    Json(json!({
        "status": "ready",
        "service": "api-gateway",
        "publicRouting": if state.flags.public_routing { "enabled" } else { "disabled" },
        "routes": state.routes.len(),
        "websockets": if state.flags.websockets { "enabled" } else { "disabled" },
        "quotas": state.quota.as_ref().map(|q| q.mode()).unwrap_or("not_enforced"),
        "centralWebhooks": state.controls.central_webhooks,
        "centralUsage": state.controls.central_usage,
        "disabledFamilies": state.controls.disabled_domains.iter().collect::<Vec<_>>(),
        "upstreams": upstreams,
    }))
}
