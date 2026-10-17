//! Command execution workers (Part E §34, §48, §75, §120).
//!
//! Commands are claimed with a lease (`FOR UPDATE SKIP LOCKED`; any number of
//! instances). An ORIGINATE runs in fenced steps:
//!
//! 1. checks (leg not cancelled, kill switches, authority still valid, grant valid);
//! 2. route choice + capacity reservation + correlation key persisted (one transaction);
//! 3. protected destination resolved from Privacy Relay with the grant — in memory only;
//!    the destination is classified (Part H §66: premium / shared-cost / satellite /
//!    high-risk ranges are refused before any provider action); the caller identity is
//!    the session's relay identity Privacy Relay selected, it must be bound to the
//!    chosen interconnect, and its assignment reference is kept on the leg as identity
//!    evidence (Part H §78);
//! 4. the TRANSMIT FENCE is committed (`transmitted_at`, binding SENT, leg ORIGINATING)
//!    after re-checking, inside the fence transaction (Part H §75): the kill switches,
//!    that the chosen route and provider are still ENABLED, that neither circuit
//!    opened, and the destination class decision. Any change since route choice stops
//!    the request — definite, nothing sent (`ROUTE_DISABLED` / `PROVIDER_DISABLED` /
//!    `CIRCUIT_OPEN` / `DESTINATION_CLASS_BLOCKED` / `DESTINATION_UNCLASSIFIED`); a
//!    change committed after this read applies to the next request. If the database
//!    is unavailable here no provider request is ever sent;
//! 5. the provider request (bounded by a deadline);
//! 6. the outcome is persisted: ACCEPTED, DEFINITE_FAILURE or UNKNOWN_OUTCOME.
//!
//! A worker that dies holding a lease is recovered: before the fence the command is
//! safely retried; after it the outcome is UNKNOWN and goes to reconciliation — never a
//! blind retry (also enforced by a database trigger).
//!
//! Retries are capped by the command's `max_attempts`: an untransmitted command that
//! reaches it is DEAD_LETTER (final; an ORIGINATE's leg fails as NOT_SENT), except a
//! TERMINATE, which is never abandoned (unresolved, its leg is held in ENDING and
//! reconciled). Concurrent workers keep each leg's commands in creation order.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};
use tracing::Instrument;
use uuid::Uuid;

