//! Public Privacy Relay API ingress (contract `cfmeet.privacy-relay.http-api`,
//! `packages/contracts/privacy-relay/http/v1.openapi.json`).
//!
//! Every handler is THIN — it contains no Privacy Relay business logic:
//!
//! 1. platform credential authentication (route layer) and the public scope check;
//! 2. request shape at the edge: path ids are UUIDs, bodies are bounded JSON objects,
//!    `Idempotency-Key` is required on every mutation, query parameters are a closed set;
//! 3. principals named by participants / directions must be ACTIVE principals of the
//!    caller's own application environment (principal identity is Calling's domain);
//! 4. the safe signed [`DelegatedPlatformContext`](cf_service_auth::DelegatedPlatformContext)
//!    is derived from the authenticated credential — the customer credential itself is
//!    never forwarded;
//! 5. the request is forwarded to Privacy Relay over authenticated internal HTTP and its
//!    response / error envelope relayed with this request's id.
//!
//! Privacy Relay remains the business and data authority (and itself refuses scope,
//! status or result fields in bodies).

use std::collections::BTreeSet;

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{PrivacyRelayError, ProxyRequest};
use crate::{
    calls::service::IdempotencyKey,
    domain::PrincipalId,
    error::CallingError,
    internal_auth::delegation::delegated_context,
    middleware::request_id::current_request_id,
    platform::{
        context::PlatformAuthContext,
        principals::{PrincipalReference, PrincipalResolutionError},
        scopes::names,
    },
    state::AppState,
};

const MAX_BODY_BYTES: usize = 64 * 1024;
const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

/// Public error codes Privacy Relay may answer with (relayed as-is). Internal codes are
/// never relayed: they mean a platform misconfiguration and become SERVICE_UNAVAILABLE.
pub const RELAYED_ERROR_CODES: &[&str] = &[
    "VALIDATION_ERROR",
    "NOT_FOUND",
    "CONTEXT_NOT_FOUND",
    "SESSION_NOT_FOUND",
    "CONTACT_NOT_FOUND",
    "CONTEXT_VERSION_CONFLICT",
    "SESSION_VERSION_CONFLICT",
    "CONTEXT_REFERENCE_CONFLICT",
    "CONTEXT_NOT_ACTIVE",
    "CONTEXT_EXPIRED",
    "CONTEXT_REVOKED",
    "CONTEXT_SUSPENDED",
    "SESSION_NOT_ACTIVE",
    "PARTICIPANT_NOT_AUTHORIZED",
    "DIRECTION_NOT_ALLOWED",
    "CHANNEL_NOT_ALLOWED",
    "STRATEGY_NOT_ALLOWED",
    "VALIDITY_NOT_CONTAINED",
    "IDEMPOTENCY_CONFLICT",
    "PRIVACY_RELAY_DISABLED",
    "SERVICE_UNAVAILABLE",
];

/// Customer routes of the Privacy Relay API (mounted behind platform authentication).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/privacy-relay/contexts",
            post(create_context).get(list_contexts),
        )
        .route(
            "/v1/privacy-relay/contexts/:context_id",
            get(get_context).patch(update_context),
        )
        .route(
            "/v1/privacy-relay/contexts/:context_id/evidence",
            get(context_evidence),
        )
        .route(
            "/v1/privacy-relay/sessions/:relay_session_id/evidence",
            get(super::evidence::session_evidence),
        )
        .route(
            "/v1/privacy-relay/contexts/:context_id/suspend",
            post(suspend_context),
        )
        .route(
            "/v1/privacy-relay/contexts/:context_id/resume",
            post(resume_context),
        )
        .route(
            "/v1/privacy-relay/contexts/:context_id/complete",
            post(complete_context),
        )
        .route(
            "/v1/privacy-relay/contexts/:context_id/revoke",
            post(revoke_context),
        )
        .route(
            "/v1/privacy-relay/contexts/:context_id/sessions",
            post(create_session),
        )
        .route("/v1/privacy-relay/sessions", get(list_sessions))
        .route(
            "/v1/privacy-relay/sessions/:relay_session_id",
            get(get_session),
        )
        .route(
            "/v1/privacy-relay/sessions/:relay_session_id/suspend",
            post(suspend_session),
        )
        .route(
            "/v1/privacy-relay/sessions/:relay_session_id/resume",
            post(resume_session),
        )
        .route(
            "/v1/privacy-relay/sessions/:relay_session_id/revoke",
            post(revoke_session),
        )
        // Contact Vault delegated access (§31): Calling mints the short-lived token; the
        // raw number itself goes straight to Privacy Relay, never through this ingress.
        .route(
            "/v1/privacy-relay/contact-access-grants",
            post(super::contacts::create_contact_access_grant),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// Closed set of list query parameters.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_context_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_id: Option<Uuid>,
}

