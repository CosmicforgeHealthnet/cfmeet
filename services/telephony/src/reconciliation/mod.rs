//! The Telephony reconciler (Part E §48, §50, §134–§140).
//!
//! Multi-instance safe: legs due for reconciliation are leased (`reconcile_lease_until`,
//! `FOR UPDATE SKIP LOCKED`), processed in bounded batches with backoff, and every state
//! change is version-checked — two reconcilers looking at the same leg produce exactly
//! one effective repair. Each decision is recorded in `telephony_reconciliation_runs`.
//!
//! Repairs happen only when the outcome is unambiguous (provider read-back says ended /
//! absent / active). Anything else stays as it is and becomes (or escalates) an
//! incident; an UNKNOWN origination is never assumed failed or succeeded.
//!
//! Detected drift: UNKNOWN origination / termination outcomes, legs ringing beyond the
//! ring timeout, CONNECTED legs the provider already ended (event gaps), late provider
//! success after a terminal leg (orphan provider call), orphan bridges, legs beyond their
//! maximum duration, missing usage finalization.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

use crate::{
    db,
    domain::{EndReason, LegStatus, NormalizedEvent},
    error::TelephonyResult,
    incidents::{self, Incident, Severity},
    legs::{self, Change, LegRow, LEG_COLUMNS},
    providers::{ControlOutcome, QueryKey, QueryOutcome},
    service::TelephonyService,
};

/// Live legs with no provider news for this long get a read-back (event-gap check).
pub const EVENT_GAP_SECONDS: i64 = 600;
/// How often an instance reads back the interconnects' call inventory.
pub const INVENTORY_EVERY_SECONDS: i64 = 60;

#[derive(Debug, FromQueryResult)]
struct BindingRow {
    id: Uuid,
    provider_id: String,
    provider_call_ref: Option<String>,
    correlation_key: String,
    sent_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl TelephonyService {
    async fn record_run<C: ConnectionTrait>(
        &self,
        db_: &C,
        leg: &LegRow,
        case_code: &'static str,
        observed: &'static str,
        action: &'static str,
    ) -> TelephonyResult<()> {
        crate::metrics::inc(
            "telephony_reconciliation_runs_total",
            &[("case", case_code), ("action", action)],
        );
        db::exec(
            db_,
            "INSERT INTO telephony_reconciliation_runs (id, telephony_leg_id, case_code, observed, action, worker_id, decided_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            vec![
                Uuid::new_v4().into(),
                leg.id.into(),
                case_code.into(),
                observed.into(),
                action.into(),
                self.worker_id.clone().into(),
                self.now().into(),
            ],
        )
        .await
        .map(|_| ())
    }

    /// Flags drift cases (cheap scans) and reconciles due legs.
    pub async fn reconcile_once(&self, limit: u64) -> TelephonyResult<usize> {
        let now = self.now();
        let ring = Duration::from_std(self.settings.ring_timeout).unwrap_or(Duration::seconds(60));
        // Ringing beyond the ring timeout; live legs with an event gap.
        db::exec(
            &self.db,
            "UPDATE telephony_legs SET reconcile_after = $1 WHERE reconcile_after IS NULL AND ( \
               (status IN ('ORIGINATING', 'RINGING') AND COALESCE(ringing_at, originating_at, created_at) < $2) \
               OR (status IN ('ANSWERED', 'CONNECTED', 'HELD') AND updated_at < $3))",
            vec![
                now.into(),
                (now - ring).into(),
                (now - Duration::seconds(EVENT_GAP_SECONDS)).into(),
            ],
        )
        .await?;
        // Independent scans: one failing never stalls the others or leg reconciliation.
        if let Err(e) = self.enforce_duration_limits().await {
            tracing::warn!(error = %e, "duration limit scan failed");
        }
        if let Err(e) = self.repair_orphan_bridges().await {
            tracing::warn!(error = %e, "orphan bridge scan failed");
        }
        if let Err(e) = self.reconcile_inventory().await {
            tracing::warn!(error = %e, "provider inventory scan failed");
        }
        let due: Vec<LegRow> = db::all(
            &self.db,
            &format!(
                "UPDATE telephony_legs SET reconcile_lease_until = $1, reconcile_attempts = reconcile_attempts + 1 \
                 WHERE id IN (SELECT id FROM telephony_legs WHERE reconcile_after <= $2 \
                   AND (reconcile_lease_until IS NULL OR reconcile_lease_until < $2) \
                   ORDER BY reconcile_after LIMIT $3 FOR UPDATE SKIP LOCKED) RETURNING {LEG_COLUMNS}"
            ),
            vec![
                (now + Duration::seconds(30)).into(),
                now.into(),
                (limit.clamp(1, 200) as i64).into(),
            ],
        )
        .await?;
        let n = due.len();
        for leg in due {
            if let Err(e) = self.reconcile_leg(&leg).await {
                tracing::warn!(telephony_leg_id = %leg.id, error = %e, "reconciliation of a leg failed");
            }
        }
        Ok(n)
    }

