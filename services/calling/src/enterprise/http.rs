//! Enterprise administration and user API (`/v1/enterprise/**`, Part F §34, §144).
//!
//! Every route runs behind platform credential authentication (`routes::app`); scope
//! comes only from the authenticated context. Reads need `enterprise.config.read`,
//! configuration changes `enterprise.config.write` (voicemail / recording routes their own
//! scopes) — never implied by `calls.*`. Bodies refuse unknown fields; every mutation of
//! an existing resource carries `expectedVersion` (body, or query for DELETE) and a stale
//! one is 409 VERSION_CONFLICT. A resource of another scope is 404.

use axum::{
    extract::{
        rejection::JsonRejection, rejection::QueryRejection, DefaultBodyLimit, Path, Query, State,
    },
    http::StatusCode,
    response::Response,
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    departments::{DepartmentChange, DepartmentMember, DepartmentRole, NewDepartment},
    destinations::{Destination, DestinationType},
    extensions::{ExtensionChange, ExtensionFilter, NewExtension},
    policy::{NewPolicy, PolicyChange, PolicyRules, ResourceStatus},
    AdminActor, PageRequest,
};
use crate::{
    calls::{
        app::http::{parse_body, respond, uuid_path},
        command::CallScope,
    },
    error::CallingError,
    platform::{context::PlatformAuthContext, scopes::names},
    state::AppState,
};

const MAX_BODY_BYTES: usize = 64 * 1024;

