//! The admin bridge's identity commands (ADR 0068, design §6.8 and §17.6):
//! `POST /internal/v1/operator/*` and the settlement read
//! `GET /internal/v1/operator/operations/{idempotencyKey}`.
//!
//! Reachable only with `platform-identity.administer` AND the admin bridge's strict HQ
//! operator attribution (`X-CF-Operator: hq:<staff>`, parsed by the middleware); the
//! operator console keeps its own `/internal/v1/admin/*` routes. Every command:
//!
//! * requires `Idempotency-Key` (a UUID; missing or malformed: 400 `VALIDATION_ERROR`);
//! * is R3 / R4, so it requires the second person (`X-CF-Approver`, `X-CF-Approval-Ref`,
//!   `X-CF-Intent-Digest`; missing: 422 `POLICY_VIOLATION`, nothing done);
//! * takes a camelCase body without unknown fields, carrying `expectedVersion` (integer
//!   ≥ 1) whenever its subject is versioned; another version is 409 `VERSION_CONFLICT`;
//! * runs the same audited domain operation as the console and records itself in
//!   `platform_access_admin_operations` in the same transaction
//!   ([`crate::identity::bridge`]); a repeated key for the same command answers again
//!   without applying, the same key for anything else is 409 `CONFLICT`;
//! * answers `{status, version, resource}`: the resulting state (UPPER_SNAKE), the
//!   resulting version as a string (null when unversioned) and a small closed resource
//!   (ids, codes, states, timestamps — never a secret, verifier, external reference,
//!   display name or free text).
//!
//! API keys are never ISSUED or ROTATED here: Platform Access hands a new secret out only
//! in the response of that request, and HQ staff must never see a partner secret.

use std::future::Future;

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use sea_orm::DatabaseConnection;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::error::{AccessError, AccessResult, ErrorCode};
use super::middleware::InternalCaller;
use crate::{
    domain::{
        ApplicationId, ApplicationStatus, CredentialId, EnvironmentId, EnvironmentKind,
        EnvironmentStatus, PrincipalId, PrincipalKind, PrincipalStatus, ServiceAccountId,
        ServiceAccountStatus, TenantId, TenantStatus,
    },
    identity::{
        bridge::{self, BridgeCommand, Subject},
        end_users::{self, EndUserError, EndUserScope},
        environment_origins::MAX_ORIGIN_LEN,
        operator::OperatorContext,
        principals::NewPrincipal,
        scopes::ScopeTrust,
        PlatformError, PlatformResult,
    },
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/operator/tenants/:tenant_id/status", post(tenant_status))
        .route(
            "/operator/applications/:application_id/status",
            post(application_status),
        )
        .route(
            "/operator/environments/:environment_id/status",
            post(environment_status),
        )
        .route(
            "/operator/service-accounts/:service_account_id/status",
            post(service_account_status),
        )
        .route(
            "/operator/applications/:application_id/scope-grants",
            post(scope_grant),
        )
        .route(
            "/operator/applications/:application_id/privileged-scope-grants",
            post(privileged_scope_grant),
        )
        .route(
            "/operator/credentials/:credential_id/revoke",
            post(revoke_credential),
        )
        .route(
            "/operator/credentials/:credential_id/suspend",
            post(suspend_credential),
        )
        .route(
            "/operator/credentials/:credential_id/resume",
            post(resume_credential),
        )
        .route(
            "/operator/environments/:environment_id/principals",
            post(register_principal),
        )
        .route(
            "/operator/principals/:principal_id/status",
            post(principal_status),
        )
        .route(
            "/operator/principals/:principal_id/remove",
            post(remove_principal),
        )
        .route(
            "/operator/principals/:principal_id/sign-out",
            post(sign_out),
        )
        .route(
            "/operator/app-grants/:grant_id/revoke",
            post(revoke_app_grant),
        )
        .route(
            "/operator/environments/:environment_id/origins/remove",
            post(remove_origin),
        )
        .route(
            "/operator/operations/:idempotency_key",
            get(operation_record),
        )
}

