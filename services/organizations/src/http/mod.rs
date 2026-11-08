//! HTTP surface (contract `cfmeet.organizations.internal-api`, served at `/openapi.json`).
//!
//! * `GET /health` — liveness; `GET /ready` — database reachable and migrations applied.
//! * `/internal/v1/**` — service-authenticated (every request, unknown paths included):
//!   [`middleware::require_internal_service`].
//!
//! There is no public route: humans reach Organizations only through the Meeting Service
//! relay, which forwards the verified acting user. Extractor rejections are normalized to
//! the error envelope.

pub mod authorization;
pub mod bindings;
pub mod invitations;
pub mod members;
pub mod middleware;
pub mod migration;
pub mod organizations;
pub mod outbox;
pub mod policies;
pub mod teams;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{from_fn, from_fn_with_state, map_response},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use sea_orm::ConnectionTrait;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    domain::{evaluate_with_policy, Capability, DecisionReason},
    error::{error_response, ApiError, ErrorCode},
    state::AppState,
    store::{self, MembershipRow, OrgRow},
};

/// Request bodies above this are refused before parsing.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// The migration import carries a whole Meeting Service export.
pub const MAX_IMPORT_BYTES: usize = 32 * 1024 * 1024;

pub fn app(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/openapi.json", get(openapi))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state.clone())
        .layer(from_fn(middleware::request_context));
    let v1 = Router::new()
        .route("/organizations", post(organizations::create))
        .route(
            "/organizations/memberships/resolve",
            post(authorization::resolve),
        )
        .route(
            "/organizations/authorization/evaluate",
            post(authorization::evaluate),
        )
        .route(
            "/organizations/scopes/resolve",
            post(authorization::resolve_scope),
        )
        .route(
            "/organizations/:id",
            get(organizations::detail).patch(organizations::update),
        )
        .route("/organizations/:id/members", get(members::list))
        .route("/organizations/:id/members/me/leave", post(members::leave))
        .route(
            "/organizations/:id/members/:membership_id",
            patch(members::update),
        )
        .route(
            "/organizations/:id/ownership-transfer",
            post(members::transfer_ownership),
        )
        .route(
            "/organizations/:id/invitations",
            get(invitations::list).post(invitations::create),
        )
        .route(
            "/organizations/:id/invitations/:invitation_id/revoke",
            post(invitations::revoke),
        )
        .route(
            "/organization-invitations/accept",
            post(invitations::accept),
        )
        .route(
            "/organization-invitations/decline",
            post(invitations::decline),
        )
        .route("/organizations/:id/suspend", post(organizations::suspend))
        .route(
            "/organizations/:id/reactivate",
            post(organizations::reactivate),
        )
        .route("/organizations/:id/archive", post(organizations::archive))
        .route(
            "/organizations/:id/audit-events",
            get(authorization::audit_events),
        )
        .route(
            "/organizations/:id/provisioning",
            get(organizations::provisioning),
        )
        .route(
            "/organizations/:id/roles/capabilities",
            get(authorization::roles_capabilities),
        )
        .route(
            "/organizations/:id/policies",
            get(policies::get).patch(policies::update),
        )
        .route(
            "/organizations/:id/policies/effective",
            get(policies::effective),
        )
        .route(
            "/organizations/:id/teams",
            get(teams::list).post(teams::create),
        )
        .route(
            "/organizations/:id/teams/:team_id",
            get(teams::detail).patch(teams::update),
        )
        .route(
            "/organizations/:id/teams/:team_id/archive",
            post(teams::archive),
        )
        .route(
            "/organizations/:id/teams/:team_id/members",
            get(teams::members).post(teams::add_member),
        )
        .route(
            "/organizations/:id/teams/:team_id/members/:team_member_id",
            patch(teams::update_member),
        )
        .route(
            "/organizations/:id/teams/:team_id/members/:team_member_id/remove",
            post(teams::remove),
        )
        .route(
            "/users/:user_id/organizations",
            get(organizations::user_organizations),
        )
        .route(
            "/admin/organizations",
            get(organizations::admin_list).post(organizations::admin_create),
        )
        .route(
            "/admin/organizations/:id",
            patch(organizations::admin_update),
        )
        .route(
            "/admin/organizations/:id/provisioning/retry",
            post(organizations::provisioning_retry),
        )
        .route(
            "/admin/organizations/:id/scopes/primary/service-bindings/calling",
            get(bindings::get_calling)
                .put(bindings::put_calling)
                .delete(bindings::delete_calling),
        )
        .route("/admin/event-delivery", get(outbox::status))
        .route("/admin/event-delivery/replay", post(outbox::replay))
        .route(
            "/migration/import",
            post(migration::import).layer(DefaultBodyLimit::max(MAX_IMPORT_BYTES)),
        )
        .route("/migration/reconciliation", get(migration::reconciliation))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed);
    let internal = Router::new()
        .nest("/internal/v1", v1)
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(
            state.clone(),
            middleware::require_internal_service,
        ))
        .with_state(state);
    public.merge(internal)
}

