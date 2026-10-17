//! Background workers (every instance runs them; `SKIP LOCKED` leases make that safe):
//! command execution, outbox delivery to Calling, reconciliation, usage finalization,
//! duration limits, bridge expiry, provider health.

use std::time::Duration;

use serde_json::{json, Value};

use sea_orm::TransactionTrait;
use tracing::Instrument;

use crate::{
    domain::Health,
    error::TelephonyResult,
    events::outbox,
    incidents::{self, Incident, Severity},
    service::TelephonyService,
};

/// What one sweep did (tests and metrics).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    pub commands: usize,
    pub delivered: usize,
    pub delivery_failures: usize,
    pub reconciled: usize,
    pub usage_finalized: usize,
    pub bridges_expired: usize,
    pub evidence_purged: u64,
    /// Part H: RESOLVED incidents past their retention bound.
    pub incidents_purged: u64,
    pub abuse_signals: usize,
    /// Part F: edge captures delivered to Calling.
    pub captures_delivered: usize,
}

impl TelephonyService {
    /// Delivers due outbox events to Calling in per-leg order. A transient failure is
    /// retried with capped backoff until [`outbox::MAX_DELIVERY_ATTEMPTS`]; a permanent
    /// refusal (or the exhausted cap) dead-letters the event and raises one deduplicated
    /// `OUTBOX_DEAD_LETTER` incident per reason. Returns (delivered, failed) where failed
    /// counts retries and dead letters.
    pub async fn deliver_outbox_once(&self, limit: u64) -> TelephonyResult<(usize, usize)> {
        let now = self.now();
        let rows = outbox::claim_due(&self.db, now, limit).await?;
        let (mut ok, mut failed) = (0, 0);
        for row in rows {
            let span = tracing::info_span!(
                "telephony_outbox_delivery",
                telephony_leg_id = %row.telephony_leg_id.map(|l| l.to_string()).unwrap_or_default(),
                correlation_id = row.correlation_id.as_deref().unwrap_or(""),
                event_type = %row.event_type,
            );
            if self.deliver_outbox_row(row).instrument(span).await? {
                ok += 1;
            } else {
                failed += 1;
            }
        }
        Ok((ok, failed))
    }

    async fn deliver_outbox_row(&self, row: outbox::OutboxRow) -> TelephonyResult<bool> {
        let failure = match self
            .calling
            .deliver_event(&row.payload, row.correlation_id.as_deref())
            .await
        {
            Ok(()) => {
                outbox::mark_delivered(&self.db, row.id, self.now()).await?;
                crate::metrics::inc(
                    "telephony_outbox_deliveries_total",
                    &[("result", "DELIVERED")],
                );
                tracing::debug!("telephony event delivered to Calling");
                return Ok(true);
            }
            Err(e) => outbox::DeliveryFailure::classify(&e),
        };
        let now = self.now();
        let exhausted = row.attempts + 1 >= outbox::MAX_DELIVERY_ATTEMPTS;
        if let (outbox::DeliveryFailure::Transient(code), false) = (&failure, exhausted) {
            outbox::mark_failed(&self.db, &row, code, now).await?;
            crate::metrics::inc("telephony_outbox_deliveries_total", &[("result", "RETRY")]);
            tracing::warn!(
                code = code.as_str(),
                attempts = row.attempts + 1,
                "telephony event delivery failed; retrying"
            );
            return Ok(false);
        }
        let reason = outbox::dead_letter_reason(&failure);
        let txn = self.db.begin().await?;
        if outbox::mark_dead_letter(&txn, &row, failure.code(), now).await? {
            let mut incident = Incident::new(
                "OUTBOX_DEAD_LETTER",
                Severity::High,
                format!("outbox-dead-letter:{reason}"),
            )
            .evidence(json!({
                "reason": reason,
                "code": failure.code(),
                "eventType": row.event_type,
                "attempts": row.attempts + 1,
            }));
            incident.telephony_leg_id = row.telephony_leg_id;
            incidents::raise(&txn, &incident, now).await?;
        }
        txn.commit().await?;
        crate::metrics::inc(
            "telephony_outbox_deliveries_total",
            &[("result", "DEAD_LETTER")],
        );
        crate::metrics::inc("telephony_outbox_dead_letters_total", &[("reason", reason)]);
        tracing::warn!(
            reason,
            code = failure.code(),
            "telephony event dead-lettered; it will not be retried"
        );
        Ok(false)
    }

