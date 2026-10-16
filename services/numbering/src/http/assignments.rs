//! Ownership and assignment (Workstream J), in the caller's TRUSTED scope:
//!
//! * `POST /numbers/assignments` (`numbers.assign`, `Idempotency-Key` required) — assign
//!   a provisioned acquisition or a held reservation to its legal owner (201, RESERVED);
//! * `GET  /numbers/assignments`, `GET /numbers/assignments/{assignmentId}`
//!   (`numbers.read`) — the caller's assignments (filters, keyset pages);
//! * `POST /numbers/assignments/{assignmentId}/activate` (`numbers.assign`) — ACTIVE once
//!   eligibility re-evaluates and every routed family is ready;
//! * `GET / POST /numbers/assignments/{assignmentId}/routing` (`numbers.read` /
//!   `numbers.manage`) — the pinned routing revision and readiness; a target change;
//! * `POST /numbers/assignments/{assignmentId}/offboarding` (`numbers.manage`) — an
//!   employee's direct number leaves the departing employee;
//! * `POST /numbers/assignments/{assignmentId}/transfer` (`numbers.assign`,
//!   `Idempotency-Key` required) — within the same legal owner only;
//! * `/numbers/pools…` — organizational pools (`numbers.read` / `numbers.manage`);
//! * `GET /admin/number-resources/{numberResourceId}/assignment-history`
//!   (`numbers.admin`, first-party, `X-CF-Operator`) — every owner's history of a number.
//!
//! Another scope's assignment, pool or profile is indistinguishable from a missing one.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    acquisitions::standing,
    customer::{assignment_view, NUMBERS_READ},
    error::{ApiError, ErrorCode},
    middleware::{AdminCaller, CustomerCaller},
    numbers::{ctx, required_idempotency_key, NUMBERS_MANAGE},
};
use crate::{
    acquisition::{
        assignments::{AssignmentBasis, AssignmentRequest, TransferRequest},
        reservations::customer_actor,
    },
    numbers::{
        acquisitions::FamilyReadiness,
        pools::{PoolMemberRow, PoolRow},
        routing::RoutingProfileRow,
        AssignmentStatus, Capability, OwnershipClass, SmsTargetKind, VoiceTargetKind,
    },
    state::AppState,
};