    async fn latest_binding(&self, leg: &LegRow) -> TelephonyResult<Option<BindingRow>> {
        db::one(
            &self.db,
            "SELECT id, provider_id, provider_call_ref, correlation_key, sent_at FROM provider_call_bindings \
             WHERE telephony_leg_id = $1 AND status <> 'PREPARED' ORDER BY created_at DESC LIMIT 1",
            vec![leg.id.into()],
        )
        .await
    }

    async fn reschedule_reconcile(
        &self,
        leg: &LegRow,
        escalate_key: Option<String>,
    ) -> TelephonyResult<()> {
        let now = self.now();
        let base =
            Duration::from_std(self.settings.reconcile_delay).unwrap_or(Duration::seconds(5));
        let cap = Duration::from_std(self.settings.reconcile_backoff_cap)
            .unwrap_or(Duration::seconds(300));
        let delay = (base * 2_i32.pow(leg.reconcile_attempts.clamp(0, 10) as u32)).min(cap);
        db::exec(
            &self.db,
            "UPDATE telephony_legs SET reconcile_after = $2, reconcile_lease_until = NULL WHERE id = $1",
            vec![leg.id.into(), (now + delay).into()],
        )
        .await?;
        if let Some(key) = escalate_key {
            if leg.reconcile_attempts >= self.settings.reconcile_escalate_after {
                incidents::escalate(&self.db, &key, now).await?;
            }
        }
        Ok(())
    }

    async fn clear_reconcile(&self, leg_id: Uuid) -> TelephonyResult<()> {
        db::exec(
            &self.db,
            "UPDATE telephony_legs SET reconcile_after = NULL, reconcile_lease_until = NULL WHERE id = $1",
            vec![leg_id.into()],
        )
        .await
        .map(|_| ())
    }

