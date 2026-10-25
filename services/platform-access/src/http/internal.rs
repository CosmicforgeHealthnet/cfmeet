//! Service-facing internal API (contract `cfmeet.platform-access.internal-api`).
//!
//! | Route | Permission | Scope source |
//! |---|---|---|
//! | `POST /internal/v1/credentials/authenticate` | `platform-credential.authenticate` | the presented credential itself |
//! | `GET /internal/v1/credentials/{id}` | `platform-identity.read` | signed delegated context (must own the credential) |
//! | `POST /internal/v1/authorization/evaluate` | `platform-identity.read` | signed delegated context |
//! | `GET /internal/v1/identity/context` | `platform-identity.read` | signed delegated context |
//! | `POST /internal/v1/identity/changes` | `platform-identity.read` | — (identity metadata, no verifiers) |
//! | `GET /internal/v1/identity/imports/{digest}` | `platform-identity.read` | — |
//! | `GET /internal/v1/scopes` | `platform-identity.read` | — |
//! | `POST /internal/v1/principals/resolve` | `platform-principal.resolve` | signed delegated context |
//! | `POST /internal/v1/principals/telephone-endpoints` | `platform-principal.register` | signed delegated context |
//! | `POST /internal/v1/principals/ensure` | `platform-principal.ensure` | signed delegated context of a FIRST_PARTY tenant (NATIVE_CF_USER / TEMPORARY_EXTERNAL_PARTICIPANT / BOT_OR_SERVICE only) |
//! | `POST /internal/v1/principals/{id}/tombstone` | `platform-principal.register` | — (a named operator's retention decision, audited) |
//!
//! A tenant / application / environment identifier in a request BODY is never
//! authoritative: scope comes from the presented credential (authentication) or from the
//! signed delegated context, which this service re-validates against its own authority
//! chain (the environment must belong to the application and tenant, the initiating
//! service account to the environment) before using it.

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::{DelegatedPlatformContext, InternalPermission};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::error::{AccessError, AccessResult, ErrorCode};
use super::middleware::InternalCaller;
use crate::{
    domain::{EnvironmentKind, PrincipalId, PrincipalKind},
    identity::{
        authentication::{
            AuthenticationError, AuthorityChain, AuthorityMode, SuspendedLink, Throttle,
        },
        changes,
        credentials::CredentialEffectiveState,
        handoff,
        operator::OperatorContext,
        principals::{PrincipalReference, PrincipalResolutionError, PrincipalResolver},
        snapshots::{
            from_row_json, ApplicationSnapshot, CredentialSnapshot, EnvironmentSnapshot,
            PrincipalSnapshot, ServiceAccountSnapshot, TenantSnapshot,
        },
    },
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/credentials/authenticate", post(authenticate))
        .route(
            "/credentials/delegated-authority",
            post(delegated_authority),
        )
        .route("/credentials/:credential_id", get(credential_status))
        .route("/authorization/evaluate", post(evaluate))
        .route("/identity/context", get(trusted_context))
        .route("/identity/changes", post(identity_changes))
        .route("/identity/imports/:bundle_digest", get(import_receipt))
        .route("/scopes", get(scope_registry))
        .route("/principals/resolve", post(resolve_principals))
        .route(
            "/principals/telephone-endpoints",
            post(ensure_telephone_endpoint),
        )
        .route("/principals/ensure", post(ensure_principal))
        .route(
            "/principals/:principal_id/tombstone",
            post(tombstone_principal),
        )
}

fn internal(err: sea_orm::DbErr) -> AccessError {
    AccessError::service(err.into())
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuthenticateRequest {
    /// The presented customer credential (bearer value). Zeroized after use, never logged.
    #[serde(deserialize_with = "zeroizing_string")]
    credential: Zeroizing<String>,
}

fn zeroizing_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(d).map(Zeroizing::new)
}