pub const NUMBERS_ASSIGN: &str = "numbers.assign";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/numbers/assignments",
            get(list_assignments).post(create_assignment),
        )
        .route("/numbers/assignments/:assignment_id", get(get_assignment))
        .route(
            "/numbers/assignments/:assignment_id/activate",
            post(activate_assignment),
        )
        .route(
            "/numbers/assignments/:assignment_id/routing",
            get(get_routing).post(update_routing),
        )
        .route(
            "/numbers/assignments/:assignment_id/offboarding",
            post(offboard),
        )
        .route(
            "/numbers/assignments/:assignment_id/transfer",
            post(transfer),
        )
        .route("/numbers/pools", get(list_pools).post(create_pool))
        .route("/numbers/pools/:pool_id", get(get_pool))
        .route("/numbers/pools/:pool_id/members", post(add_member))
        .route(
            "/numbers/pools/:pool_id/members/:assignment_id",
            axum::routing::delete(remove_member),
        )
        .route(
            "/admin/number-resources/:number_resource_id/assignment-history",
            get(admin_history),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoiceTargetBody {
    pub kind: VoiceTargetKind,
    #[serde(default, rename = "ref")]
    pub target_ref: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SmsTargetBody {
    pub kind: SmsTargetKind,
    #[serde(default, rename = "ref")]
    pub target_ref: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetsBody {
    pub voice: VoiceTargetBody,
    pub sms: SmsTargetBody,
}

impl TargetsBody {
    fn targets(&self) -> crate::numbers::routing::RoutingTargets {
        crate::numbers::routing::RoutingTargets {
            voice: (self.voice.kind, self.voice.target_ref),
            sms: (self.sms.kind, self.sms.target_ref),
        }
    }
}

fn created(replayed: bool, body: Value) -> Response {
    let status = if replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    let mut r = (status, Json(body)).into_response();
    if replayed {
        r.headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    r
}

fn routing_view(p: &RoutingProfileRow) -> Value {
    json!({
        "routingProfileId": p.profile_id,
        "revision": p.revision,
        "voice": { "kind": p.voice_target_kind, "ref": p.voice_target_ref },
        "sms": { "kind": p.sms_target_kind, "ref": p.sms_target_ref },
        "status": p.status,
        "createdAt": p.created_at,
    })
}

fn readiness_view(routes: &[FamilyReadiness]) -> Value {
    json!(routes
        .iter()
        .map(|r| json!({
            "family": r.family,
            "routeRevision": r.route_revision,
            "routeStatus": r.route_status,
            "acknowledgedBy": r.acknowledged_by,
            "ready": r.ready,
            "reason": r.reason,
        }))
        .collect::<Vec<_>>())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAssignmentBody {
    #[serde(default)]
    pub acquisition_id: Option<Uuid>,
    #[serde(default)]
    pub reservation_id: Option<Uuid>,
    pub ownership_class: OwnershipClass,
    #[serde(default)]
    pub owner_principal_id: Option<Uuid>,
    pub routing: TargetsBody,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub eligibility_decision_id: Option<Uuid>,
    #[serde(default)]
    pub pool_id: Option<Uuid>,
}

async fn create_assignment(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<CreateAssignmentBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_ASSIGN)?;
    let key = required_idempotency_key(&headers)?;
    let basis = match (body.acquisition_id, body.reservation_id) {
        (Some(a), None) => AssignmentBasis::Acquisition(a),
        (None, Some(r)) => AssignmentBasis::Reservation(r),
        _ => return Err(ApiError::Code(ErrorCode::ValidationError)),
    };
    let out = s
        .acquisition()
        .create_assignment(
            &ctx(),
            &tc.owner_scope(),
            standing(tc),
            AssignmentRequest {
                basis,
                ownership_class: body.ownership_class,
                owner_principal_id: body.owner_principal_id,
                targets: body.routing.targets(),
                capabilities: body.capabilities,
                eligibility_decision_id: body.eligibility_decision_id,
                pool_id: body.pool_id,
            },
            &key,
        )
        .await?;
    Ok(created(out.replayed, assignment_view(&out.assignment)))
}

fn decode_cursor(cursor: &str) -> Result<(DateTime<Utc>, Uuid), ApiError> {
    let bad = || ApiError::Code(ErrorCode::ValidationError);
    if cursor.len() > 200 {
        return Err(bad());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| bad())?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    let at = v["t"]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .ok_or_else(bad)?
        .with_timezone(&Utc);
    let id = v["i"]
        .as_str()
        .and_then(|i| Uuid::parse_str(i).ok())
        .ok_or_else(bad)?;
    Ok((at, id))
}

async fn list_assignments(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    let bad = || ApiError::Code(ErrorCode::ValidationError);
    if params
        .keys()
        .any(|k| !["limit", "cursor", "status", "ownershipClass"].contains(&k.as_str()))
    {
        return Err(bad());
    }
    let limit = match params.get("limit") {
        None => 50,
        Some(l) => l
            .parse::<u64>()
            .ok()
            .filter(|l| (1..=100).contains(l))
            .ok_or_else(bad)?,
    };
    let status = params
        .get("status")
        .map(|v| AssignmentStatus::parse(v).ok_or_else(bad))
        .transpose()?;
    let class = params
        .get("ownershipClass")
        .map(|v| OwnershipClass::parse(v).ok_or_else(bad))
        .transpose()?;
    let before = params.get("cursor").map(|c| decode_cursor(c)).transpose()?;
    let rows = s
        .numbers
        .list_assignments(&tc.owner_scope(), status, class, before, limit + 1)
        .await?;
    let more = rows.len() as u64 > limit;
    let page: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next = more.then(|| {
        let last = page.last().expect("a full page");
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({ "t": last.requested_at, "i": last.id }).to_string())
    });
    Ok(Json(json!({
        "data": page.iter().map(assignment_view).collect::<Vec<_>>(),
        "nextCursor": next,
    })))
}

async fn get_assignment(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    Ok(Json(assignment_view(
        &s.numbers.assignment(&tc.owner_scope(), id).await?,
    )))
}

async fn activate_assignment(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_ASSIGN)?;
    let out = s
        .acquisition()
        .activate_assignment(&ctx(), &tc.owner_scope(), standing(tc), id)
        .await?;
    Ok(Json(assignment_view(&out.assignment)))
}

async fn get_routing(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    let a = s.numbers.assignment(&tc.owner_scope(), id).await?;
    let profile = match (a.routing_profile_id, a.routing_profile_revision) {
        (Some(p), Some(r)) => Some(s.numbers.routing_profile_revision(p, r).await?),
        _ => None,
    };
    let readiness = s.acquisition().assignment_readiness(&a).await?;
    Ok(Json(json!({
        "assignmentId": a.id,
        "routing": profile.as_ref().map(routing_view),
        "routingRevision": s.numbers.resource(a.number_resource_id).await?.routing_revision,
        "readiness": readiness_view(&readiness),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRoutingBody {
    pub expected_routing_revision: i64,
    pub routing: TargetsBody,
}

async fn update_routing(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateRoutingBody>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let row = s
        .acquisition()
        .update_routing(
            &ctx(),
            &tc.owner_scope(),
            id,
            body.expected_routing_revision,
            body.routing.targets(),
            "ROUTING_CHANGED",
        )
        .await?;
    Ok(Json(assignment_view(&row)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OffboardBody {
    pub expected_routing_revision: i64,
    #[serde(default)]
    pub replacement: Option<TargetsBody>,
}

async fn offboard(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<OffboardBody>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let row = s
        .acquisition()
        .offboard_employee(
            &ctx(),
            &tc.owner_scope(),
            id,
            body.expected_routing_revision,
            body.replacement.as_ref().map(TargetsBody::targets),
        )
        .await?;
    Ok(Json(assignment_view(&row)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferBody {
    pub ownership_class: OwnershipClass,
    #[serde(default)]
    pub owner_principal_id: Option<Uuid>,
    pub routing: TargetsBody,
    #[serde(default)]
    pub eligibility_decision_id: Option<Uuid>,
    pub expected_routing_revision: i64,
}

async fn transfer(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<TransferBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_ASSIGN)?;
    let key = required_idempotency_key(&headers)?;
    let out = s
        .acquisition()
        .transfer_assignment(
            &ctx(),
            &tc.owner_scope(),
            standing(tc),
            id,
            TransferRequest {
                ownership_class: body.ownership_class,
                owner_principal_id: body.owner_principal_id,
                targets: body.routing.targets(),
                eligibility_decision_id: body.eligibility_decision_id,
                expected_routing_revision: body.expected_routing_revision,
            },
            &key,
        )
        .await?;
    let mut r = Json(json!({
        "transferKind": match out.kind {
            crate::numbers::ownership::TransferKind::Retarget => "RETARGET",
            crate::numbers::ownership::TransferKind::Reclassify => "RECLASSIFY",
        },
        "assignment": assignment_view(&out.assignment),
        "predecessor": out.predecessor.as_ref().map(assignment_view),
    }))
    .into_response();
    if out.replayed {
        r.headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    Ok(r)
}

fn pool_view(p: &PoolRow) -> Value {
    json!({
        "poolId": p.id,
        "name": p.name,
        "status": p.status,
        "usagePolicyRef": p.usage_policy_ref,
        "subscriptionPolicyRef": p.subscription_policy_ref,
        "revision": p.revision,
        "createdAt": p.created_at,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePoolBody {
    pub name: String,
    #[serde(default)]
    pub usage_policy_ref: Option<Uuid>,
    #[serde(default)]
    pub subscription_policy_ref: Option<Uuid>,
}

async fn create_pool(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Json(body): Json<CreatePoolBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let scope = tc.owner_scope();
    let row = s
        .numbers
        .create_pool(
            &ctx(),
            &scope,
            &customer_actor(&scope),
            &body.name,
            body.usage_policy_ref,
            body.subscription_policy_ref,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(pool_view(&row))))
}

async fn list_pools(
    State(s): State<AppState>,
    caller: CustomerCaller,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    let rows = s.numbers.pools(&tc.owner_scope()).await?;
    Ok(Json(
        json!({ "data": rows.iter().map(pool_view).collect::<Vec<_>>() }),
    ))
}

async fn get_pool(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    let scope = tc.owner_scope();
    let pool = s.numbers.pool(&scope, id).await?;
    let members = s.numbers.pool_members(&scope, id).await?;
    let mut view = pool_view(&pool);
    view["members"] = json!(members
        .iter()
        .map(|m| json!({
            "assignmentId": m.assignment.id,
            "numberResourceId": m.assignment.number_resource_id,
            "ownershipClass": m.assignment.ownership_class,
            "assignmentStatus": m.assignment.status,
            "usage": if m.assigned() { "ASSIGNED" } else { "AVAILABLE" },
            "routing": m.routing.as_ref().map(routing_view),
            "addedAt": m.member.added_at,
        }))
        .collect::<Vec<_>>());
    Ok(Json(view))
}

fn membership_view(m: &PoolMemberRow) -> Value {
    json!({
        "poolId": m.pool_id,
        "assignmentId": m.assignment_id,
        "numberResourceId": m.number_resource_id,
        "addedAt": m.added_at,
        "removedAt": m.removed_at,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddMemberBody {
    pub assignment_id: Uuid,
}

async fn add_member(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(pool_id): Path<Uuid>,
    Json(body): Json<AddMemberBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let scope = tc.owner_scope();
    let m = s
        .numbers
        .add_pool_member(
            &ctx(),
            &scope,
            &customer_actor(&scope),
            pool_id,
            body.assignment_id,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(membership_view(&m))))
}

async fn remove_member(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path((pool_id, assignment_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let scope = tc.owner_scope();
    let m = s
        .numbers
        .remove_pool_member(
            &ctx(),
            &scope,
            &customer_actor(&scope),
            pool_id,
            assignment_id,
        )
        .await?;
    Ok(Json(membership_view(&m)))
}

async fn admin_history(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rows = s
        .numbers
        .resource_assignment_history(&admin.actor, id)
        .await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|h| {
            json!({
                "sequence": h.sequence,
                "assignmentId": h.assignment_id,
                "tenantId": h.tenant_id,
                "applicationId": h.application_id,
                "environmentId": h.environment_id,
                "eventKind": h.event_kind,
                "fromStatus": h.from_status,
                "toStatus": h.to_status,
                "revision": h.assignment_revision,
                "ownerKind": h.owner_kind,
                "ownerRef": h.owner_ref,
                "ownershipClass": h.ownership_class,
                "routingProfileId": h.routing_profile_id,
                "routingProfileRevision": h.routing_profile_revision,
                "actorKind": h.actor_kind,
                "actorRef": h.actor_ref,
                "reasonCode": h.reason_code,
                "occurredAt": h.occurred_at,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}