/// The enterprise routes (mounted behind platform credential authentication).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/enterprise/policies",
            get(list_policies).post(create_policy),
        )
        .route(
            "/v1/enterprise/policies/:policy_id",
            get(get_policy).patch(update_policy).delete(disable_policy),
        )
        .route("/v1/enterprise/effective-policy", get(effective_policy))
        .route(
            "/v1/enterprise/departments",
            get(list_departments).post(create_department),
        )
        .route(
            "/v1/enterprise/departments/:department_id",
            get(get_department)
                .patch(update_department)
                .delete(disable_department),
        )
        .route(
            "/v1/enterprise/departments/:department_id/members",
            put(replace_department_members),
        )
        .route("/v1/enterprise/audit", get(list_audit))
        .route(
            "/v1/enterprise/extensions",
            get(list_extensions).post(create_extension),
        )
        .route(
            "/v1/enterprise/extensions/:extension_id",
            get(get_extension)
                .patch(update_extension)
                .delete(disable_extension),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// Bounded query parameters; unknown or repeated ones are refused.
pub(crate) struct QueryParams(Vec<(String, String)>);

impl QueryParams {
    pub(crate) fn parse(
        query: Result<Query<Vec<(String, String)>>, QueryRejection>,
        allowed: &[&str],
    ) -> Result<Self, CallingError> {
        let Query(pairs) =
            query.map_err(|_| CallingError::Validation("The query string is invalid."))?;
        let mut seen: Vec<&str> = Vec::new();
        for (key, _) in &pairs {
            if !allowed.contains(&key.as_str()) {
                return Err(CallingError::Validation("Unknown query parameter."));
            }
            if seen.contains(&key.as_str()) {
                return Err(CallingError::Validation("A query parameter is repeated."));
            }
            seen.push(key);
        }
        Ok(Self(pairs))
    }

    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn page(&self) -> Result<PageRequest, CallingError> {
        Ok(PageRequest {
            limit: self
                .get("limit")
                .map(|v| {
                    v.parse::<u64>().map_err(|_| {
                        CallingError::Validation("limit must be an integer between 1 and 100.")
                    })
                })
                .transpose()?,
            cursor: self.get("cursor").map(str::to_string),
        })
    }

    pub(crate) fn status(&self) -> Result<Option<ResourceStatus>, CallingError> {
        self.get("status")
            .map(|v| {
                ResourceStatus::parse(v)
                    .ok_or(CallingError::Validation("The status filter is invalid."))
            })
            .transpose()
    }

    pub(crate) fn uuid(&self, key: &str) -> Result<Option<Uuid>, CallingError> {
        self.get(key)
            .map(|v| {
                Uuid::parse_str(v).map_err(|_| CallingError::Validation("An id filter is invalid."))
            })
            .transpose()
    }

    /// `expectedVersion` (required).
    pub(crate) fn expected_version(&self) -> Result<i64, CallingError> {
        self.get("expectedVersion")
            .and_then(|v| v.parse::<i64>().ok())
            .ok_or(CallingError::Validation(
                "expectedVersion is required and must be an integer.",
            ))
    }
}

pub(crate) fn ok<T: Serialize>(body: T) -> Response {
    respond(StatusCode::OK, body, false)
}

pub(crate) fn created<T: Serialize>(body: T) -> Response {
    respond(StatusCode::CREATED, body, false)
}

// ---------------------------------------------------------------------------
// Policies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreatePolicyBody {
    name: String,
    #[serde(default)]
    is_default: bool,
    rules: PolicyRules,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdatePolicyBody {
    expected_version: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    is_default: Option<bool>,
    #[serde(default)]
    rules: Option<PolicyRules>,
}

async fn create_policy(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreatePolicyBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_policy(
            &AdminActor::of(&ctx),
            NewPolicy {
                name: body.name,
                is_default: body.is_default,
                rules: body.rules,
            },
        )
        .await?;
    Ok(created(view))
}

async fn list_policies(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_policies(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_policy(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_policy(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn effective_policy(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .effective_default_policy(CallScope::of_platform(&ctx))
        .await?;
    Ok(ok(view))
}

async fn update_policy(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<UpdatePolicyBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .update_policy(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            PolicyChange {
                name: body.name,
                is_default: body.is_default,
                rules: body.rules,
            },
        )
        .await?;
    Ok(ok(view))
}

async fn disable_policy(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["expectedVersion"])?;
    let view = state
        .enterprise
        .disable_policy(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

// ---------------------------------------------------------------------------
// Departments
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateDepartmentBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateDepartmentBody {
    expected_version: i64,
    #[serde(default)]
    name: Option<String>,
    /// Present (even `null`) = replace the description.
    #[serde(default, with = "double_option")]
    description: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemberBody {
    principal_id: Uuid,
    #[serde(default = "default_role")]
    role: DepartmentRole,
}

fn default_role() -> DepartmentRole {
    DepartmentRole::Member
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplaceMembersBody {
    expected_version: i64,
    members: Vec<MemberBody>,
}

/// `Option<Option<T>>` that distinguishes an absent field from an explicit `null`.
pub(crate) mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }
}

async fn create_department(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateDepartmentBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_department(
            &AdminActor::of(&ctx),
            NewDepartment {
                name: body.name,
                description: body.description,
            },
        )
        .await?;
    Ok(created(view))
}

async fn list_departments(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_departments(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_department(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_department(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn update_department(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<UpdateDepartmentBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .update_department(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            DepartmentChange {
                name: body.name,
                description: body.description,
            },
        )
        .await?;
    Ok(ok(view))
}

async fn replace_department_members(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<ReplaceMembersBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .replace_department_members(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            body.members
                .into_iter()
                .map(|m| DepartmentMember {
                    principal_id: m.principal_id,
                    role: m.role,
                })
                .collect(),
        )
        .await?;
    Ok(ok(view))
}

async fn disable_department(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["expectedVersion"])?;
    let view = state
        .enterprise
        .disable_department(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

async fn list_audit(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "resourceId"])?;
    let page = state
        .enterprise
        .audit_records(
            CallScope::of_platform(&ctx),
            q.uuid("resourceId")?,
            &q.page()?,
        )
        .await?;
    Ok(ok(page))
}

// ---------------------------------------------------------------------------
// Extensions (§54)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateExtensionBody {
    extension_number: String,
    display_label: String,
    destination: Destination,
    #[serde(default)]
    policy_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateExtensionBody {
    expected_version: i64,
    #[serde(default)]
    extension_number: Option<String>,
    #[serde(default)]
    display_label: Option<String>,
    #[serde(default)]
    destination: Option<Destination>,
    /// Present (even `null`) = replace the named policy (`null`: the environment default).
    #[serde(default, with = "double_option")]
    policy_id: Option<Option<Uuid>>,
}

async fn create_extension(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateExtensionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_extension(
            &AdminActor::of(&ctx),
            NewExtension {
                extension_number: body.extension_number,
                display_label: body.display_label,
                destination: body.destination,
                policy_id: body.policy_id,
            },
        )
        .await?;
    Ok(created(view))
}

async fn list_extensions(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status", "destinationType"])?;
    let filter = ExtensionFilter {
        status: q.status()?,
        destination_type: q
            .get("destinationType")
            .map(|v| {
                DestinationType::parse(v).ok_or(CallingError::Validation(
                    "The destination type filter is invalid.",
                ))
            })
            .transpose()?,
    };
    let page = state
        .enterprise
        .list_extensions(CallScope::of_platform(&ctx), &filter, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_extension(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_extension(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn update_extension(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<UpdateExtensionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .update_extension(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            ExtensionChange {
                extension_number: body.extension_number,
                display_label: body.display_label,
                destination: body.destination,
                policy_id: body.policy_id,
            },
        )
        .await?;
    Ok(ok(view))
}

async fn disable_extension(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["expectedVersion"])?;
    let view = state
        .enterprise
        .disable_extension(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}