    /// Reconciles one leased leg.
    pub async fn reconcile_leg(&self, leg: &LegRow) -> TelephonyResult<()> {
        let status = leg.status()?;
        let Some(binding) = self.latest_binding(leg).await? else {
            // Nothing was ever sent for this leg.
            if status == LegStatus::Unknown {
                return self.resolve_unknown(leg, QueryOutcome::Absent, None).await;
            }
            return self.clear_reconcile(leg.id).await;
        };
        let Some(provider) = self.providers.get(&binding.provider_id).cloned() else {
            return self.reschedule_reconcile(leg, None).await;
        };
        let outcome = provider
            .query_call(&QueryKey {
                provider_call_ref: binding.provider_call_ref.clone(),
                correlation_key: binding.correlation_key.clone(),
                transmitted_at: binding.sent_at,
            })
            .await;
        match status {
            LegStatus::Unknown => self.resolve_unknown(leg, outcome, Some(&binding)).await,
            LegStatus::Ended | LegStatus::Failed => {
                // Late success after terminal: a provider call still alive is an orphan.
                if let QueryOutcome::Active {
                    provider_call_ref, ..
                } = &outcome
                {
                    let result = provider.terminate(provider_call_ref).await;
                    self.record_run(
                        &self.db,
                        leg,
                        "TERMINAL_CALL_ACTIVE_LEG",
                        "PROVIDER_ACTIVE",
                        if matches!(result, ControlOutcome::Done | ControlOutcome::AlreadyGone) {
                            "TERMINATED_ORPHAN"
                        } else {
                            "RETRY_SCHEDULED"
                        },
                    )
                    .await?;
                    if !matches!(result, ControlOutcome::Done | ControlOutcome::AlreadyGone) {
                        return self.reschedule_reconcile(leg, None).await;
                    }
                }
                self.clear_reconcile(leg.id).await
            }
            LegStatus::Originating | LegStatus::Ringing => {
                let ring =
                    Duration::from_std(self.settings.ring_timeout).unwrap_or(Duration::seconds(60));
                let since = leg
                    .ringing_at
                    .or(leg.originating_at)
                    .unwrap_or(leg.created_at);
                match outcome {
                    QueryOutcome::Active {
                        status: s,
                        provider_call_ref,
                    } => {
                        if matches!(s, LegStatus::Answered | LegStatus::Connected) {
                            self.repair(
                                leg,
                                Change::to(
                                    LegStatus::Answered,
                                    NormalizedEvent::LegAnswered,
                                    self.now(),
                                ),
                                "EVENT_GAP",
                                "PROVIDER_ANSWERED",
                                "MARKED_ACTIVE",
                            )
                            .await
                        } else if self.now() - since >= ring {
                            let result = provider.terminate(&provider_call_ref).await;
                            if matches!(result, ControlOutcome::Done | ControlOutcome::AlreadyGone)
                            {
                                self.repair(
                                    leg,
                                    Change::ended(
                                        NormalizedEvent::LegNoAnswer,
                                        EndReason::NoAnswer,
                                        self.now(),
                                    ),
                                    "RINGING_TIMEOUT",
                                    "PROVIDER_RINGING",
                                    "MARKED_FAILED",
                                )
                                .await
                            } else {
                                self.reschedule_reconcile(leg, None).await
                            }
                        } else {
                            self.reschedule_reconcile(leg, None).await
                        }
                    }
                    QueryOutcome::Ended { reason, .. } => {
                        let event = match reason {
                            EndReason::Busy => NormalizedEvent::LegBusy,
                            EndReason::NoAnswer => NormalizedEvent::LegNoAnswer,
                            EndReason::Declined => NormalizedEvent::LegDeclined,
                            EndReason::Completed => NormalizedEvent::LegEnded,
                            _ => NormalizedEvent::LegFailed,
                        };
                        self.repair(
                            leg,
                            Change::ended(event, reason, self.now()),
                            "RINGING_TIMEOUT",
                            "PROVIDER_ENDED",
                            "MARKED_ENDED",
                        )
                        .await
                    }
                    QueryOutcome::Absent => {
                        self.repair(
                            leg,
                            Change::ended(
                                NormalizedEvent::LegFailed,
                                EndReason::AbsentAtProvider,
                                self.now(),
                            ),
                            "RINGING_TIMEOUT",
                            "PROVIDER_ABSENT",
                            "MARKED_FAILED",
                        )
                        .await
                    }
                    QueryOutcome::Unknown => self.reschedule_reconcile(leg, None).await,
                }
            }
            LegStatus::Ending => self.reconcile_termination(leg, &*provider, outcome).await,
            LegStatus::Answered | LegStatus::Connected | LegStatus::Held => {
                match outcome {
                    QueryOutcome::Ended { reason, .. } => {
                        let event = if reason == EndReason::Completed {
                            NormalizedEvent::LegEnded
                        } else {
                            NormalizedEvent::LegFailed
                        };
                        let reason = if reason == EndReason::Completed || leg.answered_at.is_some()
                        {
                            EndReason::Completed
                        } else {
                            reason
                        };
                        self.repair(
                            leg,
                            Change::ended(event, reason, self.now()),
                            "CONNECTED_PROVIDER_ENDED",
                            "PROVIDER_ENDED",
                            "MARKED_ENDED",
                        )
                        .await
                    }
                    QueryOutcome::Active { .. } => {
                        self.record_run(&self.db, leg, "EVENT_GAP", "PROVIDER_ACTIVE", "NO_CHANGE")
                            .await?;
                        self.clear_reconcile(leg.id).await
                    }
                    QueryOutcome::Absent => {
                        // We believe it is live, the provider has no record: ambiguous.
                        incidents::raise(
                            &self.db,
                            &Incident::new(
                                "CONNECTED_BUT_PROVIDER_ENDED",
                                Severity::High,
                                format!("connected-absent:{}", leg.id),
                            )
                            .leg(leg)
                            .evidence(json!({ "readBack": "ABSENT" })),
                            self.now(),
                        )
                        .await?;
                        self.record_run(
                            &self.db,
                            leg,
                            "CONNECTED_PROVIDER_ENDED",
                            "PROVIDER_ABSENT",
                            "INCIDENT_OPENED",
                        )
                        .await?;
                        self.reschedule_reconcile(leg, Some(format!("connected-absent:{}", leg.id)))
                            .await
                    }
                    QueryOutcome::Unknown => self.reschedule_reconcile(leg, None).await,
                }
            }
            LegStatus::Planned => self.clear_reconcile(leg.id).await,
        }
    }

