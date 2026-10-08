//! Command execution workers (Part E §34, §48, §75, §120).
//!
//! Commands are claimed with a lease (`FOR UPDATE SKIP LOCKED`; any number of
//! instances). An ORIGINATE runs in fenced steps:
//!
//! 1. checks (leg not cancelled, kill switches, authority still valid, grant valid);
//! 2. route choice + capacity reservation + correlation key persisted (one transaction);
//! 3. protected destination resolved from Privacy Relay with the grant — in memory only;
//!    the caller identity is the session's relay identity Privacy Relay selected, and it
//!    must be bound to the chosen interconnect;
//! 4. the TRANSMIT FENCE is committed (`transmitted_at`, binding SENT, leg ORIGINATING);
//!    if the database is unavailable here no provider request is ever sent;
//! 5. the provider request (bounded by a deadline);
//! 6. the outcome is persisted: ACCEPTED, DEFINITE_FAILURE or UNKNOWN_OUTCOME.
//!
//! A worker that dies holding a lease is recovered: before the fence the command is
//! safely retried; after it the outcome is UNKNOWN and goes to reconciliation — never a
//! blind retry (also enforced by a database trigger).

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{CommandRow, COMMAND_COLUMNS};
use crate::{
    db,
    domain::{
        CommandKind, EndReason, FailureCategory, LegStatus, NormalizedEvent, PublicRouteClass,
        RouteCapability,
    },
    error::{ErrorCode, TelephonyError, TelephonyResult},
    events::outbox,
    incidents::{self, Incident, Severity},
    legs::{self, Change, LegRow},
    ports::{PortError, ResolveForExecution},
    providers::{
        ControlOutcome, OriginateOutcome, OriginateRequest, QueryKey, QueryOutcome, UnknownReason,
    },
    service::TelephonyService,
};

/// Result of one execution step (for tests and metrics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Executed {
    Accepted,
    DefiniteFailure(&'static str),
    Unknown,
    Rescheduled,
    Done,
    Skipped,
}

fn correlation_key() -> String {
    format!("cfm-{}", Uuid::new_v4().simple())
}

impl TelephonyService {
    /// One worker pass: recover stale leases, then execute due commands.
    pub async fn run_commands_once(&self, limit: u64) -> TelephonyResult<Vec<(Uuid, Executed)>> {
        self.recover_stale_commands(limit).await?;
        let now = self.now();
        let claimed: Vec<CommandRow> = db::all(
            &self.db,
            &format!(
                "UPDATE telephony_commands SET status = 'CLAIMED', lease_owner = $1, \
                   lease_until = $2, attempts = attempts + 1, updated_at = GREATEST(updated_at, $3) \
                 WHERE id IN (SELECT id FROM telephony_commands WHERE status = 'PENDING' AND next_attempt_at <= $3 \
                   ORDER BY next_attempt_at, created_at LIMIT $4 FOR UPDATE SKIP LOCKED) \
                 RETURNING {COMMAND_COLUMNS}"
            ),
            vec![
                self.worker_id.clone().into(),
                (now + Duration::from_std(self.settings.command_lease).unwrap_or(Duration::seconds(30))).into(),
                now.into(),
                (limit.clamp(1, 200) as i64).into(),
            ],
        )
        .await?;
        let mut out = Vec::new();
        for command in claimed {
            let id = command.id;
            let result = match CommandKind::parse(&command.kind) {
                Some(CommandKind::Originate) => self.execute_originate(command).await,
                Some(CommandKind::Terminate) => {
                    self.execute_control(command, CommandKind::Terminate).await
                }
                Some(CommandKind::Hold) => self.execute_control(command, CommandKind::Hold).await,
                Some(CommandKind::Resume) => {
                    self.execute_control(command, CommandKind::Resume).await
                }
                Some(other) => self.execute_bridge_command(command, other).await,
                None => Ok(Executed::Skipped),
            };
            match result {
                Ok(r) => out.push((id, r)),
                Err(e) => {
                    // Leave the lease to expire: recovery decides (pre- or post-fence).
                    tracing::warn!(command_id = %id, error = %e, "command execution interrupted");
                }
            }
        }
        Ok(out)
    }