/// Capability ids (HQ's catalogue) recorded as the command.
pub mod capability {
    pub const STATUS_CHANGE: &str = "identity.status.change";
    pub const SCOPE_GRANT_CHANGE: &str = "identity.scope-grant.change";
    pub const PRIVILEGED_SCOPE_GRANT_CHANGE: &str = "identity.privileged-scope-grant.change";
    pub const KEY_REVOKE: &str = "identity.key.revoke";
    pub const KEY_SUSPEND: &str = "identity.key.suspend";
    pub const KEY_RESUME: &str = "identity.key.resume";
    pub const RECORD_REGISTER: &str = "identity.record.register";
    pub const RECORD_STATUS_CHANGE: &str = "identity.record.status.change";
    pub const RECORD_REMOVE: &str = "identity.record.remove";
    pub const APP_GRANT_REVOKE: &str = "identity.app-grant.revoke";
    pub const WEBSITE_REMOVE: &str = "partners.website.remove";
    pub const USER_SIGN_OUT: &str = "partners.user.sign-out";
}

fn op_err(e: PlatformError) -> AccessError {
    AccessError::operator(e)
}

fn invalid(message: &str) -> AccessError {
    AccessError::Operator(ErrorCode::ValidationError, message.to_string())
}

/// What a command is about, before the caller's checks.
struct Target {
    capability: &'static str,
    subject: Subject,
    id: Uuid,
    detail: Option<String>,
    /// `Some` for a versioned subject (the body's `expectedVersion`).
    expected_version: Option<i64>,
}

impl Target {
    fn versioned(capability: &'static str, subject: Subject, id: Uuid, version: i64) -> Self {
        Self {
            capability,
            subject,
            id,
            detail: None,
            expected_version: Some(version),
        }
    }

    fn unversioned(capability: &'static str, subject: Subject, id: Uuid) -> Self {
        Self {
            capability,
            subject,
            id,
            detail: None,
            expected_version: None,
        }
    }

    fn detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// The verified bridge command: permission, the bridge's HQ operator, the
/// `Idempotency-Key`, a positive expected version for a versioned subject and the second
/// person (every command here is R3 / R4).
fn command(caller: &InternalCaller, headers: &HeaderMap, t: Target) -> AccessResult<BridgeCommand> {
    caller.require_permission(InternalPermission::PlatformIdentityAdminister)?;
    let attribution = caller
        .bridge_operator()
        .ok_or(AccessError::Refused(ErrorCode::OperatorRequired))?;
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v.trim()).ok())
        .ok_or_else(|| invalid("Idempotency-Key (a UUID) is required"))?;
    if t.expected_version.is_some_and(|v| v < 1) {
        return Err(invalid("expectedVersion must be at least 1"));
    }
    let approval = attribution.approval.as_ref().ok_or_else(|| {
        AccessError::Operator(
            ErrorCode::PolicyViolation,
            "this command requires a second-person approval (X-CF-Approver)".to_string(),
        )
    })?;
    Ok(BridgeCommand {
        key,
        command: t.capability.to_string(),
        operator: attribution.operator.as_str().to_string(),
        approval: Some((
            approval.approver.as_str().to_string(),
            approval.approval_ref.clone(),
        )),
        subject: t.subject,
        subject_id: t.id,
        subject_detail: t.detail,
        expected_version: t.expected_version,
    })
}

/// Applies `cmd` once and answers `{status, version, resource}`. A key already applied to
/// the same command and subject is answered without applying again (also when a
/// concurrent duplicate won the race to the record).
async fn run<F, Fut>(
    db: DatabaseConnection,
    cmd: BridgeCommand,
    apply: F,
) -> AccessResult<Json<Value>>
where
    F: FnOnce(OperatorContext) -> Fut,
    Fut: Future<Output = AccessResult<()>>,
{
    let db = &db;
    if !bridge::replayed(db, &cmd).await.map_err(op_err)? {
        if let Err(e) = apply(OperatorContext::for_bridge(cmd.clone())).await {
            if !matches!(e.code(), ErrorCode::Conflict)
                || !bridge::replayed(db, &cmd).await.unwrap_or(false)
            {
                return Err(e);
            }
        }
    }
    let answer = bridge::answer(db, cmd.key)
        .await
        .map_err(op_err)?
        .ok_or_else(|| {
            AccessError::operator(PlatformError::Database {
                summary: "applied bridge command without a record".to_string(),
                connectivity: false,
            })
        })?;
    Ok(Json(answer))
}