    /// An unresolved termination (leg ENDING): the provider says ended or has no call ->
    /// ENDED (unambiguous: the intent was to end it); still active -> terminate again
    /// (idempotent); unknown -> keep reading back with backoff and escalate.
    async fn reconcile_termination(
        &self,
        leg: &LegRow,
        provider: &dyn crate::providers::TelecomInterconnectProvider,
        outcome: QueryOutcome,
    ) -> TelephonyResult<()> {
        let key = format!("unknown-termination:{}", leg.id);
        let observed = match &outcome {
            QueryOutcome::Ended { .. } => "PROVIDER_ENDED",
            QueryOutcome::Absent => "PROVIDER_ABSENT",
            QueryOutcome::Active {
                provider_call_ref, ..
            } => match provider.terminate(provider_call_ref).await {
                ControlOutcome::Done | ControlOutcome::AlreadyGone => "TERMINATED_ON_RETRY",
                _ => {
                    self.record_run(
                        &self.db,
                        leg,
                        "UNKNOWN_TERMINATION",
                        "PROVIDER_ACTIVE",
                        "RETRY_SCHEDULED",
                    )
                    .await?;
                    return self.reschedule_reconcile(leg, Some(key)).await;
                }
            },
            QueryOutcome::Unknown => {
                self.record_run(
                    &self.db,
                    leg,
                    "UNKNOWN_TERMINATION",
                    "READ_BACK_UNKNOWN",
                    "RETRY_SCHEDULED",
                )
                .await?;
                return self.reschedule_reconcile(leg, Some(key)).await;
            }
        };
        let reason = if leg.answered_at.is_some() {
            EndReason::Completed
        } else {
            EndReason::Cancelled
        };
        self.repair(
            leg,
            Change::ended(NormalizedEvent::LegEnded, reason, self.now()),
            "UNKNOWN_TERMINATION",
            observed,
            "MARKED_ENDED",
        )
        .await?;
        let now = self.now();
        let txn = self.db.begin().await?;
        db::exec(
            &txn,
            "UPDATE telephony_commands SET status = 'RECONCILING', updated_at = GREATEST(updated_at, $2) \
             WHERE telephony_leg_id = $1 AND kind = 'TERMINATE' AND status = 'UNKNOWN_OUTCOME'",
            vec![leg.id.into(), now.into()],
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE telephony_commands SET status = 'SUCCEEDED', result_code = $2, finished_at = GREATEST(created_at, $3), \
               updated_at = GREATEST(updated_at, $3) WHERE telephony_leg_id = $1 AND kind = 'TERMINATE' AND status = 'RECONCILING'",
            vec![leg.id.into(), observed.into(), now.into()],
        )
        .await?;
        incidents::resolve(&txn, &key, now).await?;
        txn.commit().await?;
        Ok(())
    }

    /// Applies an unambiguous repair (version-checked: a concurrent reconciler's repair
    /// wins and this one changes nothing).
    async fn repair(
        &self,
        leg: &LegRow,
        change: Change,
        case_code: &'static str,
        observed: &'static str,
        action: &'static str,
    ) -> TelephonyResult<()> {
        let txn = self.db.begin().await?;
        let locked = legs::lock(&txn, leg.id).await?;
        if locked.version != leg.version {
            // Another worker (or an event) moved it meanwhile: re-evaluate later.
            drop(txn);
            return self.clear_reconcile(leg.id).await;
        }
        let applied = legs::transition_in(&txn, &locked, &change).await?;
        self.record_run(
            &txn,
            leg,
            case_code,
            observed,
            if applied.is_some() {
                action
            } else {
                "NO_CHANGE"
            },
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE telephony_legs SET reconcile_after = NULL, reconcile_lease_until = NULL WHERE id = $1",
            vec![leg.id.into()],
        )
        .await?;
        txn.commit().await?;
        if applied.is_some() && change.target.is_terminal() {
            self.bridge_member_left(Some(leg.id), None).await?;
        }
        Ok(())
    }