    /// One full sweep (deterministic for tests).
    pub async fn sweep_once(&self) -> TelephonyResult<SweepReport> {
        let mut report = SweepReport {
            commands: self.run_commands_once(50).await?.len(),
            ..Default::default()
        };
        report.reconciled = self.reconcile_once(50).await?;
        report.usage_finalized = self.finalize_usage_once(50).await?;
        report.bridges_expired = self.expire_pending_bridges().await?;
        report.evidence_purged = self.purge_expired_provider_events(500).await?;
        report.incidents_purged = self.purge_resolved_incidents(500).await?;
        report.abuse_signals = self.abuse_scan_once().await?;
        let (delivered, failed) = self.deliver_outbox_once(100).await?;
        report.delivered = delivered;
        report.delivery_failures = failed;
        // Part F: after the leg events (a caller's hang-up reaches Calling first).
        report.captures_delivered = self.deliver_captures_once(20).await?.delivered;
        Ok(report)
    }

    /// Acknowledges Numbering's configured VOICE route of every locally routable DCN
    /// (see [`crate::dcn::reconcile`]).
    pub async fn reconcile_dcn_routes(&self, limit: u64) -> TelephonyResult<u64> {
        crate::dcn::reconcile(&self.db, self.numbering.as_ref(), limit).await
    }

    /// Runs the workers forever on `interval`.
    pub async fn run_workers(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut health_tick = 0u32;
        loop {
            ticker.tick().await;
            if let Err(e) = self.sweep_once().await {
                tracing::warn!(error = %e, "telephony worker sweep failed");
            }
            health_tick = health_tick.wrapping_add(1);
            if health_tick.is_multiple_of(20) {
                if let Err(e) = self.refresh_provider_health().await {
                    tracing::warn!(error = %e, "provider health refresh failed");
                }
                // DCN voice activation continuity (Workstream L).
                if let Err(e) = self.reconcile_dcn_routes(100).await {
                    tracing::warn!(error = %e, "DCN voice route reconciliation failed");
                }
            }
        }
    }

    /// Internal readiness: database, SIP / media edge and interconnect health — counts
    /// and normalized states only (no provider names, no credentials).
    pub async fn readiness(&self) -> Value {
        use sea_orm::ConnectionTrait;
        let database = self.db.execute_unprepared("SELECT 1").await.is_ok();
        let edge = self.edge.health().await;
        let mut healthy = 0;
        let mut total = 0;
        for id in self.providers.ids() {
            total += 1;
            if let Some(p) = self.providers.get(id) {
                if matches!(p.health().await, Health::Healthy | Health::Degraded) {
                    healthy += 1;
                }
            }
        }
        let ready = database && self.flags.telephony.enabled();
        json!({
            "status": if ready { "ready" } else { "not_ready" },
            "database": if database { "ok" } else { "unavailable" },
            "telephonyEnabled": self.flags.telephony.enabled(),
            "publicOutboundEnabled": self.flags.public_outbound.enabled(),
            "publicInboundEnabled": self.flags.public_inbound.enabled(),
            "callbackRelayEnabled": self.flags.callback_relay.enabled(),
            "sipEdge": edge.as_str(),
            "interconnects": { "configured": total, "available": healthy },
        })
    }

    /// Refreshes the normalized health of every configured provider.
    pub async fn refresh_provider_health(&self) -> TelephonyResult<()> {
        self.observe_sip_edge_health().await?;
        let now = self.now();
        for id in self.providers.ids().cloned().collect::<Vec<_>>() {
            let Some(p) = self.providers.get(&id).cloned() else {
                continue;
            };
            let health = p.health().await;
            crate::db::exec(
                &self.db,
                "UPDATE telecom_providers SET health = $2, health_checked_at = $3, \
                   updated_at = GREATEST(updated_at, $3) WHERE id = $1 AND health IS DISTINCT FROM $2",
                vec![id.clone().into(), health.as_str().into(), now.into()],
            )
            .await?;
        }
        Ok(())
    }
}