/// The served API description: exactly the checked-in contract.
pub const OPENAPI: &str =
    include_str!("../../../../packages/contracts/organizations/internal/v1.openapi.json");

async fn openapi() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        OPENAPI,
    )
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "organizations" }))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let pending = crate::pending_migrations(&state.db).await;
    let (ok, database, migrations) = match pending {
        Ok(p) if p.is_empty() => (true, "ok", "current"),
        Ok(_) => (false, "ok", "pending"),
        Err(_) => (false, "unavailable", "unknown"),
    };
    let backlog = if ok {
        crate::outbox::backlog(&state.db).await.ok()
    } else {
        None
    };
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "status": if ok { "ready" } else { "not_ready" },
            "service": "organizations",
            "database": database,
            "migrations": migrations,
            "outbox": backlog.map(|b| json!({
                "pending": b.pending,
                "oldestPendingAt": b.oldest.map(store::rfc3339),
                "delivery": "disabled",
            })),
            "provisioning": { "enabled": state.provisioning_enabled },
        })),
    )
}

async fn normalize_rejections(response: Response) -> Response {
    let status = response.status();
    let json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if json || !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let code = match status {
        StatusCode::PAYLOAD_TOO_LARGE => ErrorCode::PayloadTooLarge,
        StatusCode::NOT_FOUND => ErrorCode::NotFound,
        StatusCode::METHOD_NOT_ALLOWED => ErrorCode::MethodNotAllowed,
        s if s.is_client_error() => ErrorCode::ValidationError,
        _ => ErrorCode::InternalError,
    };
    error_response(code, None)
}

async fn not_found() -> Response {
    error_response(ErrorCode::NotFound, None)
}

async fn method_not_allowed() -> Response {
    error_response(ErrorCode::MethodNotAllowed, None)
}

// ---------------------------------------------------------------------------------------
// Handler helpers
// ---------------------------------------------------------------------------------------

/// Parses a JSON body (empty = `{}`) into `T`, keeping the canonical value for
/// idempotency digests. Unknown fields are refused by every request type.
pub fn parse<T: DeserializeOwned>(body: &Bytes) -> Result<(T, Value), ApiError> {
    let value: Value = if body.iter().all(|b| b.is_ascii_whitespace()) {
        json!({})
    } else {
        serde_json::from_slice(body)
            .map_err(|_| ApiError::validation("The request body must be a JSON object."))?
    };
    if !value.is_object() {
        return Err(ApiError::validation(
            "The request body must be a JSON object.",
        ));
    }
    // serde messages may quote submitted values; the envelope never echoes them.
    let typed = serde_json::from_value(value.clone()).map_err(|_| {
        ApiError::validation("The request body has a missing, unknown or mistyped field.")
    })?;
    Ok((typed, value))
}

/// A JSON success response (`Cache-Control: no-store`).
pub fn respond(status: StatusCode, body: Value) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub fn membership_not_found() -> ApiError {
    ApiError::with_message(
        ErrorCode::OrganizationNotFound,
        "The membership was not found.",
    )
}

/// The actor's access to an organization.
pub struct Access {
    pub org: OrgRow,
    pub membership: MembershipRow,
}