fn done<T>(r: PlatformResult<T>) -> AccessResult<()> {
    r.map(|_| ()).map_err(op_err)
}

// ---------------------------------------------------------------------------
// identity.status.change (R3): tenant / application / environment / service account
// (`partners.service-account.suspend` is the service-account route with SUSPENDED)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatusBody<T> {
    expected_version: i64,
    status: T,
}

async fn tenant_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<TenantStatus>>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::STATUS_CHANGE,
        Subject::Tenant,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_set_tenant_status(&op, TenantId::from_uuid(id), b.status)
                .await,
        )
    })
    .await
}

async fn application_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<ApplicationStatus>>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::STATUS_CHANGE,
        Subject::Application,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_set_application_status(&op, ApplicationId::from_uuid(id), b.status)
                .await,
        )
    })
    .await
}

async fn environment_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<EnvironmentStatus>>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::STATUS_CHANGE,
        Subject::Environment,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_set_environment_status(&op, EnvironmentId::from_uuid(id), b.status)
                .await,
        )
    })
    .await
}

async fn service_account_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<ServiceAccountStatus>>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::STATUS_CHANGE,
        Subject::ServiceAccount,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_set_service_account_status(&op, ServiceAccountId::from_uuid(id), b.status)
                .await,
        )
    })
    .await
}

// ---------------------------------------------------------------------------
// identity.scope-grant.change (R3) / identity.privileged-scope-grant.change (R4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ScopeAction {
    Grant,
    Revoke,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScopeGrantBody {
    action: ScopeAction,
    scope: String,
    /// GRANT only (omitted: every environment kind).
    #[serde(default)]
    environment_kinds: Option<Vec<EnvironmentKind>>,
}

async fn scope_grant(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<ScopeGrantBody>,
) -> AccessResult<impl IntoResponse> {
    change_scope(s, caller, headers, id, b, false).await
}

async fn privileged_scope_grant(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<ScopeGrantBody>,
) -> AccessResult<impl IntoResponse> {
    change_scope(s, caller, headers, id, b, true).await
}

/// Scope grants are unversioned (an application scope row has no revision): GRANT sets
/// the grant, REVOKE removes it; both answer `GRANTED` / `REVOKED` with version null. A
/// privileged operator scope (`numbers.admin`) changes only through the privileged
/// route, every other scope only through the normal one.
async fn change_scope(
    s: AppState,
    caller: InternalCaller,
    headers: HeaderMap,
    id: Uuid,
    b: ScopeGrantBody,
    privileged: bool,
) -> AccessResult<Json<Value>> {
    let capability = if privileged {
        capability::PRIVILEGED_SCOPE_GRANT_CHANGE
    } else {
        capability::SCOPE_GRANT_CHANGE
    };
    let t = Target::unversioned(capability, Subject::ApplicationScope, id).detail(b.scope.clone());
    let cmd = command(&caller, &headers, t)?;
    let definition = s
        .platform
        .registry()
        .definition(&b.scope)
        .ok_or_else(|| invalid("scope is not a registered scope"))?;
    if (definition.trust == ScopeTrust::PrivilegedOperator) != privileged {
        return Err(AccessError::Operator(
            ErrorCode::PolicyViolation,
            if privileged {
                "only a privileged operator scope changes through privileged-scope-grants"
            } else {
                "a privileged operator scope changes only through privileged-scope-grants"
            }
            .to_string(),
        ));
    }
    if b.action == ScopeAction::Revoke && b.environment_kinds.is_some() {
        return Err(invalid("environmentKinds applies to GRANT only"));
    }
    run(s.platform.db().clone(), cmd, |op| async move {
        let app = ApplicationId::from_uuid(id);
        let kinds = b.environment_kinds.as_deref();
        match (b.action, privileged) {
            (ScopeAction::Grant, false) => done(
                s.platform
                    .operator_grant_application_scope(&op, app, &b.scope, kinds)
                    .await,
            ),
            (ScopeAction::Grant, true) => done(
                s.platform
                    .operator_grant_privileged_scope(&op, app, &b.scope, kinds)
                    .await,
            ),
            (ScopeAction::Revoke, _) => done(
                s.platform
                    .operator_revoke_application_scope(&op, app, &b.scope)
                    .await,
            ),
        }
    })
    .await
}

