//! Background workers (every instance runs them; `SKIP LOCKED` leases make that safe):
//! command execution, outbox delivery to Calling, reconciliation, usage finalization,
//! duration limits, bridge expiry, provider health.

use std::time::Duration;

use serde_json::{json, Value};

use crate::{
    domain::Health, error::TelephonyResult, events::outbox, ports::PortError,
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
    pub abuse_signals: usize,
    /// Part F: edge captures delivered to Calling.
    pub captures_delivered: usize,
}

impl TelephonyService {
    /// Delivers due outbox events to Calling in per-leg order.
    pub async fn deliver_outbox_once(&self, limit: u64) -> TelephonyResult<(usize, usize)> {
        let now = self.now();
        let rows = outbox::claim_due(&self.db, now, limit).await?;
        let (mut ok, mut failed) = (0, 0);
        for row in rows {
            match self.calling.deliver_event(&row.payload).await {
                Ok(()) => {
                    outbox::mark_delivered(&self.db, row.id, self.now()).await?;
                    ok += 1;
                }
                Err(e) => {
                    let code = match &e {
                        PortError::Denied(c) => c.clone(),
                        PortError::NotFound => "NOT_FOUND".to_string(),
                        PortError::Unavailable => "UNAVAILABLE".to_string(),
                    };
                    outbox::mark_failed(&self.db, &row, &code, self.now()).await?;
                    failed += 1;
                }
            }
        }
        Ok((ok, failed))
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
        report.abuse_signals = self.abuse_scan_once().await?;
        let (delivered, failed) = self.deliver_outbox_once(100).await?;
        report.delivered = delivered;
        report.delivery_failures = failed;
        // Part F: after the leg events (a caller's hang-up reaches Calling first).
        report.captures_delivered = self.deliver_captures_once(20).await?.delivered;
        Ok(report)
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
