//! Provider-level usage (Part E §81, §110, §156, §225).
//!
//! Exactly ONE final usage fact per Telephony leg (`uq_telephony_usage_records_leg`; a
//! second finalizer's insert does nothing). Connected duration comes from Telephony's own
//! answer / connect and end evidence; billable seconds round up per second (the provider's
//! own granularity is not assumed). Provider cost is recorded as KNOWN only when the
//! provider actually reported it; otherwise the route's conservative estimate is kept as
//! an estimate (`cost_known = false`). Calling receives USAGE_FINALIZED to settle the
//! spend reservation; no provider commercial data leaves Telephony beyond that amount.

use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

use crate::{
    db,
    error::TelephonyResult,
    events::outbox,
    legs::{self, LegRow, LEG_COLUMNS},
    service::TelephonyService,
};

#[derive(Debug, FromQueryResult)]
struct RouteCost {
    estimated_leg_cost_minor: Option<i64>,
    cost_currency: Option<String>,
}

impl TelephonyService {
    /// Finalizes usage of terminal legs (bounded batch; idempotent).
    pub async fn finalize_usage_once(&self, limit: u64) -> TelephonyResult<usize> {
        let due: Vec<LegRow> = db::all(
            &self.db,
            &format!(
                "SELECT {LEG_COLUMNS} FROM telephony_legs WHERE usage_status = 'PENDING' \
                 AND status IN ('ENDED', 'FAILED') ORDER BY ended_at LIMIT $1"
            ),
            vec![(limit.clamp(1, 500) as i64).into()],
        )
        .await?;
        let mut n = 0;
        for leg in due {
            if self.finalize_leg_usage(leg.id).await? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Finalizes one leg's usage; returns whether THIS call created the fact.
    pub async fn finalize_leg_usage(&self, leg_id: Uuid) -> TelephonyResult<bool> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let leg = legs::lock(&txn, leg_id).await?;
        if leg.usage_status != "PENDING" || !leg.status()?.is_terminal() {
            return Ok(false);
        }
        let start = leg.connected_at.or(leg.answered_at);
        let duration_ms = match (start, leg.ended_at) {
            (Some(s), Some(e)) if e > s => (e - s).num_milliseconds(),
            _ => 0,
        };
        let billable_seconds = ((duration_ms + 999) / 1000) as i32;
        let route_cost: Option<RouteCost> = match &leg.route_id {
            Some(r) => db::one(
                &txn,
                "SELECT estimated_leg_cost_minor, cost_currency FROM telecom_routes WHERE id = $1",
                vec![r.clone().into()],
            )
            .await?,
            None => None,
        };
        let (cost_known, provider_cost, estimate, currency) =
            match (&leg.provider_cost_minor, &leg.provider_cost_currency) {
                (Some(c), Some(cur)) => (true, Some(*c), None, Some(cur.clone())),
                _ => {
                    let estimate = route_cost
                        .as_ref()
                        .and_then(|r| r.estimated_leg_cost_minor)
                        .filter(|_| billable_seconds > 0);
                    let currency =
                        estimate.and(route_cost.as_ref().and_then(|r| r.cost_currency.clone()));
                    (false, None, estimate, currency)
                }
            };
        let inserted = db::exec(
            &txn,
            "INSERT INTO telephony_usage_records (id, telephony_leg_id, calling_call_id, calling_call_leg_id, tenant_id, \
               application_id, environment_id, provider_id, route_id, destination_country, connected_duration_ms, \
               billable_seconds, provider_cost_minor, estimated_cost_minor, currency, cost_known, finalization_status, \
               idempotency_key, finalized_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, 'FINAL', $17, $18, $18) \
             ON CONFLICT (telephony_leg_id) DO NOTHING",
            vec![
                Uuid::new_v4().into(),
                leg.id.into(),
                leg.calling_call_id.into(),
                leg.calling_call_leg_id.into(),
                leg.tenant_id.into(),
                leg.application_id.into(),
                leg.environment_id.into(),
                leg.provider_id.clone().into(),
                leg.route_id.clone().into(),
                leg.destination_country.clone().into(),
                duration_ms.into(),
                billable_seconds.into(),
                provider_cost.into(),
                estimate.into(),
                currency.clone().into(),
                cost_known.into(),
                format!("usage-{}", leg.id).into(),
                now.into(),
            ],
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE telephony_legs SET usage_status = 'FINALIZED', updated_at = GREATEST(updated_at, $2), \
               version = version + 1 WHERE id = $1",
            vec![leg.id.into(), now.into()],
        )
        .await?;
        if inserted == 1 {
            outbox::emit_leg_fact(
                &txn,
                &leg,
                "USAGE_FINALIZED",
                json!({
                    "usage": {
                        "connectedDurationMs": duration_ms,
                        "billableSeconds": billable_seconds,
                        "costMinor": provider_cost.or(estimate),
                        "costKnown": cost_known,
                        "currency": currency,
                        "destinationCountry": leg.destination_country,
                    }
                }),
                now,
            )
            .await?;
        }
        txn.commit().await?;
        Ok(inserted == 1)
    }
}
