//! Dispatch of Calling's durable Telephony commands (Part E §75, §165–§168).
//!
//! Every instance runs it; leases (`FOR UPDATE SKIP LOCKED`) make that safe. A command is
//! sent with its stored id and idempotency key — Telephony deduplicates, so a retry after
//! "no answer" can never create a second paid leg. An ORIGINATE gets a fresh single-use
//! Contact Resolution Grant at every submission (the secret is never stored).
//!
//! ORIGINATE of a Call that is no longer active:
//! * never submitted (attempts = 0) → DEAD, nothing was ever sent;
//! * submitted before without an answer → NOT resubmitted while its authority proof is
//!   still valid (that could create the paid leg now); after `validUntil` a resubmission
//!   is answered definitively: a replay (Telephony had accepted it — then it is
//!   terminated) or AUTHORIZATION_PROOF_EXPIRED (it never executed).

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    enqueue_in, exec, parties, retry_delay, stmt, CommandKind, CommandRow, COMMAND_COLUMNS,
};
use crate::{
    calls::{
        app::{scope_of, system_cmd, AppCallService},
        errors::{CallError, CallResult},
        ids::{CallId, CallLegId},
        kinds::{AttemptOutcome, FailureCategory, LegState},
        service::{attempt_of, leg_of},
        state::CallState,
    },
    models::calls,
    privacy_relay::{GrantRequest, PrivacyRelayError},
    telephony::{spend, LegControl, TelephonyError},
};

/// Lease of a claimed command.
const LEASE_SECONDS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dispatched {
    Accepted,
    Refused,
    Retry,
    Dead,
}

fn correlation(payload: &Value) -> cf_service_auth::CorrelationId {
    payload["correlationId"]
        .as_str()
        .and_then(|c| cf_service_auth::CorrelationId::parse(c).ok())
        .unwrap_or_else(cf_service_auth::CorrelationId::generate)
}

/// Maps a Telephony refusal code onto Calling's failure vocabulary.
pub(crate) fn refusal_category(code: &str) -> FailureCategory {
    match code {
        "AUTHORIZATION_PROOF_INVALID"
        | "AUTHORIZATION_PROOF_EXPIRED"
        | "SPEND_RESERVATION_REQUIRED"
        | "PUBLIC_ROUTES_DISABLED"
        | "TELEPHONY_DISABLED"
        | "INTERNAL_PERMISSION_DENIED" => FailureCategory::Execution,
        "NO_ROUTE" | "ROUTE_UNAVAILABLE" | "CAPACITY_EXHAUSTED" => FailureCategory::Routing,
        _ => FailureCategory::Execution,
    }
}

impl AppCallService {
    /// Claims and sends due commands; returns how many were processed.
    pub async fn dispatch_telephony_commands(&self, limit: u64) -> CallResult<usize> {
        let now = self.calls.now();
        let rows = CommandRow::find_by_statement(stmt(
            &format!(
                "UPDATE call_telephony_commands SET lease_until = $1 + make_interval(secs => {LEASE_SECONDS}), \
                   lease_claim = gen_random_uuid(), updated_at = GREATEST(updated_at, $1) \
                 WHERE id IN (SELECT id FROM call_telephony_commands \
                   WHERE status = 'PENDING' AND next_attempt_at <= $1 AND (lease_until IS NULL OR lease_until < $1) \
                   ORDER BY next_attempt_at, created_at, idempotency_key LIMIT $2 FOR UPDATE SKIP LOCKED) \
                 RETURNING {COMMAND_COLUMNS}"
            ),
            vec![now.into(), (limit.clamp(1, 200) as i64).into()],
        ))
        .all(self.db())
        .await?;
        let mut rows = rows;
        rows.sort_by(|a, b| {
            (a.created_at, &a.idempotency_key).cmp(&(b.created_at, &b.idempotency_key))
        });
        let mut done = 0;
        for row in rows {
            if self.dispatch_claimed(&row).await {
                done += 1;
            }
        }
        Ok(done)
    }