#[derive(Serialize)]
#[serde(
    rename_all = "camelCase",
    tag = "outcome",
    rename_all_fields = "camelCase"
)]
enum AuthenticateResponse {
    #[serde(rename = "AUTHENTICATED")]
    Authenticated { authority: Box<AuthorityChain> },
    #[serde(rename = "REJECTED")]
    Rejected {
        reason: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        public_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        throttle: Option<Throttle>,
    },
}

/// Credential presented values are bounded before any work.
const MAX_PRESENTED: usize = 256;

/// Part VI §51: the CURRENT authority chain of the credential the API Gateway
/// authenticated, for a receiving domain service. The credential id comes ONLY from the
/// signed delegated context (never a request body), and the chain must be exactly the
/// delegated tenant / application / environment / service account: anything else is
/// reported like an unknown credential. Revocation, expiry, supersession, suspension
/// (SAFETY_ONLY) and effective scopes are evaluated now, exactly as at authentication.
async fn delegated_authority(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformCredentialAuthenticate)?;
    let delegated = caller
        .context()
        .delegated_platform_context()
        .cloned()
        .ok_or(AccessError::Refused(ErrorCode::DelegatedContextRequired))?;
    let Some(credential_id) = delegated.initiating_credential_id() else {
        return Err(AccessError::Refused(ErrorCode::DelegatedContextRequired));
    };
    let rejected = |reason: &'static str| AuthenticateResponse::Rejected {
        reason,
        public_id: None,
        throttle: None,
    };
    match state.authenticator.authority_of(credential_id).await {
        Ok(chain)
            if chain.tenant.id == delegated.tenant_id()
                && chain.application.id == delegated.application_id()
                && chain.environment.id == delegated.application_environment_id()
                && chain.service_account.id == delegated.initiating_service_account_id() =>
        {
            Ok(Json(AuthenticateResponse::Authenticated {
                authority: Box::new(chain),
            }))
        }
        Ok(_) => {
            tracing::warn!(
                caller = caller.service(),
                "delegated credential does not belong to the delegated scope"
            );
            Ok(Json(rejected("unknown_credential")))
        }
        Err(AuthenticationError::Rejected { reason, .. }) => Ok(Json(rejected(reason.as_str()))),
        Err(AuthenticationError::Unavailable { summary }) => {
            tracing::error!(%summary, "credential authority unavailable");
            Err(AccessError::Refused(ErrorCode::ServiceUnavailable))
        }
    }
}

async fn authenticate(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<AuthenticateRequest>,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformCredentialAuthenticate)?;
    if body.credential.len() > MAX_PRESENTED {
        return Ok(Json(AuthenticateResponse::Rejected {
            reason: "malformed_credential",
            public_id: None,
            throttle: None,
        }));
    }
    let outcome = state
        .authenticator
        .authenticate(body.credential.trim())
        .await;
    drop(body);
    match outcome {
        Ok(chain) => {
            crate::metrics::inc(
                "platform_access_authentications_total",
                &[("outcome", "AUTHENTICATED")],
            );
            tracing::info!(
                caller = caller.service(),
                credential_id = %chain.credential.id,
                tenant_id = %chain.tenant.id,
                mode = ?chain.authority_mode,
                "credential authenticated"
            );
            Ok(Json(AuthenticateResponse::Authenticated {
                authority: Box::new(chain),
            }))
        }
        Err(AuthenticationError::Rejected { reason, public_id }) => {
            crate::metrics::inc(
                "platform_access_authentications_total",
                &[("outcome", reason.as_str())],
            );
            // §109: a failed window store keeps the plain rejection (fail closed anyway).
            let throttle = match state
                .authenticator
                .note_failure(reason, public_id.as_deref())
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(summary = %crate::identity::error::summarize_db_error(&e), "failed-authentication window unavailable");
                    None
                }
            };
            tracing::info!(
                caller = caller.service(),
                reason = reason.as_str(),
                public_id = public_id.as_deref().unwrap_or(""),
                "credential authentication rejected"
            );
            Ok(Json(AuthenticateResponse::Rejected {
                reason: reason.as_str(),
                public_id,
                throttle,
            }))
        }
        Err(AuthenticationError::Unavailable { summary }) => {
            tracing::error!(%summary, "credential authority unavailable");
            Err(AccessError::Refused(ErrorCode::ServiceUnavailable))
        }
    }
}

