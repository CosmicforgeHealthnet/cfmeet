//! Workstream R — permanent history, audit and recovery evidence.
//!
//! The evidence itself stays where it is written, append-only: the lifecycle audit, the
//! assignment history (every owner, class, routing revision, capability snapshot, KYC /
//! eligibility and risk references), suspensions, releases, quarantines, late activity,
//! risk decisions and incidents. This module READS it:
//!
//! * **ownership periods** — who held a number when (owner scope, kind, class, the start
//!   and end of routing authority), derived from the assignment records;
//! * **ownership verification** for account recovery / legal requests — an operator
//!   states WHO is claimed to have held the number WHEN and gets MATCH / NO_MATCH /
//!   AMBIGUOUS. Possession of a (reassigned) number never establishes anything: the
//!   claim is checked against the record, other owners are never revealed;
//! * every privileged read is audited (`number_history_access`: operator, purpose, case
//!   reference, outcome) BEFORE anything is returned, and owner references are masked
//!   (a keyed digest per case) unless verification matched.
//!
//! Calling and SMS history stay with their owning services.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::Stewardship;
use crate::{
    domain::RequestContext,
    numbers::{stmt, Actor, ActorKind, DomainError, DomainResult},
};

/// Why an operator reads history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AccessPurpose {
    AccountRecovery,
    LegalRequest,
    FraudInvestigation,
    CustomerSupport,
    Audit,
}

impl AccessPurpose {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccessPurpose::AccountRecovery => "ACCOUNT_RECOVERY",
            AccessPurpose::LegalRequest => "LEGAL_REQUEST",
            AccessPurpose::FraudInvestigation => "FRAUD_INVESTIGATION",
            AccessPurpose::CustomerSupport => "CUSTOMER_SUPPORT",
            AccessPurpose::Audit => "AUDIT",
        }
    }
}

/// The justification every privileged read carries.
#[derive(Debug, Clone)]
pub struct AccessJustification {
    pub purpose: AccessPurpose,
    /// The operator's case / ticket reference.
    pub case_ref: String,
}

#[derive(Debug, Clone, PartialEq, FromQueryResult)]
struct PeriodRow {
    assignment_id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    owner_kind: String,
    owner_ref: Uuid,
    ownership_class: String,
    status: String,
    activated_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    closed_at: Option<DateTime<Utc>>,
    close_reason: Option<String>,
    predecessor_assignment_id: Option<Uuid>,
}

/// One owner's period of routing authority (masked).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipPeriod {
    pub assignment_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub owner_kind: String,
    /// A per-case digest of the owner reference: comparable within one case, never the id.
    pub owner_digest: String,
    pub ownership_class: String,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub close_reason: Option<String>,
    pub transferred_from: Option<Uuid>,
}

/// The claim an operator verifies.
#[derive(Debug, Clone)]
pub struct OwnershipClaim {
    pub tenant_id: Uuid,
    pub owner_ref: Uuid,
    pub at: DateTime<Utc>,
}

/// A verification answer: never another owner.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipVerification {
    pub outcome: &'static str,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub assignment_id: Option<Uuid>,
}