    /// Sends one claimed command inside a span naming its Call (and the Call's
    /// correlation): every log line of the dispatch is attributable without an
    /// identifier in a metric. A failure reschedules it (bounded by the attempt cap).
    async fn dispatch_claimed(&self, row: &CommandRow) -> bool {
        use tracing::Instrument;
        let span = tracing::info_span!(
            "telephony_command",
            call_id = %row.call_id,
            command_id = %row.id,
            command_kind = %row.kind,
            correlation_id = %correlation(&row.payload),
        );
        async {
            match self.dispatch_one(row).await {
                Ok(_) => true,
                Err(error) => {
                    tracing::warn!(command_id = %row.id, %error, "telephony command dispatch failed; will retry");
                    let _ = self.reschedule(row, "DISPATCH_ERROR").await;
                    false
                }
            }
        }
        .instrument(span)
        .await
    }

    /// Sends the due commands of one Call now (after the transaction that wrote them).
    /// Boxed: dispatching can lead back here (a refused leg ends the Call, which releases
    /// its other legs).
    pub(crate) fn dispatch_call(
        &self,
        call_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(self.dispatch_call_inner(call_id))
    }

    async fn dispatch_call_inner(&self, call_id: Uuid) {
        let rows = CommandRow::find_by_statement(stmt(
            &format!(
                "UPDATE call_telephony_commands SET lease_until = $2 + make_interval(secs => {LEASE_SECONDS}), \
                   lease_claim = gen_random_uuid() \
                 WHERE id IN (SELECT id FROM call_telephony_commands \
                   WHERE call_id = $1 AND status = 'PENDING' AND next_attempt_at <= $2 \
                     AND (lease_until IS NULL OR lease_until < $2) \
                   ORDER BY created_at, idempotency_key FOR UPDATE SKIP LOCKED) \
                 RETURNING {COMMAND_COLUMNS}"
            ),
            vec![call_id.into(), self.calls.now().into()],
        ))
        .all(self.db())
        .await;
        let Ok(mut rows) = rows else { return };
        // RETURNING has no order: send in creation order (prompts of one leg in sequence).
        rows.sort_by(|a, b| {
            (a.created_at, &a.idempotency_key).cmp(&(b.created_at, &b.idempotency_key))
        });
        for row in rows {
            self.dispatch_claimed(&row).await;
        }
    }

    /// A claimed command is retried later (bounded backoff). Guarded by the claim: a
    /// stale worker whose lease expired (and was reclaimed) changes nothing. The attempt
    /// that reaches the configured cap moves the command to DEAD instead — it stops
    /// retrying, and ONE deduplicated incident (per command) asks an operator to look:
    /// an ORIGINATE whose outcome is unknown is never re-invented as a new command.
    pub(super) async fn reschedule(&self, row: &CommandRow, code: &str) -> CallResult<()> {
        #[derive(FromQueryResult)]
        struct Rescheduled {
            status: String,
            attempts: i32,
            tenant_id: Uuid,
            application_id: Uuid,
            environment_id: Uuid,
        }
        let now = self.calls.now();
        let max = self.config.telephony_command_max_attempts.max(1);
        let done = Rescheduled::find_by_statement(stmt(
            "UPDATE call_telephony_commands SET attempts = attempts + 1, \
               status = CASE WHEN attempts + 1 >= $6 THEN 'DEAD' ELSE status END, \
               completed_at = CASE WHEN attempts + 1 >= $6 THEN GREATEST($4, created_at) ELSE completed_at END, \
               last_error_code = CASE WHEN attempts + 1 >= $6 THEN 'MAX_ATTEMPTS_EXCEEDED' ELSE $3 END, \
               next_attempt_at = $2, lease_until = NULL, lease_claim = NULL, \
               updated_at = GREATEST(updated_at, $4) \
             WHERE id = $1 AND status = 'PENDING' AND lease_claim IS NOT DISTINCT FROM $5 \
             RETURNING status, attempts, tenant_id, application_id, environment_id",
            vec![
                row.id.into(),
                (now + retry_delay(row.attempts)).into(),
                code.into(),
                now.into(),
                row.lease_claim.into(),
                max.into(),
            ],
        ))
        .one(self.db())
        .await?;
        let Some(done) = done.filter(|d| d.status == "DEAD") else {
            return Ok(());
        };
        let kind = CommandKind::parse(&row.kind)
            .map(|k| k.as_str())
            .unwrap_or("UNKNOWN");
        crate::metrics::inc(
            "calling_telephony_commands_dead_total",
            &[("kind", kind), ("reason", "MAX_ATTEMPTS_EXCEEDED")],
        );
        tracing::error!(
            call_id = %row.call_id,
            command_id = %row.id,
            command_kind = kind,
            attempts = done.attempts,
            last_code = code,
            "telephony command reached its attempt cap; DEAD (operator incident raised)"
        );
        let scope = crate::calls::command::CallScope::from_parts(
            crate::domain::TenantId::from_uuid(done.tenant_id),
            crate::domain::ApplicationId::from_uuid(done.application_id),
            crate::domain::EnvironmentId::from_uuid(done.environment_id),
        );
        let incident = crate::calls::app::incidents::Incident::new(
            crate::calls::app::incidents::IncidentCategory::TelephonyCommandDead,
            crate::calls::app::incidents::IncidentSeverity::High,
            scope,
            crate::calls::app::incidents::IncidentResource::TelephonyCommand,
            row.id,
        )
        .call(row.call_id)
        .evidence(json!({
            "commandKind": kind,
            "attempts": done.attempts,
            "lastErrorCode": &code[..code.len().min(64)],
        }));
        crate::calls::app::incidents::raise(self.db(), &incident, now).await;
        Ok(())
    }

