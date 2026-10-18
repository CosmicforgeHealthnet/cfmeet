//! Delivery receipts (Part IV Workstream O, Step 17).
//!
//! An authenticated, parsed receipt is de-duplicated by `(provider, event id)` and matched
//! to a message ONLY through the internal provider binding (a reference the gateway
//! recorded itself). The state machine decides: SUBMITTED / UNKNOWN → DELIVERED / FAILED,
//! UNKNOWN → SUBMITTED on an acceptance; anything that would repeat or regress a state
//! (a receipt after a final state, an acceptance after delivery) is STALE and changes
//! nothing. The provider's cost is confirmed once. An unknown reference is UNMATCHED
//! (kept as evidence; read-back resolves the message).

use sea_orm::TransactionTrait;
use uuid::Uuid;

use super::{
    dispatch,
    inbound::{record_event, recorded, CallbackAck},
    store, SmsResult, TransportRow,
};
use crate::{db, domain::RequestContext, providers::DeliveryReceipt, state::AppState};

pub async fn receive(
    state: &AppState,
    provider_id: &str,
    receipt: DeliveryReceipt,
    digest: &str,
    ctx: &RequestContext,
) -> SmsResult<CallbackAck> {
    if let Some(d) = recorded(&state.db, provider_id, &receipt.event_ref).await? {
        crate::metrics::inc(
            "sms_callbacks_total",
            &[("kind", "receipt"), ("outcome", "duplicate")],
        );
        return Ok(CallbackAck {
            status: "DUPLICATE",
            disposition: d,
        });
    }
    let now = state.now();
    let txn = state.db.begin().await?;
    #[derive(sea_orm::FromQueryResult)]
    struct Binding {
        transport_id: Uuid,
    }
    let binding = db::one::<Binding, _>(
        &txn,
        "SELECT transport_id FROM sms_provider_bindings WHERE provider_id = $1 AND provider_message_ref = $2",
        vec![provider_id.into(), receipt.provider_message_ref.clone().into()],
    )
    .await?;
    let row: Option<TransportRow> = match binding {
        Some(b) => store::lock(&txn, b.transport_id).await?,
        None => None,
    };
    let disposition = match &row {
        None => "UNMATCHED",
        Some(row) => {
            let ctx = RequestContext::continuing(ctx.correlation_id.clone(), ctx.cause());
            dispatch::apply_provider_status(
                &txn,
                row,
                receipt.status,
                receipt.error_code.as_deref(),
                receipt.cost_minor,
                &ctx,
                now,
            )
            .await?
        }
    };
    let fresh = record_event(
        &txn,
        provider_id,
        &receipt.event_ref,
        "DELIVERY_RECEIPT",
        row.as_ref().map(|r| r.id),
        Some(receipt.status.as_str()),
        disposition,
        None,
        Some(receipt.occurred_at),
        digest,
        now,
    )
    .await?;
    if fresh.is_none() {
        // A concurrent delivery of the same receipt won: nothing of this one applies.
        txn.rollback().await?;
        let d = recorded(&state.db, provider_id, &receipt.event_ref)
            .await?
            .unwrap_or_else(|| "STALE".into());
        return Ok(CallbackAck {
            status: "DUPLICATE",
            disposition: d,
        });
    }
    txn.commit().await?;
    let label = match disposition {
        "APPLIED" => "applied",
        "STALE" => "stale",
        _ => "unmatched",
    };
    crate::metrics::inc(
        "sms_callbacks_total",
        &[("kind", "receipt"), ("outcome", label)],
    );
    Ok(CallbackAck {
        status: "ACCEPTED",
        disposition: disposition.to_string(),
    })
}