// ---------------------------------------------------------------------------
// Trusted delegated scope
// ---------------------------------------------------------------------------

/// The delegated scope re-validated against this service's authority chain.
pub(crate) struct TrustedScope {
    pub tenant: TenantSnapshot,
    pub application: ApplicationSnapshot,
    pub environment: EnvironmentSnapshot,
    pub service_account: ServiceAccountSnapshot,
}

impl TrustedScope {
    fn authority_mode(&self) -> (AuthorityMode, Option<SuspendedLink>) {
        use crate::domain::{ApplicationStatus, EnvironmentStatus, TenantStatus};
        let link = if self.tenant.status != TenantStatus::Active {
            Some(SuspendedLink::Tenant)
        } else if self.application.status != ApplicationStatus::Active {
            Some(SuspendedLink::Application)
        } else if self.environment.status != EnvironmentStatus::Active {
            Some(SuspendedLink::Environment)
        } else {
            None
        };
        (
            if link.is_some() {
                AuthorityMode::SafetyOnly
            } else {
                AuthorityMode::Full
            },
            link,
        )
    }
}

fn delegated_kind(ctx: &DelegatedPlatformContext) -> EnvironmentKind {
    use cf_service_auth::ApplicationEnvironmentKind as K;
    match ctx.application_environment_kind() {
        K::Sandbox => EnvironmentKind::Sandbox,
        K::Test => EnvironmentKind::Test,
        K::Production => EnvironmentKind::Production,
    }
}

/// Loads and checks the delegated scope: every id must exist and the chain must be
/// consistent (environment ∈ application ∈ tenant, environment kind as signed, initiating
/// service account ∈ environment). Anything else is DELEGATED_CONTEXT_INVALID — a forged
/// or stale tenant / application claim never becomes authority here.
pub(crate) async fn trusted_scope(
    state: &AppState,
    caller: &InternalCaller,
) -> AccessResult<TrustedScope> {
    let ctx = caller.delegated()?;
    let row = state
        .platform
        .db()
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT to_jsonb(t) AS tenant, to_jsonb(a) AS application, to_jsonb(e) AS environment, \
                    to_jsonb(sa) AS service_account \
             FROM platform_environments e \
             JOIN platform_applications a ON a.id = e.application_id AND a.tenant_id = e.tenant_id \
             JOIN platform_tenants t ON t.id = e.tenant_id \
             JOIN platform_service_accounts sa ON sa.environment_id = e.id AND sa.application_id = e.application_id \
                                              AND sa.tenant_id = e.tenant_id \
             WHERE e.id = $1 AND e.application_id = $2 AND e.tenant_id = $3 AND sa.id = $4",
            [
                ctx.application_environment_id().into(),
                ctx.application_id().into(),
                ctx.tenant_id().into(),
                ctx.initiating_service_account_id().into(),
            ],
        ))
        .await
        .map_err(internal)?;
    let invalid = || {
        crate::metrics::inc("platform_access_delegated_context_rejected_total", &[]);
        tracing::warn!(
            caller = caller.service(),
            "delegated platform context rejected"
        );
        AccessError::Refused(ErrorCode::DelegatedContextInvalid)
    };
    let row = row.ok_or_else(invalid)?;
    let get =
        |col: &str| -> AccessResult<Value> { row.try_get::<Value>("", col).map_err(internal) };
    let corrupt = |_| AccessError::Refused(ErrorCode::InternalError);
    let scope = TrustedScope {
        tenant: from_row_json(get("tenant")?).map_err(corrupt)?,
        application: from_row_json(get("application")?).map_err(corrupt)?,
        environment: from_row_json(get("environment")?).map_err(corrupt)?,
        service_account: from_row_json(get("service_account")?).map_err(corrupt)?,
    };
    if scope.environment.kind != delegated_kind(ctx) {
        return Err(invalid());
    }
    Ok(scope)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrustedContextView {
    authority_mode: AuthorityMode,
    suspended_link: Option<SuspendedLink>,
    tenant: TenantSnapshot,
    application: ApplicationSnapshot,
    environment: EnvironmentSnapshot,
    service_account: ServiceAccountSnapshot,
}