    /// A claimed command waits until `at` (not an attempt). Guarded by the claim.
    async fn defer_until(&self, row: &CommandRow, at: DateTime<Utc>, code: &str) -> CallResult<()> {
        exec(
            self.db(),
            "UPDATE call_telephony_commands SET lease_until = NULL, lease_claim = NULL, next_attempt_at = $2, \
               last_error_code = $3, updated_at = GREATEST(updated_at, $4) \
             WHERE id = $1 AND status = 'PENDING' AND lease_claim IS NOT DISTINCT FROM $5",
            vec![
                row.id.into(),
                at.into(),
                code.into(),
                self.calls.now().into(),
                row.lease_claim.into(),
            ],
        )
        .await?;
        Ok(())
    }

    /// Completes a command THIS worker claimed (guarded by its claim: after a lost lease
    /// it changes nothing and returns `false`).
    pub(super) async fn complete(
        &self,
        row: &CommandRow,
        status: &str,
        code: Option<&str>,
        telephony_ref: Option<Uuid>,
    ) -> CallResult<bool> {
        let now = self.calls.now();
        Ok(exec(
            self.db(),
            "UPDATE call_telephony_commands SET status = $2, completed_at = GREATEST($3, created_at), \
               last_error_code = COALESCE($4, last_error_code), telephony_ref = COALESCE(telephony_ref, $5), \
               lease_until = NULL, lease_claim = NULL, updated_at = GREATEST(updated_at, $3) \
             WHERE id = $1 AND status = 'PENDING' AND lease_claim IS NOT DISTINCT FROM $6",
            vec![
                row.id.into(),
                status.into(),
                now.into(),
                code.map(|c| c[..c.len().min(64)].to_string()).into(),
                telephony_ref.into(),
                row.lease_claim.into(),
            ],
        )
        .await?
            == 1)
    }

    /// Completes ANOTHER command (not claimed by this worker) — only while no worker
    /// holds a live lease on it, so a command being sent right now is never declared
    /// dead underneath its sender.
    async fn complete_unclaimed(
        &self,
        id: Uuid,
        status: &str,
        code: Option<&str>,
    ) -> CallResult<bool> {
        let now = self.calls.now();
        Ok(exec(
            self.db(),
            "UPDATE call_telephony_commands SET status = $2, completed_at = GREATEST($3, created_at), \
               last_error_code = COALESCE($4, last_error_code), lease_until = NULL, lease_claim = NULL, \
               updated_at = GREATEST(updated_at, $3) \
             WHERE id = $1 AND status = 'PENDING' AND (lease_until IS NULL OR lease_until < $3)",
            vec![id.into(), status.into(), now.into(), code.into()],
        )
        .await?
            == 1)
    }

    async fn call_row(&self, call_id: Uuid) -> CallResult<calls::Model> {
        use sea_orm::EntityTrait;
        calls::Entity::find_by_id(call_id)
            .one(self.db())
            .await?
            .ok_or(CallError::NotFound)
    }

