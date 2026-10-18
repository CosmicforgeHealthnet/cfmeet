//! Workstream V — recurring lifecycle reconciliation.
//!
//! The Workstream G framework already authenticates, de-duplicates and orders provider
//! callbacks and reconciles each UNKNOWN command by read-back. This sweep adds the
//! RECURRING comparison of CF Meet's lifecycle with the provider's read-back for every
//! number CF Meet controls, and the stewardship workflows' own uncertain states:
//!
//! * inventory freshness (stale AVAILABLE observations are counted, never trusted);
//! * suspension: locally suspended but routing at the provider → drift incident (the
//!   number stays suspended); ACTIVE locally but suspended / gone at the provider →
//!   incident and a fail-closed PROVIDER suspension;
//! * release / quarantine: a CF Meet-controlled quarantined or reassignable number the
//!   provider no longer holds → incident, quarantine held (never reassigned);
//! * capabilities / routes: the provider dropping a bound capability or a configured
//!   route → incident;
//! * UNKNOWN provider commands → the G reconciliation (read-back, never re-sent); port
//!   requests and releases in an uncertain state → their own advance (status queries).
//!
//! Numbers with a provider command in flight are skipped (their command decides). A
//! provider outage leaves everything as it is. Nothing here repeats a possibly executed
//! provider command, and nothing resurrects a released number.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    count,
    suspension::{SuspendRequest, SuspensionSource},
    Stewardship,
};
use crate::{
    domain::RequestContext,
    numbers::{
        assignments::AssignmentRow,
        lifecycle::{transition_in, TransitionRequest},
        resources::{lock, ResourceRow},
        stmt, Actor, AssignmentStatus, DomainResult, LifecycleState,
    },
    providers::{incidents, ProviderNumberStatus, ProviderOutcome},
};

/// One sweep's result (counts only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileReport {
    pub inspected: usize,
    pub drift: usize,
    pub fail_closed: usize,
    pub skipped_in_flight: usize,
    pub provider_unavailable: usize,
    pub stale_inventory: i64,
    pub unknown_commands_reconciled: usize,
}