    /// Resolves an UNKNOWN origination from provider read-back. Unknown stays unknown.
    async fn resolve_unknown(
        &self,
        leg: &LegRow,
        outcome: QueryOutcome,
        binding: Option<&BindingRow>,
    ) -> TelephonyResult<()> {
        let now = self.now();
        let key = format!("unknown-origination:{}", leg.id);
        if outcome == QueryOutcome::Unknown {
            db::exec(
                &self.db,
                "UPDATE telephony_commands SET status = 'UNKNOWN_OUTCOME', updated_at = GREATEST(updated_at, $2) \
                 WHERE telephony_leg_id = $1 AND kind = 'ORIGINATE' AND status = 'RECONCILING'",
                vec![leg.id.into(), now.into()],
            )
            .await?;
            self.record_run(
                &self.db,
                leg,
                "UNKNOWN_ORIGINATION",
                "READ_BACK_UNKNOWN",
                "RETRY_SCHEDULED",
            )
            .await?;
            return self.reschedule_reconcile(leg, Some(key)).await;
        }
        let txn = self.db.begin().await?;
        let locked = legs::lock(&txn, leg.id).await?;
        if locked.status()? != LegStatus::Unknown || locked.version != leg.version {
            return Ok(());
        }
        db::exec(
            &txn,
            "UPDATE telephony_commands SET status = 'RECONCILING', updated_at = GREATEST(updated_at, $2) \
             WHERE telephony_leg_id = $1 AND kind = 'ORIGINATE' AND status = 'UNKNOWN_OUTCOME'",
            vec![leg.id.into(), now.into()],
        )
        .await?;
        let (change, command_status, observed, action) = match &outcome {
            QueryOutcome::Active {
                provider_call_ref,
                status,
            } => {
                if let Some(b) = binding {
                    bind(&txn, b.id, provider_call_ref, now).await?;
                }
                let (target, event) = match status {
                    LegStatus::Answered => (LegStatus::Answered, NormalizedEvent::LegAnswered),
                    LegStatus::Connected | LegStatus::Held => {
                        (LegStatus::Answered, NormalizedEvent::LegAnswered)
                    }
                    _ => (LegStatus::Ringing, NormalizedEvent::LegRinging),
                };
                (
                    Change::to(target, event, now),
                    "SUCCEEDED",
                    "PROVIDER_ACTIVE",
                    "RESOLVED_ACCEPTED",
                )
            }
            QueryOutcome::Ended {
                provider_call_ref,
                reason,
            } => {
                if let Some(b) = binding {
                    bind(&txn, b.id, provider_call_ref, now).await?;
                }
                let event = match reason {
                    EndReason::Busy => NormalizedEvent::LegBusy,
                    EndReason::NoAnswer => NormalizedEvent::LegNoAnswer,
                    EndReason::Declined => NormalizedEvent::LegDeclined,
                    EndReason::Completed => NormalizedEvent::LegEnded,
                    _ => NormalizedEvent::LegFailed,
                };
                (
                    Change::ended(event, *reason, now),
                    "SUCCEEDED",
                    "PROVIDER_ENDED",
                    "MARKED_ENDED",
                )
            }
            QueryOutcome::Absent | QueryOutcome::Unknown => {
                if let Some(b) = binding {
                    db::exec(
                        &txn,
                        "UPDATE provider_call_bindings SET status = 'ABSENT', updated_at = GREATEST(updated_at, $2) \
                         WHERE id = $1 AND status IN ('PREPARED', 'SENT')",
                        vec![b.id.into(), now.into()],
                    )
                    .await?;
                }
                (
                    Change::ended(NormalizedEvent::LegFailed, EndReason::AbsentAtProvider, now),
                    "FAILED",
                    "PROVIDER_ABSENT",
                    "RESOLVED_ABSENT",
                )
            }
        };
        legs::transition_in(&txn, &locked, &change).await?;
        db::exec(
            &txn,
            "UPDATE telephony_commands SET status = $2, result_code = $3, finished_at = GREATEST(created_at, $4), \
               updated_at = GREATEST(updated_at, $4), failure_category = CASE WHEN $2 = 'FAILED' THEN 'EXECUTION' END \
             WHERE telephony_leg_id = $1 AND kind = 'ORIGINATE' AND status = 'RECONCILING'",
            vec![leg.id.into(), command_status.into(), action.into(), now.into()],
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE telephony_legs SET reconcile_after = NULL, reconcile_lease_until = NULL WHERE id = $1",
            vec![leg.id.into()],
        )
        .await?;
        self.record_run(&txn, leg, "UNKNOWN_ORIGINATION", observed, action)
            .await?;
        incidents::resolve(&txn, &key, now).await?;
        txn.commit().await?;
        tracing::info!(telephony_leg_id = %leg.id, observed, action, "unknown origination reconciled");
        Ok(())
    }