fn valid_case(case: &str) -> bool {
    (3..=64).contains(&case.len())
        && case.as_bytes()[0].is_ascii_alphanumeric()
        && case
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

impl Stewardship {
    fn require_operator(actor: &Actor, j: &AccessJustification) -> DomainResult<()> {
        if actor.kind != ActorKind::Operator {
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "number history is restricted to operators",
            ));
        }
        if !valid_case(&j.case_ref) {
            return Err(DomainError::Validation(
                "a case reference is 3–64 of [A-Za-z0-9._:-]".into(),
            ));
        }
        Ok(())
    }

    async fn audit_access<C: ConnectionTrait>(
        db: &C,
        ctx: &RequestContext,
        actor: &Actor,
        resource_id: Uuid,
        j: &AccessJustification,
        kind: &'static str,
        outcome: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        db.execute(stmt(
            "INSERT INTO number_history_access (id, number_resource_id, operator_ref, purpose, case_ref, \
               access_kind, outcome, accessed_at, correlation_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            vec![
                Uuid::new_v4().into(),
                resource_id.into(),
                actor.reference.clone().into(),
                j.purpose.as_str().into(),
                j.case_ref.clone().into(),
                kind.into(),
                outcome.into(),
                now.into(),
                ctx.correlation_id.as_str().into(),
            ],
        ))
        .await?;
        crate::metrics::inc(
            "numbering_history_access_total",
            &[("kind", kind), ("purpose", j.purpose.as_str())],
        );
        Ok(())
    }

    async fn periods<C: ConnectionTrait>(
        db: &C,
        resource_id: Uuid,
    ) -> DomainResult<Vec<PeriodRow>> {
        Ok(PeriodRow::find_by_statement(stmt(
            "SELECT id AS assignment_id, tenant_id, application_id, environment_id, owner_kind, owner_ref, \
                    ownership_class, status, activated_at, \
                    COALESCE(release_requested_at, closed_at) AS ended_at, closed_at, close_reason, \
                    predecessor_assignment_id \
               FROM dcn_assignments WHERE number_resource_id = $1 ORDER BY requested_at, id",
            vec![resource_id.into()],
        ))
        .all(db)
        .await?)
    }

    /// Every owner's period of a number, masked, after the read is audited.
    pub async fn ownership_periods(
        &self,
        ctx: &RequestContext,
        actor: &Actor,
        resource_id: Uuid,
        j: &AccessJustification,
    ) -> DomainResult<Vec<OwnershipPeriod>> {
        Self::require_operator(actor, j)?;
        self.numbers.resource(resource_id).await?;
        let txn = self.numbers.db().begin().await?;
        Self::audit_access(
            &txn,
            ctx,
            actor,
            resource_id,
            j,
            "OWNERSHIP_PERIODS",
            "READ",
            self.now(),
        )
        .await?;
        let rows = Self::periods(&txn, resource_id).await?;
        txn.commit().await?;
        Ok(rows
            .into_iter()
            .map(|p| OwnershipPeriod {
                assignment_id: p.assignment_id,
                tenant_id: p.tenant_id,
                application_id: p.application_id,
                environment_id: p.environment_id,
                owner_kind: p.owner_kind,
                owner_digest: super::digest(&format!("{}:{}", j.case_ref, p.owner_ref))[..16]
                    .to_string(),
                ownership_class: p.ownership_class,
                status: p.status,
                started_at: p.activated_at,
                ended_at: p.ended_at,
                close_reason: p.close_reason,
                transferred_from: p.predecessor_assignment_id,
            })
            .collect())
    }

    /// Verifies a claimed historical ownership (audited with its outcome).
    pub async fn verify_ownership(
        &self,
        ctx: &RequestContext,
        actor: &Actor,
        resource_id: Uuid,
        claim: &OwnershipClaim,
        j: &AccessJustification,
    ) -> DomainResult<OwnershipVerification> {
        Self::require_operator(actor, j)?;
        self.numbers.resource(resource_id).await?;
        let txn = self.numbers.db().begin().await?;
        let periods = Self::periods(&txn, resource_id).await?;
        let covering: Vec<&PeriodRow> = periods
            .iter()
            .filter(|p| {
                p.activated_at.is_some_and(|s| s <= claim.at)
                    && p.ended_at.is_none_or(|e| claim.at < e)
            })
            .collect();
        let (outcome, matched) = match covering.as_slice() {
            [p] if p.tenant_id == claim.tenant_id && p.owner_ref == claim.owner_ref => {
                ("MATCH", Some(*p))
            }
            [] => ("NO_MATCH", None),
            [_] => ("NO_MATCH", None),
            _ => ("AMBIGUOUS", None),
        };
        Self::audit_access(
            &txn,
            ctx,
            actor,
            resource_id,
            j,
            "OWNERSHIP_VERIFICATION",
            outcome,
            self.now(),
        )
        .await?;
        txn.commit().await?;
        Ok(OwnershipVerification {
            outcome,
            period_start: matched.and_then(|p| p.activated_at),
            period_end: matched.and_then(|p| p.ended_at),
            assignment_id: matched.map(|p| p.assignment_id),
        })
    }

    /// The full lifecycle evidence of a number (operators, audited): lifecycle audit,
    /// suspensions, releases, quarantines, late activity counts and risk decisions —
    /// owners masked as in [`Self::ownership_periods`].
    pub async fn lifecycle_evidence(
        &self,
        ctx: &RequestContext,
        actor: &Actor,
        resource_id: Uuid,
        j: &AccessJustification,
    ) -> DomainResult<Value> {
        Self::require_operator(actor, j)?;
        self.numbers.resource(resource_id).await?;
        {
            let txn = self.numbers.db().begin().await?;
            Self::audit_access(
                &txn,
                ctx,
                actor,
                resource_id,
                j,
                "LIFECYCLE_HISTORY",
                "READ",
                self.now(),
            )
            .await?;
            txn.commit().await?;
        }
        let lifecycle = self.numbers.lifecycle_events(resource_id).await?;
        #[derive(FromQueryResult, Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Item {
            kind: String,
            id: Uuid,
            status: String,
            reason: Option<String>,
            at: DateTime<Utc>,
        }
        let items = Item::find_by_statement(stmt(
            "SELECT 'SUSPENSION' AS kind, id, status, reason_code AS reason, suspended_at AS at FROM number_suspensions WHERE number_resource_id = $1 \
             UNION ALL SELECT 'RELEASE', id, status, reason_code, requested_at FROM number_release_requests WHERE number_resource_id = $1 \
             UNION ALL SELECT 'QUARANTINE', id, status, completion_reason, started_at FROM number_quarantines WHERE number_resource_id = $1 \
             UNION ALL SELECT 'RISK_DECISION', id, outcome, decision_kind, decided_at FROM number_risk_decisions WHERE number_resource_id = $1 \
             UNION ALL SELECT 'PORT', id, status, status_reason, requested_at FROM number_port_requests WHERE number_resource_id = $1 \
             ORDER BY at, kind",
            vec![resource_id.into()],
        ))
        .all(self.numbers.db())
        .await?;
        let late = super::count(
            self.numbers.db(),
            "SELECT count(*) AS n FROM number_late_activity WHERE number_resource_id = $1",
            vec![resource_id.into()],
        )
        .await?;
        Ok(json!({
            "numberResourceId": resource_id,
            "lifecycle": lifecycle.iter().map(|e| json!({
                "fromState": e.from_state,
                "toState": e.to_state,
                "revision": e.revision,
                "actorKind": e.actor_kind,
                "reasonCode": e.reason_code,
                "occurredAt": e.occurred_at,
            })).collect::<Vec<_>>(),
            "workflow": items,
            "lateActivityEvents": late,
        }))
    }
}