// ---------------------------------------------------------------------------
// identity.key.revoke / suspend / resume (R4; partners.key.revoke = revoke)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionBody {
    expected_version: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SuspendBody {
    expected_version: i64,
    reason_code: String,
}

async fn revoke_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<VersionBody>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::KEY_REVOKE,
        Subject::Credential,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_revoke_credential(&op, CredentialId::from_uuid(id))
                .await,
        )
    })
    .await
}

async fn suspend_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<SuspendBody>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::KEY_SUSPEND,
        Subject::Credential,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_suspend_credential(&op, CredentialId::from_uuid(id), &b.reason_code)
                .await,
        )
    })
    .await
}

async fn resume_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<VersionBody>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::KEY_RESUME,
        Subject::Credential,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_resume_credential(&op, CredentialId::from_uuid(id))
                .await,
        )
    })
    .await
}

// ---------------------------------------------------------------------------
// identity.record.register / status.change / remove (R4)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisterBody {
    #[serde(default)]
    kind: Option<PrincipalKind>,
    #[serde(default)]
    external_reference: Option<String>,
    #[serde(default)]
    service_account_id: Option<Uuid>,
}

/// Unversioned (a new record): registration is idempotent on the environment and the
/// external reference (or service account); answers `REGISTERED` with the principal's
/// version. The external reference is never echoed or recorded.
async fn register_principal(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
    Json(b): Json<RegisterBody>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::unversioned(
        capability::RECORD_REGISTER,
        Subject::PrincipalRegistration,
        environment_id,
    );
    let cmd = command(&caller, &headers, t)?;
    let new = match (b.kind, b.external_reference, b.service_account_id) {
        (Some(kind), Some(external_reference), None) => NewPrincipal::External {
            kind,
            external_reference,
        },
        (None, None, Some(sa)) => NewPrincipal::ServiceAccount(ServiceAccountId::from_uuid(sa)),
        _ => {
            return Err(invalid(
                "give either kind + externalReference or serviceAccountId",
            ))
        }
    };
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_register_principal(&op, EnvironmentId::from_uuid(environment_id), new)
                .await,
        )
    })
    .await
}

async fn principal_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<PrincipalStatus>>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::RECORD_STATUS_CHANGE,
        Subject::Principal,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_set_principal_status(&op, PrincipalId::from_uuid(id), b.status)
                .await,
        )
    })
    .await
}

/// The existing tombstone: DEACTIVATED, the external reference replaced by an opaque
/// tombstone reference, irreversible; answers `TOMBSTONED`.
async fn remove_principal(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<VersionBody>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::RECORD_REMOVE,
        Subject::Principal,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    run(s.platform.db().clone(), cmd, |op| async move {
        done(
            s.platform
                .operator_tombstone_principal(&op, PrincipalId::from_uuid(id))
                .await,
        )
    })
    .await
}

// ---------------------------------------------------------------------------
// partners.user.sign-out (R3): a partner user's sessions
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyBody {}