    /// Terminates live legs beyond their maximum duration (durable: the scan repeats).
    pub async fn enforce_duration_limits(&self) -> TelephonyResult<usize> {
        let now = self.now();
        let over: Vec<LegRow> = db::all(
            &self.db,
            &format!(
                "SELECT {LEG_COLUMNS} FROM telephony_legs WHERE status IN ('ANSWERED', 'CONNECTED', 'HELD') \
                 AND answered_at + make_interval(secs => max_duration_seconds) <= $1 LIMIT 50"
            ),
            vec![now.into()],
        )
        .await?;
        for leg in &over {
            if let Some(b) = self.latest_binding(leg).await? {
                if let (Some(p), Some(r)) = (
                    self.providers.get(&b.provider_id).cloned(),
                    b.provider_call_ref.clone(),
                ) {
                    let result = p.terminate(&r).await;
                    if !matches!(result, ControlOutcome::Done | ControlOutcome::AlreadyGone) {
                        continue;
                    }
                }
            }
            self.repair(
                leg,
                Change::ended(NormalizedEvent::LegEnded, EndReason::DurationLimit, now),
                "DURATION_LIMIT",
                "MAX_DURATION_REACHED",
                "MARKED_ENDED",
            )
            .await?;
            incidents::raise(
                &self.db,
                &Incident::new(
                    "DURATION_LIMIT_ENFORCED",
                    Severity::Low,
                    format!("duration:{}", leg.id),
                )
                .leg(leg),
                now,
            )
            .await?;
        }
        Ok(over.len())
    }