/// Loads the organization (optionally `FOR UPDATE`: every mutation locks the organization
/// row first, which serializes membership changes and the last-owner invariant) and checks
/// the actor's `capability`:
/// * unknown organization, no membership, removed membership → 404 (non-enumerating);
/// * suspended membership → 403 `MEMBERSHIP_REQUIRED`;
/// * suspended / archived organization → 409 (OWNERs keep `organization.read`);
/// * role lacks the capability → 403 `PERMISSION_DENIED`.
pub async fn authorize<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    user: Uuid,
    capability: Capability,
    lock: bool,
) -> Result<Access, ApiError> {
    let org = store::org(c, organization, lock)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let membership = store::current_membership(c, organization, user, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let policy = store::policy(c, organization).await?;
    let decision = evaluate_with_policy(
        Some((membership.role(), membership.status())),
        org.status(),
        capability,
        &policy,
    );
    match decision {
        DecisionReason::Allowed => Ok(Access { org, membership }),
        DecisionReason::NotAMember => Err(ApiError::new(ErrorCode::OrganizationNotFound)),
        DecisionReason::MembershipSuspended => Err(ApiError::new(ErrorCode::MembershipRequired)),
        DecisionReason::OrganizationSuspended => {
            Err(ApiError::new(ErrorCode::OrganizationSuspended))
        }
        DecisionReason::OrganizationArchived => Err(ApiError::new(ErrorCode::OrganizationArchived)),
        DecisionReason::CapabilityNotGranted => Err(ApiError::new(ErrorCode::PermissionDenied)),
        DecisionReason::PolicyRestricted => Err(ApiError::new(ErrorCode::PolicyRestricted)),
    }
}

/// Pagination: `limit` 1–200 (default 50).
pub fn page_limit(limit: Option<i64>) -> Result<i64, ApiError> {
    match limit {
        None => Ok(50),
        Some(n) if (1..=200).contains(&n) => Ok(n),
        Some(_) => Err(ApiError::validation("limit must be between 1 and 200")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{body::Body, http::Request};
    use cf_service_auth::{
        KeyId, RuntimeEnvironment, ServiceSigningKey, ServiceTokenVerifier, SystemClock,
        TrustPolicy,
    };
    use tower::ServiceExt;

    use super::*;

    fn router() -> Router {
        let key = ServiceSigningKey::generate(KeyId::parse("meeting-test-1").unwrap());
        let bundle = json!({
            "bundle_version": 1,
            "receiver": cf_service_auth::services::ORGANIZATIONS,
            "runtime_environment": "test",
            "trusted_issuers": [{
                "issuer": cf_service_auth::services::MEETING_SERVICE,
                "audience": cf_service_auth::services::ORGANIZATIONS,
                "runtime_environment": "test",
                "keys": [key.public_jwk()],
                "permissions": ["organization.read", "organization.write", "organization.act-for-user"],
            }],
        })
        .to_string();
        let policy = TrustPolicy::from_json(
            &bundle,
            &crate::config::organizations_service_id(),
            RuntimeEnvironment::Test,
        )
        .unwrap();
        let verifier = Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)));
        app(AppState::new(
            sea_orm::DatabaseConnection::Disconnected,
            verifier,
        ))
    }

    #[tokio::test]
    async fn the_route_table_builds_and_internal_routes_require_service_auth() {
        let app = router();
        for (method, path) in [
            (
                "GET",
                "/internal/v1/organizations/00000000-0000-4000-8000-000000000001",
            ),
            ("POST", "/internal/v1/organizations/memberships/resolve"),
            ("POST", "/internal/v1/organizations/authorization/evaluate"),
            ("POST", "/internal/v1/migration/import"),
            (
                "PUT",
                "/internal/v1/admin/organizations/00000000-0000-4000-8000-000000000001/scopes/primary/service-bindings/calling",
            ),
            ("GET", "/internal/v1/no-such-route"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL).unwrap(),
                "no-store"
            );
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let v: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["schemaVersion"], 1);
            assert_eq!(v["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
            assert!(v["error"]["requestId"]
                .as_str()
                .unwrap()
                .starts_with("req_"));
        }
        let health = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
    }

    #[test]
    fn bodies_are_strict_json_objects() {
        #[derive(Debug, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct B {
            #[allow(dead_code)]
            a: Option<i64>,
        }
        assert!(parse::<B>(&Bytes::from_static(b"")).is_ok());
        assert!(parse::<B>(&Bytes::from_static(b"[]")).is_err());
        assert!(parse::<B>(&Bytes::from_static(b"{\"b\":1}")).is_err());
        assert!(parse::<B>(&Bytes::from_static(b"not json")).is_err());
    }
}