use super::{CommandRow, COMMAND_COLUMNS};
use crate::{
    db,
    destinations::{self, Classification},
    domain::{
        CommandKind, EndReason, FailureCategory, InterconnectClass, LegStatus, NormalizedEvent,
        PublicRouteClass, RouteCapability,
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
                "WITH due AS ( \
                   SELECT id, kind, telephony_leg_id, created_at FROM telephony_commands \
                   WHERE status = 'PENDING' AND next_attempt_at <= $3 \
                   ORDER BY next_attempt_at, created_at LIMIT $4 FOR UPDATE SKIP LOCKED), \
                 ready AS ( \
                   SELECT d.id FROM due d WHERE d.kind = 'TERMINATE' OR d.telephony_leg_id IS NULL \
                     OR NOT EXISTS (SELECT 1 FROM telephony_commands e \
                       WHERE e.telephony_leg_id = d.telephony_leg_id \
                         AND e.status IN ('PENDING', 'CLAIMED', 'RUNNING') \
                         AND (e.created_at, e.id) < (d.created_at, d.id) \
                         AND e.id NOT IN (SELECT id FROM due))) \
                 UPDATE telephony_commands SET status = 'CLAIMED', lease_owner = $1, \
                   lease_until = $2, attempts = attempts + 1, updated_at = GREATEST(updated_at, $3) \
                 WHERE id IN (SELECT id FROM ready) \
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
        // RETURNING has no order: execute in creation order (Part F: the prompts of one
        // enterprise leg play in sequence). Per leg, a command is claimed only together
        // with — or after — every earlier unfinished command of that leg, so concurrent
        // workers never run one leg's commands out of order (a TERMINATE never waits).
        let mut claimed = claimed;
        claimed.sort_by_key(|c| (c.created_at, c.id));
        let mut out = Vec::new();
        for command in claimed {
            let id = command.id;
            let span = tracing::info_span!(
                "telephony_command",
                command_id = %id,
                kind = %command.kind,
                telephony_leg_id = %command.telephony_leg_id.map(|l| l.to_string()).unwrap_or_default(),
                correlation_id = %command.correlation_id,
            );
            let result = self.execute_claimed(command).instrument(span).await;
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

    /// Executes one claimed command. A command already past its attempt cap is
    /// dead-lettered before anything runs (a TERMINATE is never abandoned: it keeps going
    /// and, unresolved, ends in reconciliation).
    async fn execute_claimed(&self, command: CommandRow) -> TelephonyResult<Executed> {
        if command.attempts > command.max_attempts
            && command.kind != CommandKind::Terminate.as_str()
        {
            return self
                .dead_letter_command(&command, "ATTEMPTS_EXHAUSTED")
                .await;
        }
        match CommandKind::parse(&command.kind) {
            Some(CommandKind::Originate) => self.execute_originate(command).await,
            Some(CommandKind::Terminate) => {
                self.execute_control(command, CommandKind::Terminate).await
            }
            Some(CommandKind::Hold) => self.execute_control(command, CommandKind::Hold).await,
            Some(CommandKind::Resume) => self.execute_control(command, CommandKind::Resume).await,
            Some(
                k @ (CommandKind::Answer
                | CommandKind::Prompt
                | CommandKind::Capture
                | CommandKind::CaptureStop),
            ) => self.execute_control(command, k).await,
            Some(other) => self.execute_bridge_command(command, other).await,
            None => Ok(Executed::Skipped),
        }
    }

    /// Dead-letters a command that never left this process (attempt cap reached): final,
    /// never retried. An ORIGINATE's leg fails as NOT_SENT (Calling learns it definitely
    /// never rang). Post-fence commands are never dead-lettered here — their outcome is
    /// UNKNOWN and belongs to reconciliation.
    pub(crate) async fn dead_letter_command(
        &self,
        command: &CommandRow,
        code: &'static str,
    ) -> TelephonyResult<Executed> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let finished = self
            .dead_letter_command_in(&txn, command, code, now)
            .await?;
        txn.commit().await?;
        if !finished {
            return Ok(Executed::Skipped);
        }
        Ok(Executed::DefiniteFailure(code))
    }

    async fn dead_letter_command_in<C: ConnectionTrait>(
        &self,
        txn: &C,
        command: &CommandRow,
        code: &'static str,
        now: DateTime<Utc>,
    ) -> TelephonyResult<bool> {
        let n = db::exec(
            txn,
            "UPDATE telephony_commands SET status = 'DEAD_LETTER', result_code = $2, failure_category = 'INTERNAL', \
               grant_secret = NULL, lease_owner = NULL, lease_until = NULL, updated_at = GREATEST(updated_at, $3), \
               finished_at = GREATEST(created_at, $3) \
             WHERE id = $1 AND transmitted_at IS NULL AND status IN ('PENDING', 'CLAIMED', 'RUNNING')",
            vec![command.id.into(), code.into(), now.into()],
        )
        .await?;
        if n == 0 {
            return Ok(false);
        }
        if command.kind == CommandKind::Originate.as_str() {
            if let Some(leg_id) = command.telephony_leg_id {
                let leg = legs::lock(txn, leg_id).await?;
                if leg.status()? == LegStatus::Planned {
                    legs::transition_in(
                        txn,
                        &leg,
                        &Change::ended(NormalizedEvent::LegFailed, EndReason::NotSent, now)
                            .category(FailureCategory::Internal),
                    )
                    .await?;
                }
                db::exec(
                    txn,
                    "UPDATE provider_call_bindings SET status = 'ABSENT', updated_at = GREATEST(updated_at, $2) \
                     WHERE telephony_leg_id = $1 AND status = 'PREPARED'",
                    vec![leg_id.into(), now.into()],
                )
                .await?;
            }
        }
        let kind = CommandKind::parse(&command.kind)
            .map(|k| k.as_str())
            .unwrap_or("UNKNOWN");
        crate::metrics::inc(
            "telephony_commands_dead_lettered_total",
            &[("kind", kind), ("reason", code)],
        );
        tracing::warn!(
            command_id = %command.id,
            attempts = command.attempts,
            code,
            "command dead-lettered after its attempt cap; it will not be retried"
        );
        Ok(true)
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
            if c.transmitted_at.is_none()
                && c.attempts >= c.max_attempts
                && c.kind != CommandKind::Terminate.as_str()
            {
                // Its worker died on every attempt (a poison command): stop, never loop.
                self.dead_letter_command_in(&txn, c, "ATTEMPTS_EXHAUSTED", now)
                    .await?;
            } else if c.transmitted_at.is_none() {
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
        crate::metrics::inc(
            "telephony_originations_total",
            &[
                ("outcome", "NOT_SENT"),
                ("category", category.as_str()),
                ("code", code),
            ],
        );
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

    /// Finishes a command (outside a caller transaction).
    pub(crate) async fn finish_command_pub(
        &self,
        id: Uuid,
        status: &str,
        result_code: &str,
        now: DateTime<Utc>,
    ) -> TelephonyResult<()> {
        let category = (status == "FAILED").then_some(FailureCategory::Execution);
        self.finish_command(&self.db, id, status, result_code, category, now)
            .await
    }

    pub(crate) async fn reschedule_pub(
        &self,
        command: &CommandRow,
        code: &str,
    ) -> TelephonyResult<Executed> {
        self.reschedule(command, code).await
    }

    /// Puts an untransmitted command back for a later attempt — until its attempt cap:
    /// then a TERMINATE becomes an unresolved termination (never abandoned: the leg is held
    /// in ENDING and reconciled) and any other command is dead-lettered.
    async fn reschedule(&self, command: &CommandRow, code: &str) -> TelephonyResult<Executed> {
        if command.attempts >= command.max_attempts {
            if command.kind == CommandKind::Terminate.as_str() {
                if let Some(leg_id) = command.telephony_leg_id {
                    return self
                        .termination_unresolved(command, leg_id, "ATTEMPTS_EXHAUSTED")
                        .await;
                }
            }
            return self
                .dead_letter_command(command, "ATTEMPTS_EXHAUSTED")
                .await;
        }
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
            Ok(r) => {
                self.privacy_relay_available().await?;
                r
            }
            Err(PortError::Unavailable) => {
                self.privacy_relay_unavailable("RESOLVE_FOR_EXECUTION")
                    .await?;
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
                // Privacy Relay answered (and refused): it is reachable.
                self.privacy_relay_available().await?;
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
        // Pre-toll destination classification (Part H §66): the plaintext exists only
        // here; only the bounded class leaves as a code / metric label.
        let classification = destinations::classify(
            &self.db,
            &country,
            &resolved.destination,
            &self.settings.blocked_destination_classes,
            self.settings.unclassified_destination_policy,
        )
        .await?;
        if !classification.allowed() {
            return self
                .refuse_destination(&command, &leg, classification, now)
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
        // Part IV L closure: the interconnect class Calling's route policy selected. A
        // synthetic test interconnect exists only while it is enabled (local / test).
        let interconnect = command.payload["interconnectClass"]
            .as_str()
            .and_then(InterconnectClass::parse)
            .unwrap_or_default();
        if !self.interconnect_usable(interconnect) {
            return self
                .fail_before_send(
                    &command,
                    FailureCategory::Policy,
                    "SYNTHETIC_INTERCONNECT_DISABLED",
                )
                .await;
        }
        // The caller identity: a DCN presented under a Numbering CALLER_IDENTITY grant
        // (Workstream L), else the relay number Privacy Relay lent to the session.
        let presented: Option<Uuid> = command.payload["presentedNumberResourceId"]
            .as_str()
            .and_then(|s| s.parse().ok());
        let dcn = match presented {
            Some(number_resource_id) => match self
                .verify_dcn_identity(&command, &leg, number_resource_id, grant_expires_at, now)
                .await?
            {
                Ok(identity) => Some(identity),
                Err(done) => return Ok(done),
            },
            None => None,
        };
        // A synthetic DCN travels ONLY over a synthetic test interconnect and a synthetic
        // test interconnect carries ONLY a synthetic DCN: no real number on a test route,
        // no test number on a real carrier.
        let synthetic_identity = dcn.as_ref().is_some_and(|d| d.synthetic());
        if (interconnect == InterconnectClass::SyntheticTest) != synthetic_identity {
            crate::metrics::inc(
                "telephony_interconnect_refusals_total",
                &[("class", interconnect.as_str())],
            );
            return self
                .fail_before_send(&command, FailureCategory::Policy, "INTERCONNECT_MISMATCH")
                .await;
        }
        let caller_identity = match &dcn {
            Some(identity) => identity.identity.clone(),
            None => match resolved.caller_identity.clone() {
                Some(identity) => identity,
                None => {
                    return self
                        .fail_before_send(
                            &command,
                            FailureCategory::Routing,
                            "CALLER_IDENTITY_UNAVAILABLE",
                        )
                        .await
                }
            },
        };
        // Choose the interconnect: eligible route, caller identity bound to its
        // provider, capacity reserved — and the correlation key persisted — atomically.
        let txn = self.db.begin().await?;
        // Attempt history (Part E §144): an interconnect that already failed DEFINITELY
        // on the provider side for this party of this Call is not tried again.
        let failed_before: Vec<String> = db::all::<RouteRef, _>(
            &txn,
            "SELECT DISTINCT route_id FROM telephony_legs WHERE calling_call_id = $1 AND contact_reference = $2 \
               AND id <> $3 AND status = 'FAILED' AND route_id IS NOT NULL \
               AND (failure_category IN ('ROUTING', 'NETWORK', 'EXECUTION') OR end_reason = 'ABSENT_AT_PROVIDER')",
            vec![
                leg.calling_call_id.into(),
                leg.contact_reference.into(),
                leg.id.into(),
            ],
        )
        .await?
        .into_iter()
        .map(|r| r.route_id)
        .collect();
        let routes: Vec<_> = self
            .eligible_routes(
                &txn,
                &country,
                RouteCapability::VoiceOutbound,
                &candidates,
                interconnect,
            )
            .await?
            .into_iter()
            .filter(|r| !failed_before.contains(&r.id))
            .collect();
        let mut chosen = None;
        let mut capacity_exhausted = false;
        let mut identity_unbound = 0;
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
            // A DCN must be bound locally AS that DCN (number kind and Numbering resource
            // id); a relay identity as a relay number.
            let bound = db::scalar_i64(
                &txn,
                "SELECT count(*) FROM telecom_number_bindings b JOIN telecom_routes r ON r.id = b.route_id \
                 WHERE b.public_e164 = $1 AND b.status = 'ENABLED' AND 'VOICE_OUTBOUND' = ANY(b.capabilities) \
                   AND r.provider_id = $2 AND b.number_kind = $3 \
                   AND b.number_resource_id IS NOT DISTINCT FROM $4",
                vec![
                    caller_identity.expose().into(),
                    route.provider_id.clone().into(),
                    if dcn.is_some() { "DCN" } else { "RELAY" }.into(),
                    dcn.as_ref().map(|d| d.number_resource_id).into(),
                ],
            )
            .await?;
            if bound == 0 {
                identity_unbound += 1;
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
            // The caller identity Privacy Relay selected is not bound to any eligible
            // interconnect (not in `telecom_number_bindings`, or bound to another
            // provider): no identity, no call (Part H §78).
            let code = if capacity_exhausted {
                "CAPACITY_EXHAUSTED"
            } else if identity_unbound > 0 {
                "CALLER_IDENTITY_UNAVAILABLE"
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
               relay_assignment_ref = $6, dcn_number_resource_id = $7, dcn_assignment_id = $8, \
               dcn_routing_revision = $9, dcn_grant_id = $10, \
               updated_at = GREATEST(updated_at, $5), version = version + 1 \
             WHERE id = $1",
            vec![
                leg_id.into(),
                route.id.clone().into(),
                route.provider_id.clone().into(),
                country.clone().into(),
                now.into(),
                if dcn.is_some() {
                    None
                } else {
                    resolved.relay_assignment_ref
                }
                .into(),
                dcn.as_ref().map(|d| d.number_resource_id).into(),
                dcn.as_ref().map(|d| d.assignment_id).into(),
                dcn.as_ref().map(|d| d.routing_revision).into(),
                dcn.as_ref().map(|d| d.grant_id).into(),
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
        // Re-check immediately before the request leaves (Part H §45, §75): the kill
        // switches, the chosen route and its provider still ENABLED, both circuits
        // still allowing an attempt, and the destination class decision unchanged. An
        // operator `disable` is the lasting stop; any change between route choice and
        // here means NO request (definite, safe to fail over). Nothing awaits between
        // this check and the fence commit except the fence's own statements.
        match self
            .transmit_gate(&txn, &route.id, class, &country, &resolved.destination)
            .await?
        {
            None => {}
            Some(GateRefusal::Destination(classification)) => {
                drop(txn);
                return self
                    .refuse_destination(&command, &leg, classification, now)
                    .await;
            }
            Some(GateRefusal::Code(code, category)) => {
                drop(txn);
                return self.fail_before_send(&command, category, code).await;
            }
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

    /// Verifies and CONSUMES the Numbering CALLER_IDENTITY grant Calling obtained for this
    /// origination (Workstream L): signature, audience, expiry, current assignment and
    /// routing revision are checked by Numbering against current state, single use for
    /// exactly this operation. The verified scope and number must be the command's. A
    /// Numbering outage reschedules (or fails) the attempt — never a fallback identity.
    /// `Err(executed)` is the attempt's terminal handling.
    async fn verify_dcn_identity(
        &self,
        command: &CommandRow,
        leg: &LegRow,
        number_resource_id: Uuid,
        grant_expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> TelephonyResult<Result<crate::ports::VerifiedDcnIdentity, Executed>> {
        let Some(grant) = command.dcn_identity_grant.clone() else {
            return Ok(Err(self
                .fail_before_send(command, FailureCategory::Policy, "CALLER_IDENTITY_DENIED")
                .await?));
        };
        let operation_ref =
            super::originate::dcn_operation_ref(leg.calling_call_leg_id, command.id);
        match self
            .numbering
            .verify_caller_identity(&grant, &operation_ref, &command.correlation_id)
            .await
        {
            Ok(Ok(identity)) => {
                self.numbering_available().await?;
                let scope_matches = identity.scope.tenant_id == leg.tenant_id
                    && identity.scope.application_id == leg.application_id
                    && identity.scope.environment_id == leg.environment_id;
                if !scope_matches || identity.number_resource_id != number_resource_id {
                    tracing::warn!(command_id = %command.id, "caller-identity grant does not match the origination");
                    return Ok(Err(self
                        .fail_before_send(
                            command,
                            FailureCategory::Policy,
                            "CALLER_IDENTITY_DENIED",
                        )
                        .await?));
                }
                crate::metrics::inc(
                    "telephony_dcn_caller_identity_total",
                    &[("outcome", "VERIFIED")],
                );
                Ok(Ok(identity))
            }
            Ok(Err(reason)) => {
                self.numbering_available().await?;
                tracing::info!(command_id = %command.id, reason = %reason, "caller-identity grant refused");
                crate::metrics::inc(
                    "telephony_dcn_caller_identity_total",
                    &[("outcome", "REFUSED")],
                );
                Ok(Err(self
                    .fail_before_send(command, FailureCategory::Policy, "CALLER_IDENTITY_DENIED")
                    .await?))
            }
            Err(PortError::NotFound) | Err(PortError::Denied(_)) => {
                self.numbering_available().await?;
                crate::metrics::inc(
                    "telephony_dcn_caller_identity_total",
                    &[("outcome", "REFUSED")],
                );
                Ok(Err(self
                    .fail_before_send(command, FailureCategory::Policy, "CALLER_IDENTITY_DENIED")
                    .await?))
            }
            Err(PortError::Unavailable) => {
                self.numbering_unavailable().await?;
                crate::metrics::inc(
                    "telephony_dcn_caller_identity_total",
                    &[("outcome", "UNAVAILABLE")],
                );
                if command.attempts < command.max_attempts
                    && grant_expires_at > now + Duration::seconds(2)
                {
                    return Ok(Err(self
                        .reschedule(command, "NUMBERING_UNAVAILABLE")
                        .await?));
                }
                Ok(Err(self
                    .fail_before_send(command, FailureCategory::Network, "NUMBERING_UNAVAILABLE")
                    .await?))
            }
        }
    }

    /// Refuses a destination whose class may not be called (pre-toll, Part H §66):
    /// bounded metric (the class only), abuse signal, definite unsent failure.
    async fn refuse_destination(
        &self,
        command: &CommandRow,
        leg: &LegRow,
        classification: Classification,
        now: DateTime<Utc>,
    ) -> TelephonyResult<Executed> {
        crate::metrics::inc(
            "telephony_destination_class_refusals_total",
            &[("class", classification.label())],
        );
        self.raise_abuse_signal("DESTINATION_CLASS_REFUSED", leg.application_id, 1, 0, now)
            .await?;
        let code = match classification {
            Classification::Blocked(_) => "DESTINATION_CLASS_BLOCKED",
            _ => "DESTINATION_UNCLASSIFIED",
        };
        self.fail_before_send(command, FailureCategory::Policy, code)
            .await
    }

    /// The transmit gate, evaluated inside the fence transaction: `Some(refusal)` when
    /// the request must NOT be sent.
    async fn transmit_gate<C: ConnectionTrait>(
        &self,
        txn: &C,
        route_id: &str,
        class: PublicRouteClass,
        country: &str,
        destination: &crate::providers::ExecutionNumber,
    ) -> TelephonyResult<Option<GateRefusal>> {
        let refuse = |code, category| Ok(Some(GateRefusal::Code(code, category)));
        if !self.flags.telephony.enabled() || !self.flags.public_outbound.enabled() {
            return refuse("FEATURE_DISABLED", FailureCategory::Policy);
        }
        if class == PublicRouteClass::CallbackRelay && !self.flags.callback_relay.enabled() {
            return refuse("FEATURE_DISABLED", FailureCategory::Policy);
        }
        let gate: Option<GateRow> = db::one(
            txn,
            "SELECT r.status AS route_status, p.status AS provider_status, \
               r.circuit_state AS route_circuit, r.circuit_opened_at AS route_opened_at, \
               p.circuit_state AS provider_circuit, p.circuit_opened_at AS provider_opened_at \
             FROM telecom_routes r JOIN telecom_providers p ON p.id = r.provider_id WHERE r.id = $1",
            vec![route_id.into()],
        )
        .await?;
        let now = self.now();
        match gate {
            None => return refuse("ROUTE_DISABLED", FailureCategory::Policy),
            Some(g) if g.route_status != "ENABLED" => {
                return refuse("ROUTE_DISABLED", FailureCategory::Policy)
            }
            Some(g) if g.provider_status != "ENABLED" => {
                return refuse("PROVIDER_DISABLED", FailureCategory::Policy)
            }
            Some(g)
                if !crate::routing::circuit_allows(&g.route_circuit, g.route_opened_at, now)
                    || !crate::routing::circuit_allows(
                        &g.provider_circuit,
                        g.provider_opened_at,
                        now,
                    ) =>
            {
                return refuse("CIRCUIT_OPEN", FailureCategory::Routing)
            }
            Some(_) => {}
        }
        // The destination class decision, re-read: a rule added or re-enabled (or the
        // policy's data changed) since the pre-toll check still stops the request.
        let classification = destinations::classify(
            txn,
            country,
            destination,
            &self.settings.blocked_destination_classes,
            self.settings.unclassified_destination_policy,
        )
        .await?;
        if !classification.allowed() {
            return Ok(Some(GateRefusal::Destination(classification)));
        }
        Ok(None)
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
        crate::metrics::inc(
            "telephony_provider_responses_total",
            &[
                ("operation", "ORIGINATE"),
                ("class", originate_class(outcome)),
            ],
        );
        match outcome {
            OriginateOutcome::Accepted { .. } => {
                crate::metrics::inc("telephony_originations_total", &[("outcome", "ACCEPTED")])
            }
            OriginateOutcome::DefiniteFailure { category, .. } => crate::metrics::inc(
                "telephony_originations_total",
                &[
                    ("outcome", "DEFINITE_FAILURE"),
                    ("category", category.as_str()),
                ],
            ),
            OriginateOutcome::UnknownOutcome { reason } => crate::metrics::inc(
                "telephony_originations_total",
                &[("outcome", "UNKNOWN"), ("reason", reason.as_str())],
            ),
        }
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

    /// A termination whose effect is not established (refused, or unknown after every
    /// attempt): the leg is held in ENDING — never reported ended while the provider call
    /// may be live — the command is UNKNOWN_OUTCOME, an incident is opened and the
    /// reconciler keeps reading back and re-terminating until the provider confirms.
    pub(crate) async fn termination_unresolved(
        &self,
        command: &CommandRow,
        leg_id: Uuid,
        code: &str,
    ) -> TelephonyResult<Executed> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let locked = legs::lock(&txn, leg_id).await?;
        let status = locked.status()?;
        if !status.is_terminal() && status != LegStatus::Ending {
            legs::transition_in(
                &txn,
                &locked,
                &Change::to(LegStatus::Ending, NormalizedEvent::LegStatusUnknown, now).silent(),
            )
            .await?;
        }
        // The provider was contacted: CLAIMED -> RUNNING -> UNKNOWN_OUTCOME.
        db::exec(
            &txn,
            "UPDATE telephony_commands SET status = 'RUNNING', updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1 AND status = 'CLAIMED'",
            vec![command.id.into(), now.into()],
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE telephony_commands SET status = 'UNKNOWN_OUTCOME', result_code = $2, lease_owner = NULL, \
               lease_until = NULL, updated_at = GREATEST(updated_at, $3) WHERE id = $1 AND status = 'RUNNING'",
            vec![command.id.into(), code.into(), now.into()],
        )
        .await?;
        if !status.is_terminal() {
            db::exec(
                &txn,
                "UPDATE telephony_legs SET reconcile_after = $2, reconcile_lease_until = NULL WHERE id = $1",
                vec![
                    leg_id.into(),
                    (now + Duration::from_std(self.settings.reconcile_delay)
                        .unwrap_or(Duration::seconds(5)))
                    .into(),
                ],
            )
            .await?;
            incidents::raise(
                &txn,
                &Incident::new(
                    "UNKNOWN_TERMINATION_OUTCOME",
                    Severity::High,
                    format!("unknown-termination:{leg_id}"),
                )
                .leg(&locked)
                .evidence(json!({ "code": code, "commandId": command.id })),
                now,
            )
            .await?;
        }
        txn.commit().await?;
        tracing::warn!(command_id = %command.id, telephony_leg_id = %leg_id, code, "termination outcome unresolved");
        Ok(Executed::Unknown)
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
        // Part F: an internal enterprise leg has no provider; the edge executes directly.
        if leg.is_enterprise() {
            return self.execute_enterprise_control(command, kind, leg).await;
        }
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
                        if kind == CommandKind::Terminate {
                            return self
                                .termination_unresolved(&command, leg_id, "READ_BACK_UNKNOWN")
                                .await;
                        }
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
                                "UNKNOWN_CONTROL_OUTCOME",
                                Severity::Medium,
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
        crate::metrics::inc(
            "telephony_provider_responses_total",
            &[
                ("operation", kind.as_str()),
                ("class", control_class(outcome)),
            ],
        );
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
            (CommandKind::Terminate, outcome) => {
                // The provider call may still be up (and paid): never "just failed".
                drop(txn);
                let code = if outcome == ControlOutcome::Unknown {
                    "CONTROL_UNKNOWN"
                } else {
                    "CONTROL_REFUSED"
                };
                return self.termination_unresolved(&command, leg_id, code).await;
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
struct RouteRef {
    route_id: String,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct GateRow {
    route_status: String,
    provider_status: String,
    route_circuit: String,
    route_opened_at: Option<DateTime<Utc>>,
    provider_circuit: String,
    provider_opened_at: Option<DateTime<Utc>>,
}

/// Why the transmit gate stopped a request.
enum GateRefusal {
    Code(&'static str, FailureCategory),
    Destination(Classification),
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct BindingRef {
    provider_id: String,
    provider_call_ref: Option<String>,
    correlation_key: String,
}

/// The neutral response class of an origination (metrics; never a provider status).
pub fn originate_class(outcome: &OriginateOutcome) -> &'static str {
    match outcome {
        OriginateOutcome::Accepted { .. } => "ACCEPTED",
        OriginateOutcome::DefiniteFailure { reason, .. } => match reason {
            EndReason::Busy => "BUSY",
            EndReason::NoAnswer => "NO_ANSWER",
            EndReason::Declined => "DECLINED",
            _ => "REFUSED",
        },
        OriginateOutcome::UnknownOutcome { .. } => "NO_DEFINITE_RESPONSE",
    }
}

/// The neutral response class of a control action (metrics).
pub fn control_class(outcome: ControlOutcome) -> &'static str {
    match outcome {
        ControlOutcome::Done => "ACCEPTED",
        ControlOutcome::AlreadyGone => "ALREADY_GONE",
        ControlOutcome::DefiniteFailure => "REFUSED",
        ControlOutcome::Unknown => "NO_DEFINITE_RESPONSE",
    }
}

/// Bounded stored detail of a command outcome (never a number or provider payload).
pub fn outcome_detail(code: &str) -> Value {
    json!({ "code": code })
}