    /// Provider calls without a valid internal binding (Part E §135): every interconnect
    /// that can list its live calls is read back (at most every
    /// `INVENTORY_EVERY_SECONDS` per instance). A live call bound to a terminal leg is an
    /// orphan and is terminated (unambiguous); a call matching no binding / bridge member
    /// at all is NOT guessed at — it becomes a PROVIDER_ACTIVE_WITHOUT_BINDING incident (restricted
    /// evidence), resolved automatically once it is gone or bound.
    pub async fn reconcile_inventory(&self) -> TelephonyResult<usize> {
        let now = self.now();
        {
            let mut last = self
                .last_inventory
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if last.is_some_and(|at| now - at < Duration::seconds(INVENTORY_EVERY_SECONDS)) {
                return Ok(0);
            }
            *last = Some(now);
        }
        let mut seen = std::collections::HashSet::new();
        let mut unbound: Vec<String> = Vec::new();
        let mut listed: Vec<String> = Vec::new();
        let mut repaired = 0;
        for id in self.providers.ids().cloned().collect::<Vec<_>>() {
            let Some(provider) = self.providers.get(&id).cloned() else {
                continue;
            };
            let Some(calls) = provider.list_active().await else {
                continue;
            };
            listed.push(id.clone());
            for call in calls {
                // Interconnects sharing one edge list the same channels: once each.
                if !seen.insert(call.provider_call_ref.clone()) {
                    continue;
                }
                let bound: Option<InventoryMatch> = db::one(
                    &self.db,
                    "SELECT l.id AS telephony_leg_id, l.status, b.provider_id FROM provider_call_bindings b \
                     JOIN telephony_legs l ON l.id = b.telephony_leg_id \
                     WHERE b.provider_call_ref = $1 OR ($2::text IS NOT NULL AND b.correlation_key = $2) \
                     ORDER BY b.created_at DESC LIMIT 1",
                    vec![
                        call.provider_call_ref.clone().into(),
                        call.correlation_key.clone().into(),
                    ],
                )
                .await?;
                match bound {
                    Some(m) if matches!(m.status.as_str(), "ENDED" | "FAILED") => {
                        let owner = self
                            .providers
                            .get(&m.provider_id)
                            .cloned()
                            .unwrap_or(provider.clone());
                        let result = owner.terminate(&call.provider_call_ref).await;
                        if let Some(leg) = legs::load(&self.db, m.telephony_leg_id).await? {
                            self.record_run(
                                &self.db,
                                &leg,
                                "TERMINAL_CALL_ACTIVE_LEG",
                                "PROVIDER_ACTIVE",
                                if matches!(
                                    result,
                                    ControlOutcome::Done | ControlOutcome::AlreadyGone
                                ) {
                                    "TERMINATED_ORPHAN"
                                } else {
                                    "RETRY_SCHEDULED"
                                },
                            )
                            .await?;
                        }
                        repaired += 1;
                    }
                    Some(_) => {}
                    None => {
                        let member = db::one::<InventoryMember, _>(
                            &self.db,
                            "SELECT bridge_id FROM telephony_bridge_legs WHERE edge_channel_ref = $1 LIMIT 1",
                            vec![call.provider_call_ref.clone().into()],
                        )
                        .await?;
                        if member.is_some() {
                            continue;
                        }
                        let key = format!("unbound-call:{id}:{}", call.provider_call_ref);
                        let mut incident = Incident::new(
                            "PROVIDER_ACTIVE_WITHOUT_BINDING",
                            Severity::Medium,
                            key.clone(),
                        )
                        .evidence(json!({
                            "providerCallRef": call.provider_call_ref,
                            "status": call.status.as_str(),
                        }));
                        incident.provider_id = Some(id.clone());
                        incidents::raise(&self.db, &incident, now).await?;
                        unbound.push(key);
                    }
                }
            }
        }
        // Self-healing: an unbound call that is gone or bound now closes its incident.
        let open: Vec<OpenIncident> = db::all(
            &self.db,
            "SELECT dedupe_key FROM telephony_incidents WHERE category = 'PROVIDER_ACTIVE_WITHOUT_BINDING' \
             AND status = 'OPEN' AND provider_id = ANY($1)",
            vec![listed.into()],
        )
        .await?;
        for incident in open {
            if !unbound.contains(&incident.dedupe_key) {
                incidents::resolve(&self.db, &incident.dedupe_key, now).await?;
            }
        }
        Ok(repaired)
    }

    /// Active bridges with a terminal member are orphans: end them.
    async fn repair_orphan_bridges(&self) -> TelephonyResult<usize> {
        let rows: Vec<BridgeId> = db::all(
            &self.db,
            "SELECT DISTINCT b.id FROM telephony_bridges b JOIN telephony_bridge_legs m ON m.bridge_id = b.id \
             JOIN telephony_legs l ON l.id = m.telephony_leg_id \
             WHERE b.status IN ('PENDING', 'ACTIVE') AND l.status IN ('ENDED', 'FAILED') LIMIT 50",
            vec![],
        )
        .await?;
        for r in &rows {
            if let Err(e) = self.finish_bridge(r.id, "ORPHANED", self.now()).await {
                tracing::warn!(bridge_id = %r.id, error = %e, "ending an orphan bridge failed");
            }
        }
        Ok(rows.len())
    }
}

async fn bind<C: ConnectionTrait>(
    txn: &C,
    binding: Uuid,
    provider_call_ref: &str,
    now: DateTime<Utc>,
) -> TelephonyResult<()> {
    db::exec(
        txn,
        "UPDATE provider_call_bindings SET provider_call_ref = COALESCE(provider_call_ref, $2), status = 'BOUND', \
           bound_at = COALESCE(bound_at, $3), updated_at = GREATEST(updated_at, $3) \
         WHERE id = $1 AND status IN ('PREPARED', 'SENT', 'BOUND')",
        vec![binding.into(), provider_call_ref.into(), now.into()],
    )
    .await
    .map(|_| ())
}

#[derive(Debug, FromQueryResult)]
struct InventoryMatch {
    telephony_leg_id: Uuid,
    status: String,
    provider_id: String,
}

#[derive(Debug, FromQueryResult)]
struct InventoryMember {
    #[allow(dead_code)]
    bridge_id: Uuid,
}

#[derive(Debug, FromQueryResult)]
struct OpenIncident {
    dedupe_key: String,
}

#[derive(Debug, FromQueryResult)]
struct BridgeId {
    id: Uuid,
}
