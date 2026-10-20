//! Part V I §129 — usage metering.
//!
//! Messaging records usage FACTS of an application environment (accepted messages,
//! stored attachment bytes, bridged SMS segments) in `messaging_usage_records`, in the
//! same transaction as the fact itself and idempotently per key. It never prices or bills
//! anything: there is no rate, price or cost here (SMS cost evidence stays with the SMS
//! Gateway; billing is outside this service). Readers aggregate with
//! `GET /v1/messaging/usage` (scope `usage.read`).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    error::{Error, Result},
    identity::TrustedContext,
    state::AppState,
};

pub const MESSAGE_ACCEPTED: &str = "MESSAGE_ACCEPTED";
pub const ATTACHMENT_STORED: &str = "ATTACHMENT_STORED";
pub const SMS_BRIDGED: &str = "SMS_BRIDGED";

fn unit(usage_type: &str) -> &'static str {
    match usage_type {
        MESSAGE_ACCEPTED => "MESSAGES",
        ATTACHMENT_STORED => "BYTES",
        _ => "SEGMENTS",
    }
}

/// The application environment a fact belongs to.
#[derive(Debug, Clone, Copy)]
pub struct Owner {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

impl From<&TrustedContext> for Owner {
    fn from(ctx: &TrustedContext) -> Self {
        Owner {
            tenant_id: ctx.scope.tenant_id,
            application_id: ctx.scope.application_id,
            environment_id: ctx.scope.environment_id,
        }
    }
}

/// Records one fact; a repeated key is a no-op (returns false).
#[allow(clippy::too_many_arguments)]
pub async fn record<C: ConnectionTrait>(
    db: &C,
    owner: Owner,
    usage_type: &'static str,
    quantity: i64,
    subject_id: Uuid,
    security_profile: Option<&str>,
    idempotency_key: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    let n = db::exec(
        db,
        "INSERT INTO messaging_usage_records (id, tenant_id, application_id, environment_id, usage_type, quantity, \
           unit, subject_id, security_profile, idempotency_key, occurred_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11) \
         ON CONFLICT (environment_id, idempotency_key) DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            owner.tenant_id.into(),
            owner.application_id.into(),
            owner.environment_id.into(),
            usage_type.into(),
            quantity.max(0).into(),
            unit(usage_type).into(),
            subject_id.into(),
            security_profile.map(str::to_string).into(),
            idempotency_key.into(),
            now.into(),
        ],
    )
    .await?;
    if n == 1 {
        crate::metrics::inc("messaging_usage_records_total", &[("type", usage_type)]);
    }
    Ok(n == 1)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(FromQueryResult)]
struct Total {
    usage_type: String,
    unit: String,
    security_profile: Option<String>,
    quantity: i64,
    records: i64,
}

/// Totals of the caller's environment in `[from, to)` (at most 93 days), per usage type
/// and security profile. Quantities only: no price, no cost.
pub async fn summary(state: &AppState, ctx: &TrustedContext, q: UsageQuery) -> Result<Value> {
    if q.to <= q.from || q.to - q.from > chrono::Duration::days(93) {
        return Err(Error::validation(
            "from must be before to, at most 93 days apart",
        ));
    }
    let rows: Vec<Total> = db::all(
        &state.db,
        "SELECT usage_type, unit, security_profile, sum(quantity)::bigint AS quantity, count(*) AS records \
           FROM messaging_usage_records \
          WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND occurred_at >= $4 AND occurred_at < $5 \
          GROUP BY usage_type, unit, security_profile ORDER BY usage_type, security_profile NULLS FIRST",
        vec![
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            q.from.into(),
            q.to.into(),
        ],
    )
    .await?;
    Ok(json!({
        "from": crate::conversations::service::rfc3339(q.from),
        "to": crate::conversations::service::rfc3339(q.to),
        "totals": rows.iter().map(|r| json!({
            "usageType": r.usage_type, "unit": r.unit, "securityProfile": r.security_profile,
            "quantity": r.quantity, "records": r.records,
        })).collect::<Vec<_>>(),
        "pricing": "NONE",
    }))
}

pub mod http {
    use axum::{
        extract::{Query, State},
        response::IntoResponse,
        routing::get,
        Json, Router,
    };

    use crate::{
        http::{error::ApiError, middleware::CustomerCaller},
        state::AppState,
    };

    pub const USAGE_READ: &str = "usage.read";

    pub fn routes() -> Router<AppState> {
        Router::new().route("/usage", get(usage))
    }

    async fn usage(
        State(state): State<AppState>,
        caller: CustomerCaller,
        Query(q): Query<super::UsageQuery>,
    ) -> Result<impl IntoResponse, ApiError> {
        let ctx = caller.require_scope(USAGE_READ)?;
        Ok(Json(super::summary(&state, ctx, q).await?))
    }
}
