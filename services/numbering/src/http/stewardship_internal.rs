//! Lifecycle stewardship internal API (`/internal/v1`, service tokens; Workstreams S, T
//! and Part P §§119–120):
//!
//! * `POST /number-usage` (`number-usage.report`: Telephony voice seconds, SMS Gateway SMS
//!   segments) — idempotent internal cost evidence;
//! * `POST /number-risk-signals` (`number-risk.report`) — restrictive signals;
//! * `POST /relay-inventory/imports`, `POST /relay-leases`, `POST /relay-leases/{id}/release`,
//!   `GET /relay-numbers/{relayInventoryRef}/resolution` (`number-relay.lease`, Privacy
//!   Relay) — relay numbers under Numbering's authority.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{error::ApiError, middleware::InternalCaller, numbers::ctx, stewardship::with_status};
use crate::{
    numbers::Capability,
    state::AppState,
    stewardship::{
        relay::{LeaseRequest, LeaseRow, RelayImport},
        risk::{NewSignal, SignalKind},
        usage::{NewUsage, UsageKind},
    },
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/number-usage", post(report_usage))
        .route("/number-risk-signals", post(report_signal))
        .route("/relay-inventory/imports", post(import_relay))
        .route("/relay-leases", post(lease_relay))
        .route("/relay-leases/:lease_id/release", post(release_lease))
        .route(
            "/relay-numbers/:relay_inventory_ref/resolution",
            get(resolve_relay),
        )
}

fn lease_view(l: &LeaseRow) -> Value {
    json!({
        "leaseId": l.id,
        "numberResourceId": l.number_resource_id,
        "holderRef": l.holder_ref,
        "generation": l.generation,
        "status": l.status,
        "grantedAt": l.granted_at,
        "expiresAt": l.expires_at,
        "endedAt": l.ended_at,
        "endReason": l.end_reason,
    })
}

// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UsageBody {
    source_key: String,
    number_resource_id: Uuid,
    assignment_id: Uuid,
    usage_kind: UsageKind,
    quantity: i64,
    #[serde(default)]
    provider_cost_minor: Option<i64>,
    #[serde(default)]
    currency: Option<String>,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
}

async fn report_usage(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<UsageBody>,
) -> Result<Response, ApiError> {
    caller.require_permission(InternalPermission::NumberUsageReport)?;
    let (row, recorded) = s
        .stewardship()
        .report_usage(
            &ctx(),
            caller.service(),
            NewUsage {
                source_key: body.source_key,
                number_resource_id: body.number_resource_id,
                assignment_id: Some(body.assignment_id),
                kind: body.usage_kind,
                quantity: body.quantity,
                provider_cost_minor: body.provider_cost_minor,
                currency: body.currency,
                period_start: body.period_start,
                period_end: body.period_end,
            },
        )
        .await?;
    Ok(with_status(
        StatusCode::CREATED,
        !recorded,
        json!({ "usageRecordId": row.id, "status": row.status, "recorded": recorded }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignalBody {
    number_resource_id: Uuid,
    signal_kind: SignalKind,
    magnitude: i32,
    source_key: String,
}

async fn report_signal(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<SignalBody>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberRiskReport)?;
    let out = s
        .stewardship()
        .report_signal(
            &ctx(),
            caller.service(),
            NewSignal {
                number_resource_id: body.number_resource_id,
                kind: body.signal_kind,
                magnitude: body.magnitude,
                source_key: body.source_key,
            },
        )
        .await?;
    Ok(Json(json!({
        "recorded": out.recorded,
        "score": out.score,
        "outcome": out.outcome,
        "reasons": out.reasons,
        "riskDecisionId": out.decision_id,
        "suspensionId": out.suspension_id,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportBody {
    relay_inventory_ref: Uuid,
    e164: String,
    country: String,
    #[serde(default)]
    region: Option<String>,
    capabilities: Vec<Capability>,
    binding_ref: String,
}

async fn import_relay(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<ImportBody>,
) -> Result<Response, ApiError> {
    caller.require_permission(InternalPermission::NumberRelayLease)?;
    // Synthetic inventory in local / test runtimes, never in staging / production.
    let runtime = s.service_auth.signer().runtime_environment();
    let synthetic = !matches!(
        runtime,
        cf_service_auth::RuntimeEnvironment::Staging
            | cf_service_auth::RuntimeEnvironment::Production
    );
    let (member, created) = s
        .stewardship()
        .import_relay_number(
            &ctx(),
            caller.service(),
            RelayImport {
                relay_inventory_ref: body.relay_inventory_ref,
                e164: body.e164,
                country: body.country,
                region: body.region,
                capabilities: body.capabilities,
                binding_ref: body.binding_ref,
                synthetic,
            },
        )
        .await?;
    Ok(with_status(
        StatusCode::CREATED,
        !created,
        json!({
            "relayInventoryRef": member.relay_inventory_ref,
            "numberResourceId": member.number_resource_id,
            "status": member.status,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LeaseBody {
    relay_inventory_ref: Uuid,
    holder_ref: Uuid,
    generation: i64,
    expires_at: DateTime<Utc>,
    idempotency_key: String,
}

async fn lease_relay(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<LeaseBody>,
) -> Result<Response, ApiError> {
    caller.require_permission(InternalPermission::NumberRelayLease)?;
    let (lease, replayed) = s
        .stewardship()
        .lease_relay_number(
            &ctx(),
            caller.service(),
            LeaseRequest {
                relay_inventory_ref: body.relay_inventory_ref,
                holder_ref: body.holder_ref,
                generation: body.generation,
                expires_at: body.expires_at,
                idempotency_key: body.idempotency_key,
            },
        )
        .await?;
    Ok(with_status(
        StatusCode::CREATED,
        replayed,
        json!({ "lease": lease_view(&lease) }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LeaseReleaseBody {
    reason_code: String,
}

async fn release_lease(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<LeaseReleaseBody>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberRelayLease)?;
    let lease = s
        .stewardship()
        .release_relay_lease(&ctx(), caller.service(), id, &body.reason_code)
        .await?;
    Ok(Json(json!({ "lease": lease_view(&lease) })))
}

async fn resolve_relay(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberRelayLease)?;
    let r = s.stewardship().resolve_relay_number(id).await?;
    Ok(Json(json!({
        "relayInventoryRef": r.relay_inventory_ref,
        "numberResourceId": r.number_resource_id,
        "lifecycleState": r.lifecycle_state,
        "memberStatus": r.member_status,
        "leasable": r.member_status == "ACTIVE" && r.lifecycle_state == "AVAILABLE" && r.lease.is_none(),
        "lease": r.lease.as_ref().map(lease_view),
    })))
}