fn relay_error(status: StatusCode, code: &str, message: &str) -> Response {
    let mut response = (
        status,
        Json(json!({
            "error": {
                "code": code,
                "message": message,
                "requestId": current_request_id().map(|id| id.as_str().to_string()),
            }
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Privacy Relay's answer with this request's id; unknown / internal codes and
/// unsafe messages never reach the customer.
fn relay_response(status: u16, body: Value, replayed: bool) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    if status.is_success() {
        let mut response = (status, Json(body)).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if replayed {
            headers.insert("idempotent-replayed", HeaderValue::from_static("true"));
        }
        return response;
    }
    let code = body
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !RELAYED_ERROR_CODES.contains(&code) {
        tracing::error!(
            status = status.as_u16(),
            code,
            "privacy relay refused the platform ingress"
        );
        return CallingError::ServiceUnavailable.into_response();
    }
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .filter(|m| m.len() <= 300 && m.chars().all(|c| !c.is_control()))
        .unwrap_or("The request could not be completed.");
    relay_error(status, code, message)
}

/// A path id: a UUID, or the resource's NOT_FOUND code.
fn uuid_path(raw: &str, code: &'static str) -> Result<Uuid, &'static str> {
    Uuid::parse_str(raw).map_err(|_| code)
}

fn not_found(code: &str) -> Response {
    relay_error(
        StatusCode::NOT_FOUND,
        code,
        "The requested resource was not found.",
    )
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, CallingError> {
    let mut values = headers.get_all(IDEMPOTENCY_KEY_HEADER).iter();
    let value = values.next().ok_or(CallingError::Validation(
        "The Idempotency-Key header is required.",
    ))?;
    if values.next().is_some() {
        return Err(CallingError::Validation(
            "Idempotency-Key must be sent once.",
        ));
    }
    value
        .to_str()
        .ok()
        .and_then(|v| IdempotencyKey::parse(v).ok())
        .map(|k| k.as_str().to_string())
        .ok_or(CallingError::Validation(
            "Idempotency-Key must be 1-255 printable ASCII characters without spaces.",
        ))
}

fn object_body(body: Result<Json<Value>, JsonRejection>) -> Result<Value, CallingError> {
    match body {
        Ok(Json(v)) if v.is_object() => Ok(v),
        _ => Err(CallingError::Validation(
            "The request body must be a JSON object.",
        )),
    }
}

/// Principal ids named as participants / direction endpoints (shape only; Privacy Relay
/// validates everything else).
fn named_principals(body: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut take = |v: Option<&Value>| {
        if let Some(id) = v.and_then(|s| s.get("principalId")).and_then(Value::as_str) {
            out.insert(id.to_string());
        }
    };
    for p in body
        .get("participants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        take(Some(p));
    }
    for d in body
        .get("directions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        take(d.get("from"));
        take(d.get("to"));
    }
    out
}

/// Referenced principals must be ACTIVE principals of the caller's environment.
async fn validate_principals(
    state: &AppState,
    ctx: &PlatformAuthContext,
    body: &Value,
) -> Result<(), CallingError> {
    let names = named_principals(body);
    if names.len() > 64 {
        return Err(CallingError::Validation(
            "Too many principals are referenced.",
        ));
    }
    for id in names {
        // Malformed ids are left to Privacy Relay's validation (same 400).
        let Ok(uuid) = Uuid::parse_str(&id) else {
            continue;
        };
        match state
            .principals
            .resolve(ctx, &PrincipalReference::Id(PrincipalId::from_uuid(uuid)))
            .await
        {
            Ok(_) => {}
            Err(
                PrincipalResolutionError::NotFound | PrincipalResolutionError::InvalidReference,
            ) => {
                return Err(CallingError::Validation(
                    "A referenced principal is not a principal of this application environment.",
                ))
            }
            Err(PrincipalResolutionError::Inactive) => {
                return Err(CallingError::Validation(
                    "A referenced principal is not active.",
                ))
            }
            Err(PrincipalResolutionError::Unavailable { .. }) => {
                return Err(CallingError::ServiceUnavailable)
            }
        }
    }
    Ok(())
}

/// Forwards one request to Privacy Relay for the authenticated application.
async fn forward(
    state: &AppState,
    ctx: &PlatformAuthContext,
    method: Method,
    path: String,
    body: Option<Value>,
) -> Response {
    let delegated = match delegated_context(ctx, None) {
        Ok(d) => d,
        Err(_) => return CallingError::Internal.into_response(),
    };
    match state
        .privacy_relay
        .proxy(ProxyRequest {
            delegated,
            correlation_id: ctx.correlation_id().clone(),
            method,
            path,
            body,
        })
        .await
    {
        Ok(r) => relay_response(r.status, r.body, r.replayed),
        Err(PrivacyRelayError::NotFound | PrivacyRelayError::Unavailable) => {
            CallingError::ServiceUnavailable.into_response()
        }
    }
}

/// A command: scope, Idempotency-Key, JSON body, principal validation, forward.
async fn command(
    state: AppState,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    scope: &str,
    path: String,
    body: Result<Json<Value>, JsonRejection>,
    principals: bool,
) -> Result<Response, CallingError> {
    ctx.require_scope(scope)?;
    let key = idempotency_key(&headers)?;
    let request = object_body(body)?;
    if principals {
        validate_principals(&state, &ctx, &request).await?;
    }
    Ok(forward(
        &state,
        &ctx,
        Method::POST,
        path,
        Some(json!({ "idempotencyKey": key, "request": request })),
    )
    .await)
}

async fn create_context(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, CallingError> {
    command(
        state,
        ctx,
        headers,
        names::PRIVACY_CONTEXTS_WRITE,
        "/internal/v1/contexts".into(),
        body,
        true,
    )
    .await
}

async fn list_contexts(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    params: Result<Query<ListParams>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::PRIVACY_CONTEXTS_READ)?;
    let Query(params) =
        params.map_err(|_| CallingError::Validation("The query parameters are invalid."))?;
    if params.context_id.is_some() {
        return Err(CallingError::Validation(
            "The query parameters are invalid.",
        ));
    }
    let body = serde_json::to_value(params).map_err(|_| CallingError::Internal)?;
    Ok(forward(
        &state,
        &ctx,
        Method::POST,
        "/internal/v1/contexts/query".into(),
        Some(body),
    )
    .await)
}

async fn get_context(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::PRIVACY_CONTEXTS_READ)?;
    let id = match uuid_path(&id, "CONTEXT_NOT_FOUND") {
        Ok(id) => id,
        Err(code) => return Ok(not_found(code)),
    };
    Ok(forward(
        &state,
        &ctx,
        Method::GET,
        format!("/internal/v1/contexts/{id}"),
        None,
    )
    .await)
}

async fn context_evidence(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::PRIVACY_CONTEXTS_READ)?;
    let id = match uuid_path(&id, "CONTEXT_NOT_FOUND") {
        Ok(id) => id,
        Err(code) => return Ok(not_found(code)),
    };
    Ok(forward(
        &state,
        &ctx,
        Method::GET,
        format!("/internal/v1/contexts/{id}/evidence"),
        None,
    )
    .await)
}

async fn update_context(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::PRIVACY_CONTEXTS_WRITE)?;
    let id = match uuid_path(&id, "CONTEXT_NOT_FOUND") {
        Ok(id) => id,
        Err(code) => return Ok(not_found(code)),
    };
    command(
        state,
        ctx,
        headers,
        names::PRIVACY_CONTEXTS_WRITE,
        format!("/internal/v1/contexts/{id}/update"),
        body,
        true,
    )
    .await
}

macro_rules! lifecycle {
    ($name:ident, $scope:expr, $kind:literal, $code:literal, $action:literal) => {
        async fn $name(
            State(state): State<AppState>,
            ctx: PlatformAuthContext,
            headers: HeaderMap,
            Path(id): Path<String>,
            body: Result<Json<Value>, JsonRejection>,
        ) -> Result<Response, CallingError> {
            ctx.require_scope($scope)?;
            let id = match uuid_path(&id, $code) {
                Ok(id) => id,
                Err(code) => return Ok(not_found(code)),
            };
            command(
                state,
                ctx,
                headers,
                $scope,
                format!("/internal/v1/{}/{id}/{}", $kind, $action),
                body,
                false,
            )
            .await
        }
    };
}

lifecycle!(
    suspend_context,
    names::PRIVACY_CONTEXTS_WRITE,
    "contexts",
    "CONTEXT_NOT_FOUND",
    "suspend"
);
lifecycle!(
    resume_context,
    names::PRIVACY_CONTEXTS_WRITE,
    "contexts",
    "CONTEXT_NOT_FOUND",
    "resume"
);
lifecycle!(
    complete_context,
    names::PRIVACY_CONTEXTS_WRITE,
    "contexts",
    "CONTEXT_NOT_FOUND",
    "complete"
);
lifecycle!(
    revoke_context,
    names::PRIVACY_CONTEXTS_WRITE,
    "contexts",
    "CONTEXT_NOT_FOUND",
    "revoke"
);
lifecycle!(
    suspend_session,
    names::PRIVACY_SESSIONS_WRITE,
    "sessions",
    "SESSION_NOT_FOUND",
    "suspend"
);
lifecycle!(
    resume_session,
    names::PRIVACY_SESSIONS_WRITE,
    "sessions",
    "SESSION_NOT_FOUND",
    "resume"
);
lifecycle!(
    revoke_session,
    names::PRIVACY_SESSIONS_WRITE,
    "sessions",
    "SESSION_NOT_FOUND",
    "revoke"
);

async fn create_session(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::PRIVACY_SESSIONS_WRITE)?;
    let id = match uuid_path(&id, "CONTEXT_NOT_FOUND") {
        Ok(id) => id,
        Err(code) => return Ok(not_found(code)),
    };
    command(
        state,
        ctx,
        headers,
        names::PRIVACY_SESSIONS_WRITE,
        format!("/internal/v1/contexts/{id}/sessions"),
        body,
        true,
    )
    .await
}

async fn list_sessions(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    params: Result<Query<ListParams>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::PRIVACY_SESSIONS_READ)?;
    let Query(params) =
        params.map_err(|_| CallingError::Validation("The query parameters are invalid."))?;
    if params.external_context_reference.is_some() {
        return Err(CallingError::Validation(
            "The query parameters are invalid.",
        ));
    }
    let body = serde_json::to_value(params).map_err(|_| CallingError::Internal)?;
    Ok(forward(
        &state,
        &ctx,
        Method::POST,
        "/internal/v1/sessions/query".into(),
        Some(body),
    )
    .await)
}

async fn get_session(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::PRIVACY_SESSIONS_READ)?;
    let id = match uuid_path(&id, "SESSION_NOT_FOUND") {
        Ok(id) => id,
        Err(code) => return Ok(not_found(code)),
    };
    Ok(forward(
        &state,
        &ctx,
        Method::GET,
        format!("/internal/v1/sessions/{id}"),
        None,
    )
    .await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_named_principal_references_are_collected() {
        let body = json!({
            "participants": [{ "principalId": "a" }, { "contactReferenceId": "c" }],
            "directions": [{ "from": { "principalId": "a" }, "to": { "principalId": "b" } }],
            "other": { "principalId": "ignored" },
        });
        assert_eq!(
            named_principals(&body).into_iter().collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn internal_and_unknown_codes_are_never_relayed() {
        for code in [
            "INTERNAL_AUTHENTICATION_FAILED",
            "INTERNAL_PERMISSION_DENIED",
            "INTERNAL_ERROR",
            "SOMETHING_NEW",
        ] {
            assert!(!RELAYED_ERROR_CODES.contains(&code));
            let r = relay_response(
                401,
                json!({ "error": { "code": code, "message": "x" } }),
                false,
            );
            assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        let r = relay_response(
            409,
            json!({ "error": { "code": "CONTEXT_VERSION_CONFLICT", "message": "m" } }),
            false,
        );
        assert_eq!(r.status(), StatusCode::CONFLICT);
        // A context naming an unknown (or foreign-scope) contact is the caller's error.
        let r = relay_response(
            404,
            json!({ "error": { "code": "CONTACT_NOT_FOUND", "message": "m" } }),
            false,
        );
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
}
