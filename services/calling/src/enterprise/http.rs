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
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    departments::{DepartmentChange, DepartmentMember, DepartmentRole, NewDepartment},
    destinations::{Destination, DestinationType},
    extensions::{ExtensionChange, ExtensionFilter, NewExtension},
    forwarding::ForwardingCondition,
    hours::{HoursException, HoursSettings, WeeklyWindow},
    ivr::IvrGraph,
    policy::{NewPolicy, PolicyChange, PolicyRules, ResourceStatus},
    queues::{QueueChange, QueueMember, QueueSettings},
    recording::{
        CallRecordingView, ConsentDecision, ConsentParticipants, RecordingMode,
        RecordingPolicyChange, RecordingPolicySettings, RecordingRequester,
    },
    ring_groups::{RingGroupChange, RingGroupSettings, RingStrategy},
    voicemail::{
        MailboxChange, MailboxOwnerType, MailboxSettings, MessageFilter, MessageState, ReadState,
        VoicemailRequester,
    },
    AdminActor, PageRequest,
};
use crate::{
    calls::{
        app::{
            enterprise::{RecordingActor, StartOutcome},
            http::{parse_body, respond, uuid_path},
        },
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
            "/v1/enterprise/ring-groups",
            get(list_ring_groups).post(create_ring_group),
        )
        .route(
            "/v1/enterprise/ring-groups/:ring_group_id",
            get(get_ring_group)
                .patch(update_ring_group)
                .delete(disable_ring_group),
        )
        .route(
            "/v1/enterprise/ring-groups/:ring_group_id/members",
            put(replace_ring_group_members),
        )
        .route("/v1/enterprise/queues", get(list_queues).post(create_queue))
        .route(
            "/v1/enterprise/queues/:queue_id",
            get(get_queue).patch(update_queue).delete(disable_queue),
        )
        .route(
            "/v1/enterprise/queues/:queue_id/members",
            put(replace_queue_members),
        )
        .route(
            "/v1/enterprise/queues/:queue_id/entries",
            get(list_queue_entries),
        )
        .route(
            "/v1/enterprise/ivr-flows",
            get(list_ivr_flows).post(create_ivr_flow),
        )
        .route(
            "/v1/enterprise/ivr-flows/:ivr_flow_id",
            get(get_ivr_flow)
                .patch(rename_ivr_flow)
                .delete(disable_ivr_flow),
        )
        .route(
            "/v1/enterprise/ivr-flows/:ivr_flow_id/versions",
            get(list_ivr_versions).post(publish_ivr_version),
        )
        .route(
            "/v1/enterprise/ivr-flows/:ivr_flow_id/versions/:version_number",
            get(get_ivr_version),
        )
        .route(
            "/v1/enterprise/ivr-flows/:ivr_flow_id/activation",
            put(activate_ivr_version),
        )
        .route(
            "/v1/enterprise/sip-devices",
            get(list_sip_devices).post(bind_sip_device),
        )
        .route(
            "/v1/enterprise/sip-devices/:sip_device_id",
            get(get_sip_device).delete(disable_sip_device),
        )
        .route(
            "/v1/enterprise/extensions/:extension_id",
            get(get_extension)
                .patch(update_extension)
                .delete(disable_extension),
        )
        .route(
            "/v1/enterprise/business-hours",
            get(list_business_hours).post(create_business_hours),
        )
        .route(
            "/v1/enterprise/business-hours/:business_hours_id",
            get(get_business_hours)
                .put(replace_business_hours)
                .delete(disable_business_hours),
        )
        .route(
            "/v1/enterprise/business-hours/:business_hours_id/evaluation",
            post(evaluate_business_hours),
        )
        .route(
            "/v1/enterprise/public-contacts",
            get(list_public_contacts).post(create_public_contact),
        )
        .route(
            "/v1/enterprise/public-contacts/:public_contact_id",
            get(get_public_contact).delete(disable_public_contact),
        )
        .route(
            "/v1/enterprise/forwarding-rules",
            get(list_forwarding_rules).post(create_forwarding_rule),
        )
        .route(
            "/v1/enterprise/forwarding-rules/:forwarding_rule_id",
            get(get_forwarding_rule)
                .patch(update_forwarding_rule)
                .delete(disable_forwarding_rule),
        )
        .route(
            "/v1/enterprise/voicemail/mailboxes",
            get(list_mailboxes).post(create_mailbox),
        )
        .route(
            "/v1/enterprise/voicemail/mailboxes/:mailbox_id",
            get(get_mailbox)
                .patch(update_mailbox)
                .delete(disable_mailbox),
        )
        .route("/v1/enterprise/voicemail/messages", get(list_messages))
        .route(
            "/v1/enterprise/voicemail/messages/:message_id",
            get(get_message).delete(delete_message),
        )
        .route(
            "/v1/enterprise/voicemail/messages/:message_id/read-state",
            put(set_read_state),
        )
        .route(
            "/v1/enterprise/voicemail/messages/:message_id/media-access",
            post(media_access),
        )
        .route(
            "/v1/enterprise/recording-policies",
            get(list_recording_policies).post(create_recording_policy),
        )
        .route(
            "/v1/enterprise/recording-policies/:recording_policy_id",
            get(get_recording_policy)
                .patch(update_recording_policy)
                .delete(disable_recording_policy),
        )
        .route(
            "/v1/calls/:call_id/recording-consent",
            post(record_recording_consent),
        )
        .route(
            "/v1/calls/:call_id/recordings",
            get(list_call_recordings).post(start_recording),
        )
        .route(
            "/v1/calls/:call_id/recordings/:recording_id/stop",
            post(stop_recording),
        )
        .route(
            "/v1/calls/:call_id/recording-authorizations",
            get(list_recording_authorizations),
        )
        .route(
            "/v1/enterprise/recordings/:recording_id",
            get(get_call_recording).delete(delete_call_recording),
        )
        .route(
            "/v1/enterprise/recordings/:recording_id/media-access",
            post(recording_media_access),
        )
        .route(
            "/v1/enterprise/recordings/:recording_id/access-audit",
            get(recording_access_audit),
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

// ---------------------------------------------------------------------------
// Ring groups (§55)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateRingGroupBody {
    name: String,
    strategy: RingStrategy,
    member_ring_seconds: i32,
    group_timeout_seconds: i32,
    #[serde(default)]
    fallback: Option<Destination>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateRingGroupBody {
    expected_version: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    strategy: Option<RingStrategy>,
    #[serde(default)]
    member_ring_seconds: Option<i32>,
    #[serde(default)]
    group_timeout_seconds: Option<i32>,
    /// Present (even `null`) = replace the fallback (`null` removes it).
    #[serde(default, with = "double_option")]
    fallback: Option<Option<Destination>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplaceRingGroupMembersBody {
    expected_version: i64,
    /// In ring order.
    members: Vec<Destination>,
}

async fn create_ring_group(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateRingGroupBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_ring_group(
            &AdminActor::of(&ctx),
            RingGroupSettings {
                name: body.name,
                strategy: body.strategy,
                member_ring_seconds: body.member_ring_seconds,
                group_timeout_seconds: body.group_timeout_seconds,
                fallback: body.fallback,
            },
        )
        .await?;
    Ok(created(view))
}

async fn list_ring_groups(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_ring_groups(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_ring_group(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_ring_group(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn update_ring_group(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<UpdateRingGroupBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .update_ring_group(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            RingGroupChange {
                name: body.name,
                strategy: body.strategy,
                member_ring_seconds: body.member_ring_seconds,
                group_timeout_seconds: body.group_timeout_seconds,
                fallback: body.fallback,
            },
        )
        .await?;
    Ok(ok(view))
}

async fn replace_ring_group_members(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<ReplaceRingGroupMembersBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .replace_ring_group_members(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            body.members,
        )
        .await?;
    Ok(ok(view))
}

async fn disable_ring_group(
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
        .disable_ring_group(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

// ---------------------------------------------------------------------------
// Call queues (§56)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateQueueBody {
    name: String,
    max_wait_seconds: i32,
    offer_timeout_seconds: i32,
    capacity: i32,
    #[serde(default)]
    overflow: Option<Destination>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateQueueBody {
    expected_version: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    max_wait_seconds: Option<i32>,
    #[serde(default)]
    offer_timeout_seconds: Option<i32>,
    #[serde(default)]
    capacity: Option<i32>,
    /// Present (even `null`) = replace the overflow (`null` removes it).
    #[serde(default, with = "double_option")]
    overflow: Option<Option<Destination>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplaceQueueMembersBody {
    expected_version: i64,
    members: Vec<QueueMember>,
}

async fn create_queue(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateQueueBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_queue(
            &AdminActor::of(&ctx),
            QueueSettings {
                name: body.name,
                max_wait_seconds: body.max_wait_seconds,
                offer_timeout_seconds: body.offer_timeout_seconds,
                capacity: body.capacity,
                overflow: body.overflow,
            },
        )
        .await?;
    Ok(created(view))
}

async fn list_queues(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_queues(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_queue(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_queue(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn update_queue(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<UpdateQueueBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .update_queue(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            QueueChange {
                name: body.name,
                max_wait_seconds: body.max_wait_seconds,
                offer_timeout_seconds: body.offer_timeout_seconds,
                capacity: body.capacity,
                overflow: body.overflow,
            },
        )
        .await?;
    Ok(ok(view))
}

async fn replace_queue_members(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<ReplaceQueueMembersBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .replace_queue_members(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            body.members,
        )
        .await?;
    // New or resumed agents may take waiting Calls right away.
    state.app.dispatch_queue(id).await;
    Ok(ok(view))
}

async fn disable_queue(
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
        .disable_queue(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

async fn list_queue_entries(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["state", "limit"])?;
    let active_only = match q.get("state") {
        None | Some("ACTIVE") => true,
        Some("FINISHED") => false,
        Some(_) => {
            return Err(CallingError::Validation(
                "state must be ACTIVE or FINISHED.",
            ))
        }
    };
    let limit = q.page()?.limit().map_err(CallingError::from)?;
    let page = state
        .enterprise
        .list_queue_entries(CallScope::of_platform(&ctx), id, active_only, limit)
        .await?;
    Ok(ok(page))
}

// ---------------------------------------------------------------------------
// IVR flows (§57)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateIvrFlowBody {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenameIvrFlowBody {
    expected_version: i64,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublishIvrVersionBody {
    graph: IvrGraph,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivateIvrVersionBody {
    expected_version: i64,
    version_number: i32,
}

async fn create_ivr_flow(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateIvrFlowBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_ivr_flow(&AdminActor::of(&ctx), body.name)
        .await?;
    Ok(created(view))
}

async fn list_ivr_flows(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_ivr_flows(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_ivr_flow(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_ivr_flow(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn rename_ivr_flow(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<RenameIvrFlowBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .rename_ivr_flow(&AdminActor::of(&ctx), id, body.expected_version, body.name)
        .await?;
    Ok(ok(view))
}

async fn disable_ivr_flow(
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
        .disable_ivr_flow(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

async fn publish_ivr_version(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<PublishIvrVersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .publish_ivr_version(&AdminActor::of(&ctx), id, body.graph)
        .await?;
    Ok(created(view))
}

async fn list_ivr_versions(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .list_ivr_versions(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn get_ivr_version(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path((id, number)): Path<(String, String)>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let number = number
        .parse::<i32>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or(CallingError::NotFound)?;
    let view = state
        .enterprise
        .get_ivr_version(CallScope::of_platform(&ctx), uuid_path(&id)?, number)
        .await?;
    Ok(ok(view))
}

async fn activate_ivr_version(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<ActivateIvrVersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .activate_ivr_version(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            body.version_number,
        )
        .await?;
    Ok(ok(view))
}

// ---------------------------------------------------------------------------
// Enterprise SIP devices (§54)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindSipDeviceBody {
    device_ref: String,
    principal_id: Uuid,
    display_label: String,
}

async fn bind_sip_device(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<BindSipDeviceBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .bind_sip_device(
            &AdminActor::of(&ctx),
            body.device_ref,
            body.principal_id,
            body.display_label,
        )
        .await?;
    Ok(created(view))
}

async fn list_sip_devices(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_sip_devices(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_sip_device(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_sip_device(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn disable_sip_device(
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
        .disable_sip_device(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

// ---------------------------------------------------------------------------
// Business hours, public contacts, forwarding (§58)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BusinessHoursBody {
    #[serde(default)]
    expected_version: Option<i64>,
    name: String,
    timezone: String,
    open_destination: Destination,
    closed_destination: Destination,
    weekly_windows: Vec<WeeklyWindow>,
    #[serde(default)]
    exceptions: Vec<HoursException>,
}

impl BusinessHoursBody {
    fn settings(self) -> HoursSettings {
        HoursSettings {
            name: self.name,
            timezone: self.timezone,
            open_destination: self.open_destination,
            closed_destination: self.closed_destination,
            weekly_windows: self.weekly_windows,
            exceptions: self.exceptions,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvaluateBody {
    #[serde(default)]
    at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn create_business_hours(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<BusinessHoursBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    if body.expected_version.is_some() {
        return Err(CallingError::Validation(
            "expectedVersion is not allowed when creating.",
        ));
    }
    let view = state
        .enterprise
        .create_business_hours(&AdminActor::of(&ctx), body.settings())
        .await?;
    Ok(created(view))
}

async fn list_business_hours(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_business_hours(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_business_hours(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_business_hours(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn replace_business_hours(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<BusinessHoursBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let expected = body.expected_version.ok_or(CallingError::Validation(
        "expectedVersion is required and must be an integer.",
    ))?;
    let view = state
        .enterprise
        .replace_business_hours(&AdminActor::of(&ctx), id, expected, body.settings())
        .await?;
    Ok(ok(view))
}

async fn disable_business_hours(
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
        .disable_business_hours(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

async fn evaluate_business_hours(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<EvaluateBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .evaluate_business_hours(CallScope::of_platform(&ctx), id, body.at)
        .await?;
    Ok(ok(view))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreatePublicContactBody {
    label: String,
    contact_reference: Uuid,
    privacy_relay_session_id: Uuid,
}

async fn create_public_contact(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreatePublicContactBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_public_contact(
            &AdminActor::of(&ctx),
            body.label,
            body.contact_reference,
            body.privacy_relay_session_id,
        )
        .await?;
    Ok(created(view))
}

async fn list_public_contacts(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_public_contacts(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_public_contact(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_public_contact(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn disable_public_contact(
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
        .disable_public_contact(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateForwardingRuleBody {
    principal_id: Uuid,
    condition: ForwardingCondition,
    destination: Destination,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateForwardingRuleBody {
    expected_version: i64,
    destination: Destination,
}

async fn create_forwarding_rule(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateForwardingRuleBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_forwarding_rule(
            &AdminActor::of(&ctx),
            body.principal_id,
            body.condition,
            body.destination,
        )
        .await?;
    Ok(created(view))
}

async fn list_forwarding_rules(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status", "principalId"])?;
    let page = state
        .enterprise
        .list_forwarding_rules(
            CallScope::of_platform(&ctx),
            q.status()?,
            q.uuid("principalId")?,
            &q.page()?,
        )
        .await?;
    Ok(ok(page))
}

async fn get_forwarding_rule(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_forwarding_rule(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn update_forwarding_rule(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<UpdateForwardingRuleBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .update_forwarding_rule(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            body.destination,
        )
        .await?;
    Ok(ok(view))
}

async fn disable_forwarding_rule(
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
        .disable_forwarding_rule(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

// ---------------------------------------------------------------------------
// Voicemail (§60)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxOwnerBody {
    #[serde(rename = "type")]
    kind: MailboxOwnerType,
    id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateMailboxBody {
    owner: MailboxOwnerBody,
    display_label: String,
    max_message_seconds: i32,
    retention_days: i32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateMailboxBody {
    expected_version: i64,
    #[serde(default)]
    display_label: Option<String>,
    #[serde(default)]
    max_message_seconds: Option<i32>,
    #[serde(default)]
    retention_days: Option<i32>,
}

async fn create_mailbox(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateMailboxBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_VOICEMAIL_MANAGE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_mailbox(
            &AdminActor::of(&ctx),
            MailboxSettings {
                owner_type: body.owner.kind,
                owner_id: body.owner.id,
                display_label: body.display_label,
                max_message_seconds: body.max_message_seconds,
                retention_days: body.retention_days,
            },
        )
        .await?;
    Ok(created(view))
}

async fn list_mailboxes(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_mailboxes(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_mailbox(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_mailbox(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn update_mailbox(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<UpdateMailboxBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_VOICEMAIL_MANAGE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .update_mailbox(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            MailboxChange {
                display_label: body.display_label,
                max_message_seconds: body.max_message_seconds,
                retention_days: body.retention_days,
            },
        )
        .await?;
    Ok(ok(view))
}

async fn disable_mailbox(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_VOICEMAIL_MANAGE)?;
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["expectedVersion"])?;
    let view = state
        .enterprise
        .disable_mailbox(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    Ok(ok(view))
}

/// Who a voicemail request acts for: the application on behalf of ONE principal (who must
/// own the mailbox), or — without `principalId` — a voicemail administrator (the
/// credential holds enterprise.voicemail.manage).
fn requester(
    ctx: &PlatformAuthContext,
    principal: Option<Uuid>,
) -> Result<VoicemailRequester, CallingError> {
    match principal {
        Some(p) => Ok(VoicemailRequester::Principal(p)),
        None if ctx.has_scope(names::ENTERPRISE_VOICEMAIL_MANAGE) => {
            Ok(VoicemailRequester::Administrator)
        }
        None => Err(CallingError::Validation(
            "principalId is required without enterprise.voicemail.manage.",
        )),
    }
}

async fn list_messages(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_VOICEMAIL_READ)?;
    let q = QueryParams::parse(
        query,
        &[
            "limit",
            "cursor",
            "mailboxId",
            "principalId",
            "state",
            "readState",
        ],
    )?;
    let mailbox = q
        .uuid("mailboxId")?
        .ok_or(CallingError::Validation("mailboxId is required."))?;
    let filter = MessageFilter {
        state: q
            .get("state")
            .map(|v| MessageState::parse(v).ok_or(CallingError::Validation("state is invalid.")))
            .transpose()?,
        read_state: q
            .get("readState")
            .map(|v| ReadState::parse(v).ok_or(CallingError::Validation("readState is invalid.")))
            .transpose()?,
    };
    let page = state
        .enterprise
        .list_voicemail_messages(
            CallScope::of_platform(&ctx),
            mailbox,
            requester(&ctx, q.uuid("principalId")?)?,
            filter,
            &q.page()?,
        )
        .await?;
    Ok(ok(page))
}

async fn get_message(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_VOICEMAIL_READ)?;
    let q = QueryParams::parse(query, &["principalId"])?;
    let view = state
        .enterprise
        .get_voicemail_message(
            CallScope::of_platform(&ctx),
            uuid_path(&id)?,
            requester(&ctx, q.uuid("principalId")?)?,
        )
        .await?;
    Ok(ok(view))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadStateBody {
    expected_version: i64,
    read_state: ReadState,
    #[serde(default)]
    principal_id: Option<Uuid>,
}

async fn set_read_state(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<ReadStateBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_VOICEMAIL_MANAGE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .set_voicemail_read(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            body.read_state == ReadState::Read,
            requester(&ctx, body.principal_id)?,
        )
        .await?;
    Ok(ok(view))
}

async fn delete_message(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_VOICEMAIL_MANAGE)?;
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["expectedVersion", "principalId"])?;
    let view = state
        .enterprise
        .delete_voicemail_message(
            &AdminActor::of(&ctx),
            id,
            q.expected_version()?,
            requester(&ctx, q.uuid("principalId")?)?,
        )
        .await?;
    Ok(ok(view))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MediaAccessBody {
    #[serde(default)]
    principal_id: Option<Uuid>,
}

async fn media_access(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<MediaAccessBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_VOICEMAIL_READ)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let access = state
        .enterprise
        .voicemail_media_access(
            &AdminActor::of(&ctx),
            id,
            requester(&ctx, body.principal_id)?,
        )
        .await?;
    Ok(ok(access))
}

// ---------------------------------------------------------------------------
// Call recording (§61)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateRecordingPolicyBody {
    name: String,
    mode: RecordingMode,
    notice_required: bool,
    consent_participants: ConsentParticipants,
    jurisdiction_ref: String,
    retention_days: i32,
    max_recording_seconds: i32,
    #[serde(default)]
    is_default: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateRecordingPolicyBody {
    expected_version: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mode: Option<RecordingMode>,
    #[serde(default)]
    notice_required: Option<bool>,
    #[serde(default)]
    consent_participants: Option<ConsentParticipants>,
    #[serde(default)]
    jurisdiction_ref: Option<String>,
    #[serde(default)]
    retention_days: Option<i32>,
    #[serde(default)]
    max_recording_seconds: Option<i32>,
    #[serde(default)]
    is_default: Option<bool>,
}

async fn create_recording_policy(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateRecordingPolicyBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_MANAGE)?;
    let body = parse_body(body)?;
    let view = state
        .enterprise
        .create_recording_policy(
            &AdminActor::of(&ctx),
            RecordingPolicySettings {
                name: body.name,
                mode: body.mode,
                notice_required: body.notice_required,
                consent_participants: body.consent_participants,
                jurisdiction_ref: body.jurisdiction_ref,
                retention_days: body.retention_days,
                max_recording_seconds: body.max_recording_seconds,
                is_default: body.is_default,
            },
        )
        .await?;
    Ok(created(view))
}

async fn list_recording_policies(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let page = state
        .enterprise
        .list_recording_policies(CallScope::of_platform(&ctx), q.status()?, &q.page()?)
        .await?;
    Ok(ok(page))
}

async fn get_recording_policy(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_CONFIG_READ)?;
    let view = state
        .enterprise
        .get_recording_policy(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(view))
}

async fn update_recording_policy(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<UpdateRecordingPolicyBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_MANAGE)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let (view, revoked) = state
        .enterprise
        .update_recording_policy(
            &AdminActor::of(&ctx),
            id,
            body.expected_version,
            RecordingPolicyChange {
                name: body.name,
                mode: body.mode,
                notice_required: body.notice_required,
                consent_participants: body.consent_participants,
                jurisdiction_ref: body.jurisdiction_ref,
                retention_days: body.retention_days,
                max_recording_seconds: body.max_recording_seconds,
                is_default: body.is_default,
            },
        )
        .await?;
    if revoked {
        // Recording authorized under the previous terms stops (POLICY_REVOKED).
        state
            .app
            .revoke_recordings_of_policy(CallScope::of_platform(&ctx), id)
            .await?;
    }
    Ok(ok(view))
}

async fn disable_recording_policy(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_MANAGE)?;
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["expectedVersion"])?;
    let view = state
        .enterprise
        .disable_recording_policy(&AdminActor::of(&ctx), id, q.expected_version()?)
        .await?;
    state
        .app
        .revoke_recordings_of_policy(CallScope::of_platform(&ctx), id)
        .await?;
    Ok(ok(view))
}

/// Who a recording request acts for: the application on behalf of ONE principal (a
/// participant of the Call), or — without `principalId` — a recording administrator
/// (the credential holds enterprise.recording.manage).
fn recording_requester(
    ctx: &PlatformAuthContext,
    principal: Option<Uuid>,
) -> Result<RecordingRequester, CallingError> {
    match principal {
        Some(p) => Ok(RecordingRequester::Principal(p)),
        None if ctx.has_scope(names::ENTERPRISE_RECORDING_MANAGE) => {
            Ok(RecordingRequester::Administrator)
        }
        None => Err(CallingError::Validation(
            "principalId is required without enterprise.recording.manage.",
        )),
    }
}

fn recording_actor(requester: RecordingRequester) -> RecordingActor {
    match requester {
        RecordingRequester::Principal(p) => RecordingActor::Principal(p),
        RecordingRequester::Administrator => RecordingActor::Administrator,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordingConsentBody {
    endpoint_id: Uuid,
    decision: ConsentDecision,
}

async fn record_recording_consent(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
    body: Result<Json<RecordingConsentBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    ctx.require_scope(names::ENTERPRISE_CALLS_USE)?;
    let call_id = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let view = state
        .app
        .record_recording_consent(&ctx, call_id, body.endpoint_id, body.decision)
        .await?;
    Ok(ok(view))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordingActorBody {
    #[serde(default)]
    principal_id: Option<Uuid>,
}

async fn start_recording(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
    body: Result<Json<RecordingActorBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_MANAGE)?;
    let call_id = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let actor = recording_actor(recording_requester(&ctx, body.principal_id)?);
    let outcome = state
        .app
        .start_recording(Some(&ctx), CallScope::of_platform(&ctx), call_id, actor)
        .await?;
    match outcome {
        StartOutcome::Started(recording) => Ok(created(RecordingEnvelope { recording })),
        StartOutcome::AlreadyActive(recording) => Ok(ok(RecordingEnvelope { recording })),
        StartOutcome::Denied(_) => Err(CallingError::Call {
            code: crate::error::ErrorCode::RecordingNotAuthorized,
            status: StatusCode::FORBIDDEN,
        }),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordingEnvelope {
    recording: CallRecordingView,
}

async fn stop_recording(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path((call_id, recording_id)): Path<(String, String)>,
    body: Result<Json<RecordingActorBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_MANAGE)?;
    let call_id = uuid_path(&call_id)?;
    let recording_id = uuid_path(&recording_id)?;
    let body = parse_body(body)?;
    let actor = recording_actor(recording_requester(&ctx, body.principal_id)?);
    let recording = state
        .app
        .stop_recording(&ctx, call_id, recording_id, actor)
        .await?;
    Ok(ok(RecordingEnvelope { recording }))
}

async fn list_call_recordings(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_READ)?;
    let call_id = uuid_path(&call_id)?;
    let q = QueryParams::parse(query, &["principalId"])?;
    let page = state
        .enterprise
        .list_call_recordings(
            CallScope::of_platform(&ctx),
            call_id,
            recording_requester(&ctx, q.uuid("principalId")?)?,
        )
        .await?;
    Ok(ok(page))
}

async fn list_recording_authorizations(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_READ)?;
    let call_id = uuid_path(&call_id)?;
    let q = QueryParams::parse(query, &["principalId"])?;
    let list = state
        .enterprise
        .list_recording_authorizations(
            CallScope::of_platform(&ctx),
            call_id,
            recording_requester(&ctx, q.uuid("principalId")?)?,
        )
        .await?;
    Ok(ok(list))
}

async fn get_call_recording(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_READ)?;
    let q = QueryParams::parse(query, &["principalId"])?;
    let view = state
        .enterprise
        .get_call_recording(
            &AdminActor::of(&ctx),
            uuid_path(&id)?,
            recording_requester(&ctx, q.uuid("principalId")?)?,
        )
        .await?;
    Ok(ok(view))
}

async fn recording_media_access(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<RecordingActorBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_READ)?;
    let id = uuid_path(&id)?;
    let body = parse_body(body)?;
    let access = state
        .enterprise
        .recording_media_access(
            &AdminActor::of(&ctx),
            id,
            recording_requester(&ctx, body.principal_id)?,
        )
        .await?;
    Ok(ok(access))
}

async fn delete_call_recording(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_MANAGE)?;
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["expectedVersion", "principalId"])?;
    let view = state
        .enterprise
        .delete_call_recording(
            &AdminActor::of(&ctx),
            id,
            q.expected_version()?,
            recording_requester(&ctx, q.uuid("principalId")?)?,
        )
        .await?;
    Ok(ok(view))
}

async fn recording_access_audit(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::ENTERPRISE_RECORDING_MANAGE)?;
    let list = state
        .enterprise
        .recording_access_audit(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(ok(list))
}