async fn trusted_context(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformIdentityRead)?;
    let scope = trusted_scope(&state, &caller).await?;
    let (authority_mode, suspended_link) = scope.authority_mode();
    Ok(Json(TrustedContextView {
        authority_mode,
        suspended_link,
        tenant: scope.tenant,
        application: scope.application,
        environment: scope.environment,
        service_account: scope.service_account,
    }))
}

// ---------------------------------------------------------------------------
// Credential status and scope evaluation
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CredentialStatusView {
    credential: CredentialSnapshot,
    effective_state: CredentialEffectiveState,
    authenticates: bool,
    scopes: Vec<String>,
    principal_restriction: Option<Vec<Uuid>>,
}

async fn load_credential_in_scope(
    state: &AppState,
    scope: &TrustedScope,
    credential_id: Uuid,
) -> AccessResult<CredentialStatusView> {
    let row = state
        .platform
        .db()
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT to_jsonb(c) - 'verifier' - 'verifier_algorithm' - 'verifier_key_version' AS credential \
             FROM platform_api_credentials c \
             WHERE c.id = $1 AND c.tenant_id = $2 AND c.application_id = $3 AND c.environment_id = $4",
            [
                credential_id.into(),
                scope.tenant.id.into(),
                scope.application.id.into(),
                scope.environment.id.into(),
            ],
        ))
        .await
        .map_err(internal)?
        // Another scope's credential is reported exactly like a missing one.
        .ok_or(AccessError::Refused(ErrorCode::NotFound))?;
    let credential: CredentialSnapshot =
        from_row_json(row.try_get::<Value>("", "credential").map_err(internal)?)
            .map_err(|_| AccessError::Refused(ErrorCode::InternalError))?;
    let effective_state = CredentialEffectiveState::evaluate(
        credential.status,
        credential.not_before,
        credential.expires_at,
        credential.supersession_valid_until,
        state.platform.clock().now(),
    )
    .with_suspension(
        state
            .platform
            .credential_suspended(crate::domain::CredentialId::from_uuid(credential.id))
            .await
            .map_err(AccessError::service)?,
    );
    let scopes = effective_scopes(state, scope, credential.id).await?;
    let principal_restriction = state
        .platform
        .credential_principal_restriction(crate::domain::CredentialId::from_uuid(credential.id))
        .await
        .map_err(AccessError::service)?
        .map(|ids| ids.into_iter().map(|p| p.as_uuid()).collect());
    Ok(CredentialStatusView {
        authenticates: effective_state.authenticates(),
        credential,
        effective_state,
        scopes,
        principal_restriction,
    })
}

