//! Read-back reconciliation of uncertain provider commands (Part IV Workstream G §15).
//!
//! An UNKNOWN command is finalized ONLY from authoritative provider evidence: a read-back
//! (`inspect`) of the provider object, not older than what Numbering already saw. The
//! read-back is appended as attempt evidence. When it shows the effect happened, the
//! command's success transition is applied by the RECONCILIATION actor; when it shows it
//! did not, the intent is reverted; when it shows something else (another owner, a
//! vanished number, a half-applied operation that contradicts the request) the resource
//! stays fail-closed and a drift incident is raised. An unreachable provider or a stale
//! read-back leaves everything UNKNOWN for the next attempt. This is the per-command
//! primitive; recurring inventory-wide reconciliation is Workstream V.

use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

use super::{
    commands::{insert_attempt, lock_command, CommandStatus, ProviderCommands},
    incidents, CommandOperation, OperationKind, OperationReceipt, ProviderFailure,
    ProviderNumberState, ProviderNumberStatus as St, ProviderOutcome,
};
use crate::{
    domain::RequestContext,
    infra::jobs::{self, Resolution},
    numbers::{stmt, DomainResult, FailureCategory},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciled {
    /// No authoritative answer yet (provider unreachable, stale read-back).
    Pending,
    Happened,
    DidNotHappen,
    /// The provider state contradicts the command: fail closed, incident raised.
    Drift,
    /// Nothing to reconcile (the command is not UNKNOWN).
    AlreadyFinal,
}

enum Verdict {
    Happened,
    DidNotHappen(FailureCategory),
    Drift(&'static str),
}

fn judge(op: &OperationKind, request_id: Uuid, s: &ProviderNumberState) -> Verdict {
    use Verdict::*;
    match op {
        OperationKind::Reserve { .. } => match s.status {
            St::Held if s.owned_by_us && s.held_by_request == Some(request_id) => Happened,
            St::Available => DidNotHappen(FailureCategory::ProviderUnavailable),
            St::Unavailable | St::NotFound | St::Released => {
                DidNotHappen(FailureCategory::NotObtainable)
            }
            _ => Drift("HELD_OR_OWNED_BY_ANOTHER_REQUEST"),
        },
        OperationKind::Provision { capabilities } => match s.status {
            St::Provisioned | St::Suspended if s.owned_by_us => {
                if capabilities.iter().any(|c| s.capabilities.contains(c)) {
                    Happened
                } else {
                    Drift("PROVISIONED_WITHOUT_REQUESTED_CAPABILITIES")
                }
            }
            St::Held if s.owned_by_us => DidNotHappen(FailureCategory::ProviderRejected),
            _ => Drift("NUMBER_NOT_OURS"),
        },
        OperationKind::Release => match s.status {
            St::Released | St::NotFound => Happened,
            St::Available | St::Unavailable if !s.owned_by_us => Happened,
            St::Provisioned | St::Suspended | St::Held if s.owned_by_us => {
                DidNotHappen(FailureCategory::ProviderRejected)
            }
            _ => Drift("UNEXPECTED_RELEASE_STATE"),
        },
        OperationKind::ConfigureVoice { route_ref } => match (s.owned_by_us, s.voice_route) {
            (true, Some(r)) if r == *route_ref => Happened,
            (true, _) => DidNotHappen(FailureCategory::ProviderRejected),
            _ => Drift("NUMBER_NOT_OURS"),
        },
        OperationKind::ConfigureSms { route_ref } => match (s.owned_by_us, s.sms_route) {
            (true, Some(r)) if r == *route_ref => Happened,
            (true, _) => DidNotHappen(FailureCategory::ProviderRejected),
            _ => Drift("NUMBER_NOT_OURS"),
        },
        OperationKind::Suspend { .. } => match s.status {
            St::Suspended if s.owned_by_us => Happened,
            St::Provisioned if s.owned_by_us => DidNotHappen(FailureCategory::ProviderRejected),
            _ => Drift("NUMBER_NOT_OURS"),
        },
        OperationKind::Restore => match s.status {
            St::Provisioned if s.owned_by_us => Happened,
            St::Suspended if s.owned_by_us => DidNotHappen(FailureCategory::ProviderRejected),
            _ => Drift("NUMBER_NOT_OURS"),
        },
        OperationKind::PortIn { .. } => match s.status {
            St::Provisioned if s.owned_by_us => Happened,
            _ => DidNotHappen(FailureCategory::ProviderRejected),
        },
    }
}

impl ProviderCommands {
    /// The highest provider version Numbering has already seen for an object.
    async fn known_sequence(&self, provider_id: &str, external_ref: &str) -> DomainResult<i64> {
        #[derive(FromQueryResult)]
        struct Max {
            n: Option<i64>,
        }
        let from_events = Max::find_by_statement(stmt(
            "SELECT MAX(provider_sequence) AS n FROM number_provider_events WHERE provider_id = $1 AND external_ref = $2 \
               AND disposition <> 'STALE'",
            vec![provider_id.into(), external_ref.into()],
        ))
        .one(self.numbers.db())
        .await?
        .and_then(|m| m.n)
        .unwrap_or(0);
        let from_attempts = Max::find_by_statement(stmt(
            "SELECT MAX((a.response->>'sequence')::bigint) AS n FROM number_provider_command_attempts a \
               JOIN number_provider_commands c ON c.id = a.command_id \
              WHERE c.provider_id = $1 AND c.external_ref = $2 AND a.response ? 'sequence'",
            vec![provider_id.into(), external_ref.into()],
        ))
        .one(self.numbers.db())
        .await?
        .and_then(|m| m.n)
        .unwrap_or(0);
        Ok(from_events.max(from_attempts))
    }

    /// Reconciles one UNKNOWN command from authoritative read-back (see module rules).
    pub async fn reconcile_command(
        &self,
        ctx: &RequestContext,
        command_id: Uuid,
    ) -> DomainResult<Reconciled> {
        let cmd = self.command(command_id).await?;
        if cmd.status() != CommandStatus::Unknown {
            return Ok(Reconciled::AlreadyFinal);
        }
        let Some(provider) = self.registry.get(&cmd.provider_id) else {
            return Ok(Reconciled::Pending);
        };
        let request = cmd.request();
        let started = self.now();
        let state = match provider.inspect(&request.external_ref).await {
            ProviderOutcome::Succeeded(s) => s,
            _ => {
                incidents::raise(
                    self.numbers.db(),
                    incidents::Category::ProviderOutage,
                    &format!("outage:{}", cmd.provider_id),
                    incidents::Subject {
                        provider_id: Some(cmd.provider_id.clone()),
                        ..Default::default()
                    },
                    json!({ "operation": "INSPECT" }),
                    started,
                )
                .await?;
                return Ok(Reconciled::Pending);
            }
        };
        // A read-back older than what was already seen is not authoritative.
        if state.sequence
            < self
                .known_sequence(&cmd.provider_id, &request.external_ref)
                .await?
        {
            crate::metrics::inc("numbering_provider_stale_read_backs_total", &[]);
            return Ok(Reconciled::Pending);
        }
        let verdict = judge(&request.operation, cmd.id, &state);
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let cmd = lock_command(&txn, command_id).await?;
        if cmd.status() != CommandStatus::Unknown {
            txn.commit().await?;
            return Ok(Reconciled::AlreadyFinal);
        }
        #[derive(FromQueryResult)]
        struct Next {
            n: i32,
        }
        let next = Next::find_by_statement(stmt(
            "SELECT COALESCE(MAX(attempt), 0) + 1 AS n FROM number_provider_command_attempts WHERE command_id = $1",
            vec![cmd.id.into()],
        ))
        .one(&txn)
        .await?
        .map(|n| n.n)
        .unwrap_or(1);
        let evidence = insert_attempt(
            &txn,
            cmd.id,
            next,
            "READ_BACK",
            true,
            None,
            Some(state.status.as_str()),
            json!({
                "status": state.status.as_str(),
                "sequence": state.sequence,
                "ownedByUs": state.owned_by_us,
                "capabilities": state.capabilities,
            }),
            started,
            now,
        )
        .await?;
        let outcome = match verdict {
            Verdict::Happened => {
                let receipt = OperationReceipt {
                    provider_request_ref: format!("read-back-{}", evidence.id.simple()),
                    external_ref: request.external_ref.clone(),
                    state: Some(state.clone()),
                };
                self.complete_success(
                    &txn,
                    ctx,
                    &cmd,
                    &receipt,
                    evidence.id,
                    CommandStatus::ReconciledSucceeded,
                    now,
                )
                .await?;
                if cmd.operation() == CommandOperation::Provision {
                    if let OperationKind::Provision { capabilities } = &request.operation {
                        if capabilities.iter().any(|c| !state.capabilities.contains(c)) {
                            // Partial provisioning: what exists is recorded; the gap is drift.
                            incidents::raise(
                                &txn,
                                incidents::Category::ProviderStateDrift,
                                &format!("partial:{}", cmd.id),
                                incidents::Subject {
                                    provider_id: Some(cmd.provider_id.clone()),
                                    number_resource_id: Some(cmd.number_resource_id),
                                    command_id: Some(cmd.id),
                                },
                                json!({ "reason": "PARTIAL_PROVISIONING", "served": state.capabilities, "requested": capabilities }),
                                now,
                            )
                            .await?;
                        }
                    }
                }
                Reconciled::Happened
            }
            Verdict::DidNotHappen(category) => {
                self.complete_failure(
                    &txn,
                    ctx,
                    &cmd,
                    &ProviderFailure {
                        code: "RECONCILED_NOT_APPLIED".into(),
                        category,
                        transmitted: true,
                        retryable: false,
                    },
                    CommandStatus::ReconciledFailed,
                    now,
                )
                .await?;
                Reconciled::DidNotHappen
            }
            Verdict::Drift(reason) => {
                incidents::raise(
                    &txn,
                    incidents::Category::ProviderStateDrift,
                    &format!("drift:{}", cmd.id),
                    incidents::Subject {
                        provider_id: Some(cmd.provider_id.clone()),
                        number_resource_id: Some(cmd.number_resource_id),
                        command_id: Some(cmd.id),
                    },
                    json!({ "reason": reason, "providerStatus": state.status.as_str() }),
                    now,
                )
                .await?;
                txn.commit().await?;
                return Ok(Reconciled::Drift);
            }
        };
        incidents::resolve(
            &txn,
            &format!("command:{}", cmd.id),
            "provider-reconciler",
            now,
        )
        .await?;
        if let Some(job) = cmd.job_id {
            jobs::resolve_unknown(
                &txn,
                job,
                if outcome == Reconciled::Happened {
                    Resolution::Happened
                } else {
                    Resolution::DidNotHappen
                },
                now,
            )
            .await?;
        }
        txn.commit().await?;
        crate::metrics::inc(
            "numbering_provider_reconciliations_total",
            &[(
                "outcome",
                if outcome == Reconciled::Happened {
                    "happened"
                } else {
                    "did_not_happen"
                },
            )],
        );
        Ok(outcome)
    }

    /// Reconciles every UNKNOWN command (bounded batch); returns the outcomes.
    pub async fn reconcile_unknown(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<Vec<(Uuid, Reconciled)>> {
        #[derive(FromQueryResult)]
        struct Id {
            id: Uuid,
        }
        let ids = Id::find_by_statement(stmt(
            "SELECT id FROM number_provider_commands WHERE status = 'UNKNOWN' ORDER BY updated_at LIMIT $1",
            vec![(limit.clamp(1, 1_000) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let mut out = Vec::new();
        for i in ids {
            out.push((i.id, self.reconcile_command(ctx, i.id).await?));
        }
        Ok(out)
    }
}