/// Unversioned (the session epoch only moves forward): every user token issued so far
/// stops verifying; answers `SIGNED_OUT`. Only an EXTERNAL_APPLICATION_PRINCIPAL that is
/// not tombstoned; behind PLATFORM_ACCESS_END_USERS_ENABLED (off: 404).
async fn sign_out(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(_): Json<EmptyBody>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::unversioned(capability::USER_SIGN_OUT, Subject::PrincipalSessions, id);
    let cmd = command(&caller, &headers, t)?;
    if !s.build.end_users_enabled {
        return Err(AccessError::Refused(ErrorCode::NotFound));
    }
    run(s.platform.db().clone(), cmd, |op| async move {
        let db = s.platform.db();
        let user = end_users::load(db, id)
            .await
            .map_err(|e| op_err(e.into()))?
            .filter(|u| u.kind == PrincipalKind::ExternalApplicationPrincipal && !u.tombstoned)
            .ok_or(AccessError::Refused(ErrorCode::NotFound))?;
        let environment = s
            .platform
            .environment(EnvironmentId::from_uuid(user.environment_id))
            .await
            .map_err(op_err)?;
        let scope = EndUserScope {
            tenant_id: user.tenant_id,
            application_id: user.application_id,
            environment_id: user.environment_id,
            kind: environment.kind,
        };
        s.platform
            .revoke_end_user_sessions(&op, &scope, id)
            .await
            .map(|_| ())
            .map_err(|e| match e {
                EndUserError::Platform(e) => op_err(e),
                EndUserError::LimitExceeded => AccessError::Refused(ErrorCode::LimitExceeded),
            })
    })
    .await
}

// ---------------------------------------------------------------------------
// identity.app-grant.revoke (R3): a CF Meet Apps grant
// ---------------------------------------------------------------------------

/// Answers `REVOKED`; behind PLATFORM_ACCESS_OAUTH_ENABLED (off: 404).
async fn revoke_app_grant(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<VersionBody>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::APP_GRANT_REVOKE,
        Subject::AppGrant,
        id,
        b.expected_version,
    );
    let cmd = command(&caller, &headers, t)?;
    if !s.build.oauth_enabled {
        return Err(AccessError::Refused(ErrorCode::NotFound));
    }
    run(s.platform.db().clone(), cmd, |op| async move {
        done(s.platform.operator_revoke_oauth_grant(&op, id).await)
    })
    .await
}

// ---------------------------------------------------------------------------
// partners.website.remove (R3): one allowed website of an environment
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveOriginBody {
    expected_version: i64,
    origin: String,
}

/// `expectedVersion` is the environment's allowed-websites revision (the `revision` of
/// `GET /operator/tenants/{tenantId}/origins`); answers `REMOVED`. Behind
/// PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED (off: 404).
async fn remove_origin(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
    Json(b): Json<RemoveOriginBody>,
) -> AccessResult<impl IntoResponse> {
    let t = Target::versioned(
        capability::WEBSITE_REMOVE,
        Subject::EnvironmentOrigins,
        environment_id,
        b.expected_version,
    )
    .detail(b.origin.clone());
    let cmd = command(&caller, &headers, t)?;
    if b.origin.is_empty() || b.origin.len() > MAX_ORIGIN_LEN {
        return Err(invalid("origin must be 1 to 270 characters"));
    }
    if !s.build.environment_origins_enabled {
        return Err(AccessError::Refused(ErrorCode::NotFound));
    }
    run(s.platform.db().clone(), cmd, |op| async move {
        let environment = s
            .platform
            .environment(EnvironmentId::from_uuid(environment_id))
            .await
            .map_err(op_err)?;
        done(
            s.platform
                .remove_environment_origin(&op, &environment, &b.origin)
                .await,
        )
    })
    .await
}

// ---------------------------------------------------------------------------
// The settlement read
// ---------------------------------------------------------------------------

/// `GET /operator/operations/{idempotencyKey}`: `{idempotency_key, outcome: APPLIED,
/// source_status, source_version}` of an applied bridge command — the same status and
/// version the command answered. 404 for an unknown or malformed key and for every caller
/// other than the bridge with its HQ operator.
async fn operation_record(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(key): Path<String>,
) -> AccessResult<impl IntoResponse> {
    let not_found = AccessError::Refused(ErrorCode::NotFound);
    if caller
        .require_permission(InternalPermission::PlatformIdentityAdminister)
        .is_err()
        || caller.bridge_operator().is_none()
    {
        return Err(not_found);
    }
    let key = Uuid::parse_str(&key).map_err(|_| AccessError::Refused(ErrorCode::NotFound))?;
    let record = bridge::operation_record(s.platform.db(), key)
        .await
        .map_err(op_err)?
        .ok_or(not_found)?;
    Ok(Json(record))
}