    /// Recovers commands whose lease expired (their worker died or lost the database).
    pub async fn recover_stale_commands(&self, limit: u64) -> TelephonyResult<usize> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let stale: Vec<CommandRow> = db::all(
            &txn,
            &format!(
                "SELECT {COMMAND_COLUMNS} FROM telephony_commands WHERE status IN ('CLAIMED', 'RUNNING') \
                 AND lease_until < $1 ORDER BY lease_until LIMIT $2 FOR UPDATE SKIP LOCKED"
            ),
            vec![now.into(), (limit.clamp(1, 200) as i64).into()],
        )
        .await?;
        for c in &stale {
            if c.transmitted_at.is_none() {
                // Nothing left this process: a retry is safe.
                db::exec(
                    &txn,
                    "UPDATE telephony_commands SET status = 'PENDING', lease_owner = NULL, lease_until = NULL, \
                       next_attempt_at = $2, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
                    vec![c.id.into(), now.into()],
                )
                .await?;
            } else {
                // The request may have reached the provider: UNKNOWN, reconcile first.
                self.mark_unknown_in(&txn, c, UnknownReason::WorkerInterrupted, now)
                    .await?;
            }
        }
        txn.commit().await?;
        if !stale.is_empty() {
            tracing::warn!(
                count = stale.len(),
                "recovered commands with expired leases"
            );
        }
        Ok(stale.len())
    }

    async fn finish_command<C: ConnectionTrait>(
        &self,
        db_: &C,
        id: Uuid,
        status: &str,
        result_code: &str,
        category: Option<FailureCategory>,
        now: DateTime<Utc>,
    ) -> TelephonyResult<()> {
        let finished = matches!(status, "SUCCEEDED" | "FAILED" | "DEAD_LETTER");
        db::exec(
            db_,
            "UPDATE telephony_commands SET status = $2, result_code = $3, failure_category = $4, \
               grant_secret = NULL, lease_owner = NULL, lease_until = NULL, updated_at = GREATEST(updated_at, $5), \
               finished_at = CASE WHEN $6 THEN GREATEST(created_at, $5) ELSE finished_at END WHERE id = $1",
            vec![
                id.into(),
                status.into(),
                result_code.into(),
                category.map(|c| c.as_str()).into(),
                now.into(),
                finished.into(),
            ],
        )
        .await
        .map(|_| ())
    }

    /// Fails an ORIGINATE whose request NEVER left this process (safe, definite).
    async fn fail_before_send(
        &self,
        command: &CommandRow,
        category: FailureCategory,
        code: &'static str,
    ) -> TelephonyResult<Executed> {
        let now = self.now();
        let txn = self.db.begin().await?;
        if let Some(leg_id) = command.telephony_leg_id {
            let leg = legs::lock(&txn, leg_id).await?;
            if !leg.status()?.is_terminal() {
                legs::transition_in(
                    &txn,
                    &leg,
                    &Change::ended(NormalizedEvent::LegFailed, EndReason::NotSent, now)
                        .category(category),
                )
                .await?;
            }
            db::exec(
                &txn,
                "UPDATE provider_call_bindings SET status = 'ABSENT', updated_at = GREATEST(updated_at, $2) \
                 WHERE telephony_leg_id = $1 AND status = 'PREPARED'",
                vec![leg_id.into(), now.into()],
            )
            .await?;
        }
        self.finish_command(&txn, command.id, "FAILED", code, Some(category), now)
            .await?;
        txn.commit().await?;
        tracing::info!(command_id = %command.id, code, "origination failed before transmission");
        Ok(Executed::DefiniteFailure(code))
    }

    async fn reschedule(&self, command: &CommandRow, code: &str) -> TelephonyResult<Executed> {
        let now = self.now();
        db::exec(
            &self.db,
            "UPDATE telephony_commands SET status = 'PENDING', lease_owner = NULL, lease_until = NULL, \
               next_attempt_at = $2, result_code = $3, updated_at = GREATEST(updated_at, $4) \
             WHERE id = $1 AND transmitted_at IS NULL",
            vec![
                command.id.into(),
                (now + Self::retry_delay(command.attempts)).into(),
                code.into(),
                now.into(),
            ],
        )
        .await?;
        Ok(Executed::Rescheduled)
    }

    async fn execute_originate(&self, command: CommandRow) -> TelephonyResult<Executed> {
        let now = self.now();
        let leg_id = command
            .telephony_leg_id
            .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
        let leg = legs::load(&self.db, leg_id)
            .await?
            .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
        if leg.status()? != LegStatus::Planned {
            // Cancelled meanwhile (or already executed by another path).
            self.finish_command(
                &self.db,
                command.id,
                "FAILED",
                "LEG_NOT_PLANNED",
                Some(FailureCategory::Policy),
                now,
            )
            .await?;
            return Ok(Executed::Skipped);
        }
        let class =
            PublicRouteClass::parse(&leg.route_class).unwrap_or(PublicRouteClass::AppToPstn);
        if !self.flags.telephony.enabled() || !self.flags.public_outbound.enabled() {
            return self
                .fail_before_send(&command, FailureCategory::Policy, "FEATURE_DISABLED")
                .await;
        }
        if class == PublicRouteClass::CallbackRelay && !self.flags.callback_relay.enabled() {
            return self
                .fail_before_send(&command, FailureCategory::Policy, "FEATURE_DISABLED")
                .await;
        }
        let authority_until: Option<DateTime<Utc>> = command.payload["authorityValidUntil"]
            .as_str()
            .and_then(|s| s.parse().ok());
        if authority_until.is_none_or(|until| until <= now) {
            return self
                .fail_before_send(&command, FailureCategory::Policy, "AUTHORITY_EXPIRED")
                .await;
        }
        let (Some(grant_id), Some(grant_secret), Some(grant_expires_at)) = (
            command.grant_id,
            command.grant_secret.clone(),
            command.grant_expires_at,
        ) else {
            return self
                .fail_before_send(&command, FailureCategory::Policy, "GRANT_MISSING")
                .await;
        };
        if grant_expires_at <= now {
            return self
                .fail_before_send(&command, FailureCategory::Policy, "GRANT_EXPIRED")
                .await;
        }
        let delegated = match serde_json::from_value(command.payload["delegated"].clone()) {
            Ok(d) => d,
            Err(_) => {
                return self
                    .fail_before_send(&command, FailureCategory::Internal, "DELEGATION_MISSING")
                    .await
            }
        };
        // Resolve the protected destination for THIS execution attempt (memory only).
        let resolved = match self
            .privacy
            .resolve_for_execution(&ResolveForExecution {
                delegated,
                correlation_id: command.correlation_id.clone(),
                contact_reference: leg.contact_reference.unwrap_or_default(),
                grant_id,
                grant_secret,
                call_id: leg.calling_call_id,
                call_leg_id: leg.calling_call_leg_id,
                execution_attempt_id: command.id,
            })
            .await
        {
            Ok(r) => r,
            Err(PortError::Unavailable) => {
                if command.attempts < command.max_attempts
                    && grant_expires_at > now + Duration::seconds(2)
                {
                    return self.reschedule(&command, "PRIVACY_RELAY_UNAVAILABLE").await;
                }
                return self
                    .fail_before_send(
                        &command,
                        FailureCategory::Network,
                        "PRIVACY_RELAY_UNAVAILABLE",
                    )
                    .await;
            }
            Err(PortError::NotFound) | Err(PortError::Denied(_)) => {
                return self
                    .fail_before_send(&command, FailureCategory::Policy, "RESOLUTION_DENIED")
                    .await;
            }
        };
        // The resolved destination must be in the country the route was authorized for
        // (no destination-policy bypass through a changed contact).
        let country = resolved.country.clone().unwrap_or_default();
        if command.payload["destinationCountry"].as_str() != Some(country.as_str()) {
            return self
                .fail_before_send(
                    &command,
                    FailureCategory::Policy,
                    "DESTINATION_COUNTRY_MISMATCH",
                )
                .await;
        }
        let candidates: Vec<String> = command.payload["candidateRoutes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let Some(caller_identity) = resolved.caller_identity.clone() else {
            return self
                .fail_before_send(
                    &command,
                    FailureCategory::Routing,
                    "CALLER_IDENTITY_UNAVAILABLE",
                )
                .await;
        };
        // Choose the interconnect: eligible route, caller identity bound to its
        // provider, capacity reserved — and the correlation key persisted — atomically.
        let txn = self.db.begin().await?;
        let routes = self
            .eligible_routes(&txn, &country, RouteCapability::VoiceOutbound, &candidates)
            .await?;
        let mut chosen = None;
        let mut capacity_exhausted = false;
        for route in routes {
            if route
                .estimated_leg_cost_minor
                .is_some_and(|c| leg.spend_reserved_minor.is_some_and(|r| c > r))
            {
                continue;
            }
            if route
                .cost_currency
                .as_deref()
                .is_some_and(|c| Some(c) != leg.spend_currency.as_deref())
            {
                continue;
            }
            let bound = db::scalar_i64(
                &txn,
                "SELECT count(*) FROM telecom_number_bindings b JOIN telecom_routes r ON r.id = b.route_id \
                 WHERE b.public_e164 = $1 AND b.status = 'ENABLED' AND 'VOICE_OUTBOUND' = ANY(b.capabilities) \
                   AND r.provider_id = $2",
                vec![caller_identity.expose().into(), route.provider_id.clone().into()],
            )
            .await?;
            if bound == 0 {
                continue;
            }
            match self.reserve_capacity(&txn, &route).await {
                Ok(()) => {
                    chosen = Some(route);
                    break;
                }
                Err(TelephonyError::Refused(ErrorCode::CapacityExhausted)) => {
                    capacity_exhausted = true
                }
                Err(e) => return Err(e),
            }
        }
        let Some(route) = chosen else {
            drop(txn);
            let code = if capacity_exhausted {
                "CAPACITY_EXHAUSTED"
            } else {
                "ROUTE_UNAVAILABLE"
            };
            return self
                .fail_before_send(&command, FailureCategory::Routing, code)
                .await;
        };
        let correlation = correlation_key();
        let binding_id = Uuid::new_v4();
        let locked = legs::lock(&txn, leg_id).await?;
        if locked.status()? != LegStatus::Planned {
            drop(txn);
            self.finish_command(
                &self.db,
                command.id,
                "FAILED",
                "LEG_NOT_PLANNED",
                Some(FailureCategory::Policy),
                now,
            )
            .await?;
            return Ok(Executed::Skipped);
        }
        db::exec(
            &txn,
            "UPDATE telephony_legs SET route_id = $2, provider_id = $3, destination_country = $4, \
               updated_at = GREATEST(updated_at, $5), version = version + 1 WHERE id = $1",
            vec![
                leg_id.into(),
                route.id.clone().into(),
                route.provider_id.clone().into(),
                country.clone().into(),
                now.into(),
            ],
        )
        .await?;
        db::exec(
            &txn,
            "INSERT INTO provider_call_bindings (id, telephony_leg_id, provider_id, route_id, correlation_key, \
               status, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 'PREPARED', $6, $6)",
            vec![
                binding_id.into(),
                leg_id.into(),
                route.provider_id.clone().into(),
                route.id.clone().into(),
                correlation.clone().into(),
                now.into(),
            ],
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE telephony_commands SET status = 'RUNNING', updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![command.id.into(), now.into()],
        )
        .await?;
        txn.commit().await?;

        // The transmit fence.
        let fence_at = self.now();
        let txn = self.db.begin().await?;
        let locked = legs::lock(&txn, leg_id).await?;
        if locked.status()? != LegStatus::Planned {
            drop(txn);
            self.fail_before_send(&command, FailureCategory::Policy, "CANCELLED")
                .await?;
            return Ok(Executed::Skipped);
        }
        legs::transition_in(
            &txn,
            &locked,
            &Change::to(
                LegStatus::Originating,
                NormalizedEvent::LegAccepted,
                fence_at,
            )
            .silent(),
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE telephony_commands SET transmitted_at = $2, grant_secret = NULL, \
               updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![command.id.into(), fence_at.into()],
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE provider_call_bindings SET status = 'SENT', sent_at = $2, updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1",
            vec![binding_id.into(), fence_at.into()],
        )
        .await?;
        txn.commit().await?;

        // The provider request.
        let provider = self
            .providers
            .get(&route.provider_id)
            .cloned()
            .ok_or(TelephonyError::Refused(ErrorCode::RouteUnavailable))?;
        let request = OriginateRequest {
            route_ref: route.id.clone(),
            sip_peer_config_ref: route.sip_peer_config_ref_id.clone(),
            caller_identity,
            destination: resolved.destination,
            command_id: command.id,
            correlation_key: correlation.clone(),
            idempotency_key: command.id.to_string(),
            ring_timeout: self.settings.ring_timeout,
            request_timeout: self.settings.provider_request_timeout,
        };
        let outcome = match tokio::time::timeout(
            self.settings.provider_request_timeout,
            provider.originate(request),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => OriginateOutcome::UnknownOutcome {
                reason: UnknownReason::TimeoutAfterSend,
            },
        };
        // Persist the outcome (retry a few times: losing it would turn a known outcome
        // into an unknown one; if it is lost anyway, recovery marks it UNKNOWN).
        let mut last = Err(TelephonyError::Refused(ErrorCode::ServiceUnavailable));
        for attempt in 0..3 {
            last = self
                .record_originate_outcome(
                    &command,
                    leg_id,
                    binding_id,
                    &route.id,
                    &route.provider_id,
                    &outcome,
                )
                .await;
            if last.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100 << attempt)).await;
        }
        last
    }

    async fn record_originate_outcome(
        &self,
        command: &CommandRow,
        leg_id: Uuid,
        binding_id: Uuid,
        route_id: &str,
        provider_id: &str,
        outcome: &OriginateOutcome,
    ) -> TelephonyResult<Executed> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let leg = legs::lock(&txn, leg_id).await?;
        let result = match outcome {
            OriginateOutcome::Accepted {
                provider_call_ref,
                provider_idempotency_ref,
            } => {
                db::exec(
                    &txn,
                    "UPDATE provider_call_bindings SET status = 'BOUND', provider_call_ref = $2, \
                       provider_idempotency_ref = $3, bound_at = $4, updated_at = GREATEST(updated_at, $4) \
                     WHERE id = $1 AND status = 'SENT'",
                    vec![
                        binding_id.into(),
                        provider_call_ref.clone().into(),
                        provider_idempotency_ref.clone().into(),
                        now.into(),
                    ],
                )
                .await?;
                self.finish_command(&txn, command.id, "SUCCEEDED", "ACCEPTED", None, now)
                    .await?;
                if !leg.status()?.is_terminal() {
                    outbox::emit_leg_event(
                        &txn,
                        &leg,
                        NormalizedEvent::LegAccepted,
                        json!({ "legStatus": leg.status }),
                        now,
                    )
                    .await?;
                }
                self.record_route_success(&txn, route_id, provider_id)
                    .await?;
                Executed::Accepted
            }
            OriginateOutcome::DefiniteFailure {
                category,
                reason,
                transmitted,
            } => {
                db::exec(
                    &txn,
                    "UPDATE provider_call_bindings SET status = 'ABSENT', updated_at = GREATEST(updated_at, $2) \
                     WHERE id = $1 AND status IN ('PREPARED', 'SENT')",
                    vec![binding_id.into(), now.into()],
                )
                .await?;
                let event = match reason {
                    EndReason::Busy => NormalizedEvent::LegBusy,
                    EndReason::NoAnswer => NormalizedEvent::LegNoAnswer,
                    EndReason::Declined => NormalizedEvent::LegDeclined,
                    _ => NormalizedEvent::LegFailed,
                };
                if !leg.status()?.is_terminal() {
                    legs::transition_in(
                        &txn,
                        &leg,
                        &Change::ended(event, *reason, now).category(*category),
                    )
                    .await?;
                }
                self.finish_command(
                    &txn,
                    command.id,
                    "FAILED",
                    "PROVIDER_REFUSED",
                    Some(*category),
                    now,
                )
                .await?;
                if *transmitted
                    && matches!(
                        category,
                        FailureCategory::Routing
                            | FailureCategory::Network
                            | FailureCategory::Execution
                    )
                {
                    self.record_route_failure(&txn, route_id, provider_id)
                        .await?;
                }
                Executed::DefiniteFailure("PROVIDER_REFUSED")
            }
            OriginateOutcome::UnknownOutcome { reason } => {
                self.mark_unknown_in(&txn, command, *reason, now).await?;
                Executed::Unknown
            }
        };
        txn.commit().await?;
        tracing::info!(
            command_id = %command.id,
            telephony_leg_id = %leg_id,
            outcome = ?result,
            "origination outcome recorded"
        );
        Ok(result)
    }

    /// Marks a transmitted command (and its leg) UNKNOWN and schedules reconciliation.
    pub(crate) async fn mark_unknown_in<C: ConnectionTrait>(
        &self,
        txn: &C,
        command: &CommandRow,
        reason: UnknownReason,
        now: DateTime<Utc>,
    ) -> TelephonyResult<()> {
        db::exec(
            txn,
            "UPDATE telephony_commands SET status = 'UNKNOWN_OUTCOME', result_code = $2, grant_secret = NULL, \
               lease_owner = NULL, lease_until = NULL, next_attempt_at = $3, updated_at = GREATEST(updated_at, $4) \
             WHERE id = $1 AND status IN ('CLAIMED', 'RUNNING')",
            vec![
                command.id.into(),
                reason.as_str().into(),
                (now + Duration::from_std(self.settings.reconcile_delay).unwrap_or(Duration::seconds(5))).into(),
                now.into(),
            ],
        )
        .await?;
        if let Some(leg_id) = command.telephony_leg_id {
            let leg = legs::lock(txn, leg_id).await?;
            let status = leg.status()?;
            if command.kind == CommandKind::Originate.as_str()
                && matches!(status, LegStatus::Planned | LegStatus::Originating)
            {
                let leg = if status == LegStatus::Planned {
                    // Interrupted between fence and outcome bookkeeping.
                    legs::transition_in(
                        txn,
                        &leg,
                        &Change::to(LegStatus::Originating, NormalizedEvent::LegAccepted, now)
                            .silent(),
                    )
                    .await?
                    .unwrap_or(leg)
                } else {
                    leg
                };
                legs::transition_in(
                    txn,
                    &leg,
                    &Change::to(LegStatus::Unknown, NormalizedEvent::LegStatusUnknown, now)
                        .indefinite(),
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE telephony_legs SET reconcile_after = $2 WHERE id = $1",
                    vec![
                        leg_id.into(),
                        (now + Duration::from_std(self.settings.reconcile_delay)
                            .unwrap_or(Duration::seconds(5)))
                        .into(),
                    ],
                )
                .await?;
                incidents::raise(
                    txn,
                    &Incident::new(
                        "UNKNOWN_ORIGINATION_OUTCOME",
                        Severity::High,
                        format!("unknown-origination:{leg_id}"),
                    )
                    .leg(&leg)
                    .evidence(json!({ "reason": reason.as_str(), "commandId": command.id })),
                    now,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// The edge / provider reference of a leg's current binding, if bound.
    pub(crate) async fn bound_ref<C: ConnectionTrait>(
        &self,
        db_: &C,
        leg: &LegRow,
    ) -> TelephonyResult<Option<(String, Option<String>, String)>> {
        let row: Option<BindingRef> = db::one(
            db_,
            "SELECT provider_id, provider_call_ref, correlation_key FROM provider_call_bindings \
             WHERE telephony_leg_id = $1 AND status IN ('SENT', 'BOUND') ORDER BY created_at DESC LIMIT 1",
            vec![leg.id.into()],
        )
        .await?;
        Ok(row.map(|r| (r.provider_id, r.provider_call_ref, r.correlation_key)))
    }

    async fn execute_control(
        &self,
        command: CommandRow,
        kind: CommandKind,
    ) -> TelephonyResult<Executed> {
        let now = self.now();
        let leg_id = command
            .telephony_leg_id
            .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
        let leg = legs::load(&self.db, leg_id)
            .await?
            .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
        let status = leg.status()?;
        if status.is_terminal() {
            self.finish_command(
                &self.db,
                command.id,
                "SUCCEEDED",
                "ALREADY_ENDED",
                None,
                now,
            )
            .await?;
            return Ok(Executed::Done);
        }
        if status == LegStatus::Planned {
            // Not transmitted yet: TERMINATE cancels it (the ORIGINATE then never runs).
            if kind == CommandKind::Terminate {
                let txn = self.db.begin().await?;
                let locked = legs::lock(&txn, leg_id).await?;
                if locked.status()? == LegStatus::Planned {
                    legs::transition_in(
                        &txn,
                        &locked,
                        &Change::ended(NormalizedEvent::LegEnded, EndReason::Cancelled, now),
                    )
                    .await?;
                    db::exec(
                        &txn,
                        "UPDATE telephony_commands SET status = 'FAILED', grant_secret = NULL, finished_at = $2, \
                           updated_at = $2, result_code = 'CANCELLED', failure_category = 'POLICY', \
                           lease_owner = NULL, lease_until = NULL \
                         WHERE telephony_leg_id = $1 AND kind = 'ORIGINATE' AND status IN ('PENDING', 'CLAIMED')",
                        vec![leg_id.into(), now.into()],
                    )
                    .await?;
                    self.finish_command(&txn, command.id, "SUCCEEDED", "CANCELLED", None, now)
                        .await?;
                    txn.commit().await?;
                    return Ok(Executed::Done);
                }
                drop(txn);
                return self.reschedule(&command, "LEG_IN_FLIGHT").await;
            }
            self.finish_command(
                &self.db,
                command.id,
                "FAILED",
                "LEG_NOT_ACTIVE",
                Some(FailureCategory::Policy),
                now,
            )
            .await?;
            return Ok(Executed::DefiniteFailure("LEG_NOT_ACTIVE"));
        }
        let Some((provider_id, provider_ref, correlation)) = self.bound_ref(&self.db, &leg).await?
        else {
            return self.reschedule(&command, "NO_BINDING_YET").await;
        };
        let Some(provider) = self.providers.get(&provider_id).cloned() else {
            self.finish_command(
                &self.db,
                command.id,
                "DEAD_LETTER",
                "PROVIDER_NOT_CONFIGURED",
                Some(FailureCategory::Internal),
                now,
            )
            .await?;
            return Ok(Executed::DefiniteFailure("PROVIDER_NOT_CONFIGURED"));
        };
        // An unknown provider reference: read back by correlation first.
        let provider_ref = match provider_ref {
            Some(r) => r,
            None => match provider
                .query_call(&QueryKey {
                    provider_call_ref: None,
                    correlation_key: correlation.clone(),
                    // Not known here: an adapter that cannot prove absence answers
                    // Unknown and reconciliation (which has `sent_at`) decides.
                    transmitted_at: None,
                })
                .await
            {
                QueryOutcome::Active {
                    provider_call_ref, ..
                }
                | QueryOutcome::Ended {
                    provider_call_ref, ..
                } => provider_call_ref,
                QueryOutcome::Absent => {
                    if kind == CommandKind::Terminate {
                        let txn = self.db.begin().await?;
                        let locked = legs::lock(&txn, leg_id).await?;
                        if !locked.status()?.is_terminal() {
                            legs::transition_in(
                                &txn,
                                &locked,
                                &Change::ended(
                                    NormalizedEvent::LegFailed,
                                    EndReason::AbsentAtProvider,
                                    now,
                                ),
                            )
                            .await?;
                        }
                        self.finish_command(
                            &txn,
                            command.id,
                            "SUCCEEDED",
                            "ABSENT_AT_PROVIDER",
                            None,
                            now,
                        )
                        .await?;
                        txn.commit().await?;
                        return Ok(Executed::Done);
                    }
                    self.finish_command(
                        &self.db,
                        command.id,
                        "FAILED",
                        "ABSENT_AT_PROVIDER",
                        Some(FailureCategory::Execution),
                        now,
                    )
                    .await?;
                    return Ok(Executed::DefiniteFailure("ABSENT_AT_PROVIDER"));
                }
                QueryOutcome::Unknown => {
                    if command.attempts >= command.max_attempts {
                        self.finish_command(
                            &self.db,
                            command.id,
                            "DEAD_LETTER",
                            "UNRESOLVED",
                            Some(FailureCategory::Unknown),
                            now,
                        )
                        .await?;
                        incidents::raise(
                            &self.db,
                            &Incident::new(
                                "UNKNOWN_TERMINATION_OUTCOME",
                                Severity::High,
                                format!("unknown-control:{leg_id}"),
                            )
                            .leg(&leg),
                            now,
                        )
                        .await?;
                        return Ok(Executed::Unknown);
                    }
                    return self.reschedule(&command, "READ_BACK_UNKNOWN").await;
                }
            },
        };
        let outcome = match kind {
            CommandKind::Terminate => provider.terminate(&provider_ref).await,
            CommandKind::Hold => provider.hold(&provider_ref).await,
            _ => provider.resume(&provider_ref).await,
        };
        let txn = self.db.begin().await?;
        let locked = legs::lock(&txn, leg_id).await?;
        let result = match (kind, outcome) {
            (CommandKind::Terminate, ControlOutcome::Done | ControlOutcome::AlreadyGone) => {
                if !locked.status()?.is_terminal() {
                    let reason = if locked.answered_at.is_some() {
                        EndReason::Completed
                    } else {
                        EndReason::Cancelled
                    };
                    legs::transition_in(
                        &txn,
                        &locked,
                        &Change::ended(NormalizedEvent::LegEnded, reason, now),
                    )
                    .await?;
                }
                self.finish_command(&txn, command.id, "SUCCEEDED", "TERMINATED", None, now)
                    .await?;
                Executed::Done
            }
            (CommandKind::Hold, ControlOutcome::Done) => {
                legs::transition_in(
                    &txn,
                    &locked,
                    &Change::to(LegStatus::Held, NormalizedEvent::LegHeld, now),
                )
                .await?;
                self.finish_command(&txn, command.id, "SUCCEEDED", "HELD", None, now)
                    .await?;
                Executed::Done
            }
            (CommandKind::Resume, ControlOutcome::Done) => {
                legs::transition_in(
                    &txn,
                    &locked,
                    &Change::to(LegStatus::Connected, NormalizedEvent::LegResumed, now),
                )
                .await?;
                self.finish_command(&txn, command.id, "SUCCEEDED", "RESUMED", None, now)
                    .await?;
                Executed::Done
            }
            (_, ControlOutcome::Unknown) if command.attempts < command.max_attempts => {
                drop(txn);
                return self.reschedule(&command, "CONTROL_UNKNOWN").await;
            }
            (_, _) => {
                self.finish_command(
                    &txn,
                    command.id,
                    "FAILED",
                    "CONTROL_REFUSED",
                    Some(FailureCategory::Execution),
                    now,
                )
                .await?;
                Executed::DefiniteFailure("CONTROL_REFUSED")
            }
        };
        txn.commit().await?;
        Ok(result)
    }
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct BindingRef {
    provider_id: String,
    provider_call_ref: Option<String>,
    correlation_key: String,
}

/// Bounded stored detail of a command outcome (never a number or provider payload).
pub fn outcome_detail(code: &str) -> Value {
    json!({ "code": code })
}