async fn effective_scopes(
    state: &AppState,
    scope: &TrustedScope,
    credential_id: Uuid,
) -> AccessResult<Vec<String>> {
    use crate::identity::scopes::{GrantBasis, PersistedGrant};
    let rows = state
        .platform
        .db()
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT cs.scope, aps.grant_basis, \
                    (aps.allowed_environment_kinds IS NULL OR $2 = ANY (aps.allowed_environment_kinds)) AS environment_allowed \
             FROM platform_api_credential_scopes cs \
             JOIN platform_application_scopes aps ON aps.application_id = cs.application_id AND aps.scope = cs.scope \
             WHERE cs.credential_id = $1 ORDER BY cs.scope",
            [credential_id.into(), scope.environment.kind.as_str().into()],
        ))
        .await
        .map_err(internal)?;
    let grants = rows
        .iter()
        .map(|r| -> Result<PersistedGrant, sea_orm::DbErr> {
            let basis: String = r.try_get("", "grant_basis")?;
            Ok(PersistedGrant {
                scope: r.try_get("", "scope")?,
                grant_basis: GrantBasis::parse(&basis).unwrap_or(GrantBasis::Standard),
                environment_allowed: r.try_get("", "environment_allowed")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    Ok(state
        .registry
        .effective(grants, scope.tenant.classification)
        .names())
}

async fn credential_status(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(credential_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformIdentityRead)?;
    let scope = trusted_scope(&state, &caller).await?;
    Ok(Json(
        load_credential_in_scope(&state, &scope, credential_id).await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvaluateRequest {
    credential_id: Uuid,
    required_scopes: Vec<String>,
    #[serde(default)]
    principal_id: Option<Uuid>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvaluateResponse {
    allowed: bool,
    /// `CREDENTIAL_NOT_AUTHENTICATING`, `SCOPE_SUSPENDED`, `MISSING_SCOPES`,
    /// `PRINCIPAL_NOT_AUTHORIZED` — or absent when allowed.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    missing_scopes: Vec<String>,
    effective_state: CredentialEffectiveState,
}

/// Re-evaluates, NOW, whether the credential of the delegated scope may perform an
/// operation needing `requiredScopes` (and, when given, for `principalId`).
async fn evaluate(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<EvaluateRequest>,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformIdentityRead)?;
    if body.required_scopes.len() > 32 {
        return Err(AccessError::Refused(ErrorCode::ValidationError));
    }
    let scope = trusted_scope(&state, &caller).await?;
    let status = load_credential_in_scope(&state, &scope, body.credential_id).await?;
    let missing: Vec<String> = body
        .required_scopes
        .iter()
        .filter(|s| !status.scopes.contains(s))
        .map(|s| s.chars().take(100).collect())
        .collect();
    let (mode, _) = scope.authority_mode();
    let principal_ok = match (&status.principal_restriction, body.principal_id) {
        (Some(allowed), Some(p)) => allowed.contains(&p),
        (Some(_), None) => true,
        (None, _) => true,
    };
    let reason = if !status.authenticates
        || status.credential.service_account_id != scope.service_account.id
    {
        Some("CREDENTIAL_NOT_AUTHENTICATING")
    } else if mode != AuthorityMode::Full {
        Some("SCOPE_SUSPENDED")
    } else if !missing.is_empty() {
        Some("MISSING_SCOPES")
    } else if !principal_ok {
        Some("PRINCIPAL_NOT_AUTHORIZED")
    } else {
        None
    };
    Ok(Json(EvaluateResponse {
        allowed: reason.is_none(),
        reason,
        missing_scopes: missing,
        effective_state: status.effective_state,
    }))
}

// ---------------------------------------------------------------------------
// Change feed, receipts, registry
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangesQuery {
    #[serde(default)]
    after: i64,
    #[serde(default)]
    limit: Option<u64>,
}

/// A page of the change feed after the `after` cursor (POST: the internal client never
/// sends query strings).
async fn identity_changes(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(q): Json<ChangesQuery>,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformIdentityRead)?;
    let page = changes::read_page(state.platform.db(), q.after, q.limit.unwrap_or(200))
        .await
        .map_err(AccessError::service)?;
    Ok(Json(page))
}

async fn import_receipt(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(bundle_digest): Path<String>,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformIdentityRead)?;
    if bundle_digest.len() != 64 || !bundle_digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AccessError::Refused(ErrorCode::ValidationError));
    }
    let receipt = handoff::receipt(state.platform.db(), &bundle_digest)
        .await
        .map_err(AccessError::service)?
        .ok_or(AccessError::Refused(ErrorCode::NotFound))?;
    Ok(Json(receipt))
}

async fn scope_registry(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformIdentityRead)?;
    let scopes: Vec<Value> = state
        .registry
        .definitions()
        .map(|d| json!({ "name": d.name, "description": d.description, "trust": d.trust }))
        .collect();
    Ok(Json(json!({ "scopes": scopes })))
}

// ---------------------------------------------------------------------------
// Principals
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReferenceInput {
    #[serde(default)]
    principal_id: Option<Uuid>,
    #[serde(default)]
    kind: Option<PrincipalKind>,
    #[serde(default)]
    external_reference: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveRequest {
    references: Vec<ReferenceInput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolutionView {
    /// `RESOLVED`, `NOT_FOUND`, `INACTIVE`, `INVALID_REFERENCE`.
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    principal: Option<PrincipalSnapshot>,
}

/// Largest resolution batch.
const MAX_REFERENCES: usize = 50;

async fn principal_snapshot(state: &AppState, id: Uuid) -> AccessResult<PrincipalSnapshot> {
    let row = state
        .platform
        .db()
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT to_jsonb(p) AS principal FROM platform_principals p WHERE p.id = $1",
            [id.into()],
        ))
        .await
        .map_err(internal)?
        .ok_or(AccessError::Refused(ErrorCode::NotFound))?;
    from_row_json(row.try_get::<Value>("", "principal").map_err(internal)?)
        .map_err(|_| AccessError::Refused(ErrorCode::InternalError))
}

async fn resolve_principals(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<ResolveRequest>,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformPrincipalResolve)?;
    if body.references.is_empty() || body.references.len() > MAX_REFERENCES {
        return Err(AccessError::Refused(ErrorCode::ValidationError));
    }
    let scope = trusted_scope(&state, &caller).await?;
    let resolver = PrincipalResolver::new(state.platform.db().clone());
    let mut results = Vec::with_capacity(body.references.len());
    for input in body.references {
        let reference = match (input.principal_id, input.kind, input.external_reference) {
            (Some(id), None, None) => PrincipalReference::Id(PrincipalId::from_uuid(id)),
            (None, Some(kind), Some(external_reference)) => PrincipalReference::External {
                kind,
                external_reference,
            },
            _ => {
                results.push(ResolutionView {
                    outcome: "INVALID_REFERENCE",
                    principal: None,
                });
                continue;
            }
        };
        let outcome = resolver
            .resolve_in(
                crate::domain::TenantId::from_uuid(scope.tenant.id),
                crate::domain::ApplicationId::from_uuid(scope.application.id),
                crate::domain::EnvironmentId::from_uuid(scope.environment.id),
                &reference,
            )
            .await;
        results.push(match outcome {
            Ok(p) => ResolutionView {
                outcome: "RESOLVED",
                principal: Some(principal_snapshot(&state, p.id().as_uuid()).await?),
            },
            Err(PrincipalResolutionError::NotFound) => ResolutionView {
                outcome: "NOT_FOUND",
                principal: None,
            },
            Err(PrincipalResolutionError::Inactive) => ResolutionView {
                outcome: "INACTIVE",
                principal: None,
            },
            Err(PrincipalResolutionError::InvalidReference) => ResolutionView {
                outcome: "INVALID_REFERENCE",
                principal: None,
            },
            Err(PrincipalResolutionError::Unavailable { summary }) => {
                tracing::error!(%summary, "principal directory unavailable");
                return Err(AccessError::Refused(ErrorCode::ServiceUnavailable));
            }
        });
    }
    Ok(Json(json!({ "results": results })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TelephoneEndpointRequest {
    contact_reference: Uuid,
}

/// The TELEPHONE_ENDPOINT principal of a protected public party (Part E §45): its only
/// reference is the opaque ContactReference — never a number. Idempotent.
async fn ensure_telephone_endpoint(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<TelephoneEndpointRequest>,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformPrincipalRegister)?;
    let scope = trusted_scope(&state, &caller).await?;
    let resolver = PrincipalResolver::new(state.platform.db().clone());
    let resolved = resolver
        .ensure_public_party_principal(
            crate::domain::TenantId::from_uuid(scope.tenant.id),
            crate::domain::ApplicationId::from_uuid(scope.application.id),
            crate::domain::EnvironmentId::from_uuid(scope.environment.id),
            body.contact_reference,
        )
        .await;
    match resolved {
        Ok(p) => Ok(Json(json!({
            "principal": principal_snapshot(&state, p.id().as_uuid()).await?
        }))),
        Err(PrincipalResolutionError::Unavailable { summary }) => {
            tracing::error!(%summary, "principal directory unavailable");
            Err(AccessError::Refused(ErrorCode::ServiceUnavailable))
        }
        // A tombstoned / deactivated endpoint principal stays unusable.
        Err(PrincipalResolutionError::Inactive) => {
            Err(AccessError::Refused(ErrorCode::InvalidState))
        }
        Err(_) => Err(AccessError::Refused(ErrorCode::ValidationError)),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnsurePrincipalRequest {
    kind: String,
    external_reference: String,
}

/// Part V H §104: the first-party login authority (the Meeting Service) maps an account
/// it authenticated itself to its communication principal — idempotently, inside its
/// signed delegated scope, which must be a FULL-authority FIRST_PARTY tenant. Only the
/// native-account and meeting-participant categories; a SUSPENDED / DEACTIVATED /
/// tombstoned principal is returned as refused (never reactivated).
async fn ensure_principal(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<EnsurePrincipalRequest>,
) -> AccessResult<impl IntoResponse> {
    use crate::domain::{EnvironmentId, PrincipalStatus, TenantClassification};
    use crate::identity::principals::NewPrincipal;
    caller.require_permission(InternalPermission::PlatformPrincipalEnsure)?;
    let scope = trusted_scope(&state, &caller).await?;
    if scope.tenant.classification != TenantClassification::FirstParty {
        return Err(AccessError::Refused(ErrorCode::PolicyViolation));
    }
    if scope.authority_mode().1.is_some() {
        return Err(AccessError::Refused(ErrorCode::InvalidState));
    }
    let kind = match body.kind.as_str() {
        "NATIVE_CF_USER" => PrincipalKind::NativeCfUser,
        "TEMPORARY_EXTERNAL_PARTICIPANT" => PrincipalKind::TemporaryExternalParticipant,
        // Part V remediation (meeting breakout / private chat): the first-party
        // application's own non-human participant, e.g. the owner of conversations no
        // person may own. Nobody authenticates as it through Platform Access.
        "BOT_OR_SERVICE" => PrincipalKind::BotOrService,
        _ => return Err(AccessError::Refused(ErrorCode::ValidationError)),
    };
    let (principal, created) = state
        .platform
        .register_principal(
            EnvironmentId::from_uuid(scope.environment.id),
            NewPrincipal::External {
                kind,
                external_reference: body.external_reference,
            },
        )
        .await
        .map_err(AccessError::service)?;
    if principal.status != PrincipalStatus::Active || principal.tombstoned_at.is_some() {
        return Err(AccessError::Refused(ErrorCode::InvalidState));
    }
    crate::metrics::inc(
        "platform_access_principals_ensured_total",
        &[("created", if created { "true" } else { "false" })],
    );
    Ok(Json(json!({
        "principal": principal_snapshot(&state, principal.id.as_uuid()).await?,
        "created": created,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TombstoneRequest {
    /// The human operator who decided the tombstone (audited as the actor).
    operator: String,
}

async fn tombstone_principal(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(principal_id): Path<Uuid>,
    Json(body): Json<TombstoneRequest>,
) -> AccessResult<impl IntoResponse> {
    caller.require_permission(InternalPermission::PlatformPrincipalRegister)?;
    // Existence first (NOT_FOUND), then the named operator.
    principal_snapshot(&state, principal_id).await?;
    let op = OperatorContext::new(&body.operator)
        .map_err(|_| AccessError::Refused(ErrorCode::OperatorRequired))?;
    let result = state
        .platform
        .operator_tombstone_principal(&op, PrincipalId::from_uuid(principal_id))
        .await
        .map_err(AccessError::service)?;
    Ok(Json(json!({
        "alreadyTombstoned": result.already_tombstoned,
        "principal": principal_snapshot(&state, principal_id).await?,
    })))
}