impl Stewardship {
    /// One reconciliation sweep over at most `limit` controlled numbers.
    pub async fn reconcile_lifecycle(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let now = self.now();
        report.stale_inventory = count(
            self.numbers.db(),
            "SELECT count(*) AS n FROM number_resources WHERE state = 'AVAILABLE' \
               AND (observation_fresh_until IS NULL OR observation_fresh_until <= $1)",
            vec![now.into()],
        )
        .await?;
        let Some(p) = &self.providers else {
            return Ok(report);
        };
        report.unknown_commands_reconciled = p.reconcile_unknown(ctx, limit).await?.len();
        self.advance_releases(ctx, limit).await?;
        self.advance_ports(ctx, limit).await?;
        let controlled = ResourceRow::find_by_statement(stmt(
            "SELECT r.* FROM number_resources r WHERE r.service_class = 'DCN' \
               AND r.state IN ('PROVISIONED', 'ACTIVE', 'SUSPENDED', 'QUARANTINED', 'REASSIGNABLE') \
               AND EXISTS (SELECT 1 FROM number_provider_bindings b WHERE b.number_resource_id = r.id AND b.status = 'ACTIVE') \
             ORDER BY r.updated_at LIMIT $1",
            vec![(limit.clamp(1, 1000) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        for r in controlled {
            if count(
                self.numbers.db(),
                "SELECT count(*) AS n FROM number_provider_commands WHERE number_resource_id = $1 \
                   AND status IN ('PENDING', 'DISPATCHING', 'UNKNOWN')",
                vec![r.id.into()],
            )
            .await?
                > 0
            {
                report.skipped_in_flight += 1;
                continue;
            }
            let state = match p.inspect_resource(r.id).await? {
                Some(ProviderOutcome::Succeeded(s)) => s,
                _ => {
                    report.provider_unavailable += 1;
                    continue;
                }
            };
            report.inspected += 1;
            let gone = !state.owned_by_us
                || matches!(
                    state.status,
                    ProviderNumberStatus::Released
                        | ProviderNumberStatus::NotFound
                        | ProviderNumberStatus::Unavailable
                );
            let local = r.lifecycle_state();
            let drift: Option<&'static str> = match local {
                _ if gone => Some("PROVIDER_NO_LONGER_HOLDS"),
                LifecycleState::Active if state.status == ProviderNumberStatus::Suspended => {
                    Some("PROVIDER_SUSPENDED_ACTIVE_NUMBER")
                }
                LifecycleState::Suspended
                    if state.status == ProviderNumberStatus::Provisioned
                        && self.suspension_confirmed(r.id).await? =>
                {
                    Some("PROVIDER_ROUTING_SUSPENDED_NUMBER")
                }
                _ => None,
            };
            let route_drift = self.route_drift(&r, &state).await?;
            if drift.is_none() && route_drift.is_none() {
                continue;
            }
            report.drift += 1;
            let reason = drift.or(route_drift).expect("one is set");
            let category = if gone {
                incidents::Category::ProviderReleasedUnexpectedly
            } else {
                incidents::Category::ProviderStateDrift
            };
            let txn = self.numbers.db().begin().await?;
            incidents::raise(
                &txn,
                category,
                &format!("lifecycle-drift:{}:{reason}", r.id),
                incidents::Subject {
                    provider_id: None,
                    number_resource_id: Some(r.id),
                    command_id: None,
                },
                json!({ "reason": reason, "localState": r.state, "providerStatus": state.status }),
                now,
            )
            .await?;
            // Fail closed where CF Meet would otherwise route or reassign.
            let failed_closed = match (local, drift) {
                (LifecycleState::Active, Some(_)) => {
                    let a = AssignmentRow::find_by_statement(stmt(
                        "SELECT * FROM dcn_assignments WHERE number_resource_id = $1 AND status = 'ACTIVE' FOR UPDATE",
                        vec![r.id.into()],
                    ))
                    .one(&txn)
                    .await?;
                    if let Some(a) = a {
                        self.suspend_in(
                            &txn,
                            ctx,
                            &a,
                            Actor::reconciliation("numbering.reconcile"),
                            &SuspendRequest {
                                assignment_id: a.id,
                                reason_code: "PROVIDER_STATE_DRIFT".into(),
                                source: SuspensionSource::Provider,
                                evidence_ref: None,
                            },
                            now,
                        )
                        .await?;
                        true
                    } else {
                        false
                    }
                }
                (LifecycleState::Reassignable, Some(_)) => {
                    self.hold_reassignable_in(&txn, ctx, r.id, reason, now)
                        .await?;
                    true
                }
                (LifecycleState::Quarantined, Some(_)) => {
                    txn.execute(stmt(
                        "UPDATE number_quarantines SET status = 'HELD_FOR_REVIEW', review_required = TRUE, \
                           completion_reason = $2, revision = revision + 1, updated_at = $3 \
                         WHERE number_resource_id = $1 AND status = 'ACTIVE'",
                        vec![r.id.into(), reason.into(), now.into()],
                    ))
                    .await?;
                    true
                }
                _ => false,
            };
            txn.commit().await?;
            if failed_closed {
                report.fail_closed += 1;
            }
        }
        Ok(report)
    }

    async fn suspension_confirmed(&self, resource_id: Uuid) -> DomainResult<bool> {
        Ok(count(
            self.numbers.db(),
            "SELECT count(*) AS n FROM number_suspensions WHERE number_resource_id = $1 \
               AND status = 'SUSPENDED' AND provider_state = 'CONFIRMED'",
            vec![resource_id.into()],
        )
        .await?
            > 0)
    }

    /// A configured route the provider no longer carries, or a bound capability it dropped.
    async fn route_drift(
        &self,
        r: &ResourceRow,
        state: &crate::providers::ProviderNumberState,
    ) -> DomainResult<Option<&'static str>> {
        if !matches!(r.lifecycle_state(), LifecycleState::Active) {
            return Ok(None);
        }
        for route in self.numbers.current_routes(r.id).await? {
            if route.status != "CONFIGURED" {
                continue;
            }
            let at_provider = if route.family == "VOICE" {
                state.voice_route
            } else {
                state.sms_route
            };
            if at_provider != Some(route.route_ref) {
                return Ok(Some("ROUTE_DRIFT"));
            }
        }
        let dropped = self
            .numbers
            .bindings(r.id)
            .await?
            .into_iter()
            .filter(|b| b.status == "ACTIVE")
            .flat_map(|b| b.capabilities)
            .any(|c| !state.capabilities.iter().any(|x| x.as_str() == c));
        Ok(dropped.then_some("CAPABILITY_DRIFT"))
    }

    /// A reassignable number whose provider state contradicts CF Meet goes back to a held
    /// quarantine: it is never offered for reassignment.
    async fn hold_reassignable_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        ctx: &RequestContext,
        resource_id: Uuid,
        reason: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        let r = lock(txn, resource_id).await?;
        let until = now + Duration::days(30);
        transition_in(
            txn,
            ctx,
            r.id,
            TransitionRequest::new(
                LifecycleState::Quarantined,
                r.revision,
                Actor::reconciliation("numbering.reconcile"),
                "PROVIDER_STATE_DRIFT",
            )
            .with_quarantine_until(until),
            now,
        )
        .await?;
        let previous = AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE number_resource_id = $1 ORDER BY requested_at DESC LIMIT 1",
            vec![r.id.into()],
        ))
        .one(txn)
        .await?
        .filter(|a| a.status() == AssignmentStatus::Closed);
        txn.execute(stmt(
            "INSERT INTO number_quarantines (id, number_resource_id, previous_assignment_id, disposition, policy_ref, \
               cooldown_days, started_at, quarantine_until, status, review_required, completion_reason, updated_at) \
             VALUES ($1, $2, $3, 'POOL_RETURN', 'QP1_DRIFT_HOLD', 30, $4, $5, 'HELD_FOR_REVIEW', TRUE, $6, $4)",
            vec![
                Uuid::new_v4().into(),
                r.id.into(),
                previous.map(|a| a.id).into(),
                now.into(),
                until.into(),
                reason.into(),
            ],
        ))
        .await?;
        Ok(())
    }
}

use sea_orm::ConnectionTrait;