    pub(crate) async fn dispatch_one(&self, row: &CommandRow) -> CallResult<Dispatched> {
        let call = self.call_row(row.call_id).await?;
        match CommandKind::parse(&row.kind) {
            Some(CommandKind::Originate) => self.dispatch_originate(row, &call).await,
            Some(
                k @ (CommandKind::Terminate
                | CommandKind::Hold
                | CommandKind::Resume
                | CommandKind::Answer
                | CommandKind::Prompt
                | CommandKind::Capture
                | CommandKind::CaptureStop),
            ) => self.dispatch_control(row, &call, k).await,
            Some(CommandKind::Bridge) => self.dispatch_bridge(row, &call).await,
            Some(CommandKind::Unbridge) => self.dispatch_unbridge(row, &call).await,
            None => {
                self.complete(row, "DEAD", Some("UNKNOWN_KIND"), None)
                    .await?;
                Ok(Dispatched::Dead)
            }
        }
    }

    async fn dispatch_originate(
        &self,
        row: &CommandRow,
        call: &calls::Model,
    ) -> CallResult<Dispatched> {
        let leg_id = row.call_leg_id.ok_or(CallError::NotFound)?;
        let now = self.calls.now();
        let active = matches!(
            call.current_state,
            CallState::Dialing | CallState::Ringing | CallState::Answered | CallState::Connected
        );
        let valid_until: Option<DateTime<Utc>> = row.payload["authorization"]["validUntil"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        if !active {
            if matches!(
                call.current_state,
                CallState::Authorized | CallState::Resolving | CallState::Routing
            ) {
                // Not dialing yet (media resource still being ensured).
                self.defer_until(row, now + chrono::Duration::seconds(1), "AWAITING_DIALING")
                    .await?;
                return Ok(Dispatched::Retry);
            }
            if row.attempts == 0 {
                // Never sent: nothing can exist at Telephony.
                if self
                    .complete(row, "DEAD", Some("CALL_NOT_ACTIVE"), None)
                    .await?
                {
                    self.leg_never_executed(call, leg_id, row.attempt_id, None)
                        .await?;
                }
                return Ok(Dispatched::Dead);
            }
            if let Some(until) = valid_until.filter(|u| *u >= now) {
                // Possibly accepted: only a resubmission after the proof expired is
                // answered definitively without risking a new paid leg.
                self.defer_until(
                    row,
                    until + chrono::Duration::seconds(1),
                    "AWAITING_PROOF_EXPIRY",
                )
                .await?;
                return Ok(Dispatched::Retry);
            }
        }
        let party = parties(self.db(), call.id)
            .await?
            .into_iter()
            .find(|p| p.call_leg_id == Some(leg_id))
            .ok_or(CallError::NotFound)?;
        let delegated = self.delegated_for(call).await?;
        let correlation = correlation(&row.payload);
        let decision_id = row.payload["authorization"]["privacyDecisionId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(CallError::NotFound)?;
        let grant = match self
            .privacy
            .grant_for_execution(
                &delegated,
                &correlation,
                &GrantRequest {
                    contact_reference: party.contact_reference,
                    relay_session_id: call.privacy_relay_ref.ok_or(CallError::NotFound)?,
                    decision_id,
                    call_id: call.id,
                    call_leg_id: leg_id,
                },
            )
            .await
        {
            Ok(g) => g,
            Err(PrivacyRelayError::Unavailable) => {
                self.reschedule(row, "GRANT_UNAVAILABLE").await?;
                return Ok(Dispatched::Retry);
            }
            Err(PrivacyRelayError::NotFound) => {
                // Privacy Relay refused the grant (authority changed): nothing can be
                // executed for this leg. A resubmission of an earlier accepted command
                // would not need a new grant, so only a never-sent command is final here.
                if row.attempts == 0 {
                    if self
                        .complete(row, "REFUSED", Some("CONTACT_RESOLUTION_DENIED"), None)
                        .await?
                    {
                        self.leg_never_executed(
                            call,
                            leg_id,
                            row.attempt_id,
                            Some(FailureCategory::Execution),
                        )
                        .await?;
                    }
                    return Ok(Dispatched::Refused);
                }
                self.reschedule(row, "CONTACT_RESOLUTION_DENIED").await?;
                return Ok(Dispatched::Retry);
            }
        };
        // Part H §71: the scope must still be ACTIVE at the moment of the paid effect. A
        // suspension after the Call was authorized refuses the origination definitively.
        if !super::scope_is_active(self.db(), scope_of(call)).await? {
            if self
                .complete(row, "REFUSED", Some("SCOPE_SUSPENDED"), None)
                .await?
            {
                self.leg_never_executed(
                    call,
                    leg_id,
                    row.attempt_id,
                    active.then_some(FailureCategory::Execution),
                )
                .await?;
            }
            tracing::info!(call_id = %call.id, call_leg_id = %leg_id, "telephony originate refused: scope suspended");
            return Ok(Dispatched::Refused);
        }
        let mut command = row.payload.clone();
        command["destination"]["grantId"] = json!(grant.grant_id);
        command["destination"]["grantSecret"] = json!(grant.secret.as_str());
        command["destination"]["grantExpiresAt"] = json!(grant.expires_at);
        if command["destination"]["country"].is_null() {
            return Err(CallError::Validation("the destination country is unknown"));
        }
        // Count the submission BEFORE sending: a crash after the request leaves
        // attempts > 0, which forbids treating the command as never sent.
        // The paid effect is decided HERE, atomically with recording the attempt: this
        // worker must still hold its claim on a still-PENDING command and — for a first
        // submission of an active Call — the Call must still be active NOW (a revocation /
        // cancel that committed while the grant was being obtained wins). Otherwise
        // nothing is sent: a lost claim is left to its new holder; an ended Call releases
        // the claim at once so the command is re-evaluated (never sent → DEAD, and the
        // Call's TERMINATE completes as never executed).
        let recorded = exec(
            self.db(),
            "UPDATE call_telephony_commands SET attempts = attempts + 1, updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1 AND status = 'PENDING' AND lease_claim IS NOT DISTINCT FROM $3 \
               AND (NOT $4 OR EXISTS (SELECT 1 FROM calls c WHERE c.id = call_telephony_commands.call_id \
                     AND c.current_state IN ('DIALING', 'RINGING', 'ANSWERED', 'CONNECTED')))",
            vec![
                row.id.into(),
                now.into(),
                row.lease_claim.into(),
                active.into(),
            ],
        )
        .await?;
        if recorded != 1 {
            tracing::info!(call_id = %call.id, command_id = %row.id, "originate not sent: claim lost or the call is no longer active");
            self.defer_until(row, now, "CALL_NOT_ACTIVE_RECHECK")
                .await?;
            return Ok(Dispatched::Retry);
        }
        let result = self
            .public
            .telephony
            .originate(&delegated, &correlation, &command)
            .await;
        drop(command);
        match result {
            Ok(body) => {
                let telephony_leg = body["leg"]["telephonyLegId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok());
                let Some(telephony_leg) = telephony_leg else {
                    self.reschedule(row, "MALFORMED_RESPONSE").await?;
                    return Ok(Dispatched::Retry);
                };
                self.complete(row, "ACCEPTED", None, Some(telephony_leg))
                    .await?;
                self.bind_execution(call, leg_id, telephony_leg).await?;
                if !active {
                    // Accepted earlier for a Call that is no longer active: end it.
                    self.release_public_legs(call.id).await?;
                }
                tracing::info!(call_id = %call.id, call_leg_id = %leg_id, telephony_leg_id = %telephony_leg, "telephony originate accepted");
                Ok(Dispatched::Accepted)
            }
            Err(TelephonyError::Refused { code, .. }) if code == "EXECUTION_IN_PROGRESS" => {
                // A live execution exists for this leg (an earlier submission): never a
                // second one. Wait for its events / binding.
                self.reschedule(row, "EXECUTION_IN_PROGRESS").await?;
                Ok(Dispatched::Retry)
            }
            Err(TelephonyError::Refused { code, .. }) => {
                if self
                    .complete(row, "REFUSED", Some(&code[..code.len().min(64)]), None)
                    .await?
                {
                    self.leg_never_executed(
                        call,
                        leg_id,
                        row.attempt_id,
                        active.then(|| refusal_category(&code)),
                    )
                    .await?;
                }
                tracing::info!(call_id = %call.id, call_leg_id = %leg_id, code = %code, "telephony refused the origination");
                Ok(Dispatched::Refused)
            }
            Err(TelephonyError::Unavailable) => {
                self.reschedule(row, "TELEPHONY_UNAVAILABLE").await?;
                Ok(Dispatched::Retry)
            }
        }
    }

    /// Records the opaque Telephony leg id on the canonical CallLeg (set once).
    pub(crate) async fn bind_execution(
        &self,
        call: &calls::Model,
        leg_id: Uuid,
        telephony_leg: Uuid,
    ) -> CallResult<()> {
        let txn = self.db().begin().await?;
        let call = self
            .calls
            .lock_call(&txn, scope_of(call), CallId::from_uuid(call.id))
            .await?;
        let leg = leg_of(&txn, &call, CallLegId::from_uuid(leg_id)).await?;
        match leg.execution_binding_ref {
            Some(existing) if existing == telephony_leg => {}
            Some(_) => {
                return Err(CallError::Conflict(
                    "the leg already has another execution binding",
                ))
            }
            None => {
                exec(
                    &txn,
                    "UPDATE call_legs SET execution_binding_ref = $2, updated_at = GREATEST(updated_at, $3) \
                     WHERE id = $1 AND execution_binding_ref IS NULL",
                    vec![leg_id.into(), telephony_leg.into(), self.calls.now().into()],
                )
                .await?;
                let leg = leg_of(&txn, &call, CallLegId::from_uuid(leg_id)).await?;
                if leg.state == LegState::Created && !call.current_state.is_terminal() {
                    self.calls
                        .move_leg_in(
                            &txn,
                            &system_cmd(&call),
                            &call,
                            leg,
                            LegState::Dialing,
                            None,
                        )
                        .await?;
                }
            }
        }
        txn.commit().await?;
        Ok(())
    }

    /// A public leg Telephony never executed (never sent, or definitely refused): the leg
    /// ends, its attempt fails, its spend reservation is released and — for an active
    /// Call whose only public leg it was — the Call fails with `category`.
    async fn leg_never_executed(
        &self,
        call: &calls::Model,
        leg_id: Uuid,
        attempt_id: Option<Uuid>,
        category: Option<FailureCategory>,
    ) -> CallResult<()> {
        let txn = self.db().begin().await?;
        let call = self
            .calls
            .lock_call(&txn, scope_of(call), CallId::from_uuid(call.id))
            .await?;
        let cmd = system_cmd(&call);
        let leg = leg_of(&txn, &call, CallLegId::from_uuid(leg_id)).await?;
        if !leg.state.is_terminal() {
            let (target, cat) = match category {
                Some(c) => (LegState::Failed, Some(c)),
                None => (LegState::Cancelled, None),
            };
            let target = if leg.state.can_transition_to(target) {
                target
            } else {
                LegState::Cancelled
            };
            let cat = if target == LegState::Failed {
                cat
            } else {
                None
            };
            self.calls
                .move_leg_in(&txn, &cmd, &call, leg, target, cat)
                .await?;
        }
        if let Some(attempt_id) = attempt_id {
            let attempt = attempt_of(
                &txn,
                &call,
                crate::calls::ids::CallAttemptId::from_uuid(attempt_id),
            )
            .await?;
            let outcome = if category.is_some() {
                AttemptOutcome::Failed
            } else {
                AttemptOutcome::Abandoned
            };
            if attempt.outcome.can_transition_to(outcome) {
                self.calls
                    .finish_attempt_in(
                        &txn,
                        &cmd,
                        &call,
                        attempt,
                        outcome,
                        (outcome == AttemptOutcome::Failed).then_some(FailureCategory::Execution),
                    )
                    .await?;
            }
        }
        spend::release_in(&txn, leg_id, self.calls.now()).await?;
        txn.commit().await?;
        if let Some(category) = category {
            self.public_leg_failed(call.id, leg_id, category).await?;
        }
        Ok(())
    }

    async fn dispatch_control(
        &self,
        row: &CommandRow,
        call: &calls::Model,
        kind: CommandKind,
    ) -> CallResult<Dispatched> {
        let leg_id = row.call_leg_id.ok_or(CallError::NotFound)?;
        let leg = {
            use sea_orm::EntityTrait;
            crate::models::call_legs::Entity::find_by_id(leg_id)
                .one(self.db())
                .await?
                .ok_or(CallError::NotFound)?
        };
        let Some(telephony_leg) = leg.execution_binding_ref else {
            // No execution known yet. If the ORIGINATE was never sent, nothing exists.
            #[derive(FromQueryResult)]
            struct Orig {
                id: Uuid,
                status: String,
                attempts: i32,
                last_error_code: Option<String>,
            }
            let orig = Orig::find_by_statement(stmt(
                "SELECT id, status, attempts, last_error_code FROM call_telephony_commands \
                 WHERE call_leg_id = $1 AND kind = 'ORIGINATE'",
                vec![leg_id.into()],
            ))
            .one(self.db())
            .await?;
            match orig {
                Some(o) if o.status == "PENDING" && o.attempts == 0 => {
                    if !self
                        .complete_unclaimed(o.id, "DEAD", Some("CALL_NOT_ACTIVE"))
                        .await?
                    {
                        // Another worker holds the ORIGINATE right now (it may be sending
                        // it): look again once its outcome is known.
                        self.reschedule(row, "AWAITING_EXECUTION_BINDING").await?;
                        return Ok(Dispatched::Retry);
                    }
                    self.leg_never_executed(call, leg_id, None, None).await?;
                    self.complete(row, "ACCEPTED", Some("NEVER_EXECUTED"), None)
                        .await?;
                    return Ok(Dispatched::Accepted);
                }
                Some(o)
                    if o.status == "DEAD"
                        && o.attempts > 0
                        && o.last_error_code.as_deref() == Some("MAX_ATTEMPTS_EXCEEDED") =>
                {
                    // Submitted, outcome unknown, retries capped: the execution may exist.
                    // Keep waiting for its binding (this TERMINATE is capped in turn and
                    // raises its own incident) — never "never executed".
                    self.reschedule(row, "AWAITING_EXECUTION_BINDING").await?;
                    return Ok(Dispatched::Retry);
                }
                Some(o) if o.status != "PENDING" && o.status != "ACCEPTED" => {
                    self.complete(row, "ACCEPTED", Some("NEVER_EXECUTED"), None)
                        .await?;
                    return Ok(Dispatched::Accepted);
                }
                _ => {
                    self.reschedule(row, "AWAITING_EXECUTION_BINDING").await?;
                    return Ok(Dispatched::Retry);
                }
            }
        };
        let action = match kind {
            CommandKind::Terminate => LegControl::Terminate,
            CommandKind::Hold => LegControl::Hold,
            CommandKind::Answer => LegControl::Answer,
            CommandKind::Prompt => LegControl::Prompt,
            CommandKind::Capture => LegControl::Capture,
            CommandKind::CaptureStop => LegControl::CaptureStop,
            _ => LegControl::Resume,
        };
        if matches!(
            kind,
            CommandKind::Answer
                | CommandKind::Prompt
                | CommandKind::Capture
                | CommandKind::CaptureStop
        ) && (call.current_state.is_terminal() || call.current_state == CallState::Ending)
        {
            // A prompt / answer / capture for a Call that is over has nothing left to do
            // (ending the leg ends a running capture at the edge).
            self.complete(row, "DEAD", Some("CALL_NOT_ACTIVE"), None)
                .await?;
            return Ok(Dispatched::Dead);
        }
        let body = json!({
            "schemaVersion": "1",
            "commandId": row.id,
            "idempotencyKey": row.idempotency_key,
            "correlationId": correlation(&row.payload).as_str(),
            "reason": row.payload["reason"],
            "prompt": row.payload["prompt"],
            "capture": row.payload["capture"],
        });
        let mut body = body;
        for key in ["reason", "prompt", "capture"] {
            if body[key].is_null() {
                body.as_object_mut().map(|o| o.remove(key));
            }
        }
        match self
            .public
            .telephony
            .control(telephony_leg, action, &correlation(&row.payload), &body)
            .await
        {
            Ok(_) => {
                self.complete(row, "ACCEPTED", None, Some(telephony_leg))
                    .await?;
                Ok(Dispatched::Accepted)
            }
            Err(TelephonyError::Refused { code, .. })
                if code == "LEG_NOT_FOUND"
                    && matches!(
                        kind,
                        CommandKind::Answer | CommandKind::Prompt | CommandKind::Capture
                    )
                    && row.attempts < 10 =>
            {
                // Part F: an enterprise dialog's leg is persisted by Telephony right after
                // Calling accepted its offer — a first ANSWER / PROMPT may arrive before
                // it exists. Retried (bounded); never mistaken for "already over".
                self.reschedule(row, "LEG_NOT_YET_KNOWN").await?;
                Ok(Dispatched::Retry)
            }
            Err(TelephonyError::Refused { code, .. })
                if code == "LEG_NOT_ACTIVE" || code == "LEG_NOT_FOUND" =>
            {
                // Already over at Telephony: nothing to do.
                self.complete(row, "ACCEPTED", Some(&code), Some(telephony_leg))
                    .await?;
                Ok(Dispatched::Accepted)
            }
            Err(TelephonyError::Refused { code, .. }) => {
                self.complete(row, "REFUSED", Some(&code[..code.len().min(64)]), None)
                    .await?;
                Ok(Dispatched::Refused)
            }
            Err(TelephonyError::Unavailable) => {
                self.reschedule(row, "TELEPHONY_UNAVAILABLE").await?;
                Ok(Dispatched::Retry)
            }
        }
    }

    /// Public legs of a Call that are not terminal.
    pub(crate) async fn live_public_legs(&self, call_id: Uuid) -> CallResult<i64> {
        use sea_orm::ConnectionTrait;
        let row = self
            .db()
            .query_one(stmt(
                "SELECT count(*) FROM call_legs WHERE call_id = $1 AND (leg_kind = 'PUBLIC_NETWORK' \
                   OR (leg_kind = 'PRIVATE_NETWORK' AND execution_binding_ref IS NOT NULL)) \
                 AND state NOT IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')",
                vec![call_id.into()],
            ))
            .await?;
        Ok(row
            .map(|r| r.try_get_by_index::<i64>(0))
            .transpose()?
            .unwrap_or(0))
    }

    /// Asks Telephony to end every live public leg of a Call (idempotent: one TERMINATE
    /// per leg), then sends the commands.
    ///
    /// When the Call entered ENDING or a terminal state, the TERMINATE intents were
    /// already written by the transition's own transaction (deferred trigger
    /// `trg_calls_release_public_legs`, migration 000043): this post-commit step then only
    /// finds them (same idempotency key) and sends them. A crash before it leaves durable
    /// PENDING commands that the dispatch worker sends.
    pub(crate) async fn release_public_legs(&self, call_id: Uuid) -> CallResult<usize> {
        let call = self.call_row(call_id).await?;
        let txn = self.db().begin().await?;
        let mut queued = 0;
        let mut leg_ids: Vec<Uuid> = parties(&txn, call_id)
            .await?
            .into_iter()
            .filter_map(|p| p.call_leg_id)
            .collect();
        // Part F: an enterprise SIP device's dialog (bound PRIVATE_NETWORK leg).
        {
            use sea_orm::ConnectionTrait;
            for row in txn
                .query_all(stmt(
                    "SELECT id FROM call_legs WHERE call_id = $1 AND leg_kind = 'PRIVATE_NETWORK' \
                     AND execution_binding_ref IS NOT NULL",
                    vec![call_id.into()],
                ))
                .await?
            {
                leg_ids.push(row.try_get_by_index::<Uuid>(0)?);
            }
        }
        for leg_id in leg_ids {
            let leg = leg_of(&txn, &call, CallLegId::from_uuid(leg_id)).await?;
            if leg.state.is_terminal() {
                continue;
            }
            enqueue_in(
                &txn,
                &call,
                CommandKind::Terminate,
                Some(leg_id),
                None,
                None,
                &format!("terminate:{leg_id}"),
                json!({ "commandId": Uuid::new_v4(), "correlationId": crate::calls::app::call_correlation(&call).as_str(), "reason": "CALL_ENDED" }),
                self.calls.now(),
            )
            .await?;
            queued += 1;
        }
        txn.commit().await?;
        if queued > 0 {
            self.dispatch_call(call_id).await;
        }
        Ok(queued)
    }
}
