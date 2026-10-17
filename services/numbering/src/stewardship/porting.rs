//! Workstream U — the provider-independent portability foundation.
//!
//! A port request moves `REQUESTED → ELIGIBILITY_CHECK → (DOCUMENTS_REQUIRED) →
//! SUBMITTED → IN_PROGRESS → COMPLETED`, or ends `REJECTED` / `CANCELLED`; an
//! uncertain submission or status is `UNKNOWN_PROVIDER_STATE` and is resolved only by
//! asking the porting gateway again (never by resubmitting). Every change is appended to
//! `number_port_events`.
//!
//! * The number is kept as a SHA-256 digest (uniqueness, one open request per number);
//!   ownership verification needs protected evidence references (KYC evidence ids) and
//!   an APPROVED, unexpired KYC case of the scope when one is named.
//! * Porting is behind `NUMBERING_PORTING_ENABLED` (default false) and the
//!   `NUMBER_PORTING` kill switch, and needs a gateway that supports the direction and
//!   country: the production default [`UnsupportedPorting`] supports nothing, so real
//!   providers without porting stay disabled. [`DeterministicPorting`] scripts outcomes
//!   for tests.
//! * Port-out never touches routing before the gateway reports authoritative
//!   COMPLETION; only then does the Workstream Q release run (`PORTED_OUT`: routing
//!   removed, bindings ended `PORT_OUT`, quarantine `RELINQUISHED`).
//! * Port-in completion is recorded with its evidence; the canonical Number Resource then
//!   arrives through the gaining provider's inventory and the ordinary lifecycle. No live
//!   portability provider is integrated.

use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use uuid::Uuid;

use super::{count, IdRow, Stewardship};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, EventScope, NewEvent},
    numbers::{
        assignments::AssignmentRow, e164, stmt, Actor, ActorKind, AssignmentStatus, DomainError,
        DomainResult, OwnerScope, OwnershipClass,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PortDirection {
    In,
    Out,
}

impl PortDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            PortDirection::In => "IN",
            PortDirection::Out => "OUT",
        }
    }
}

/// What the porting gateway answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortOutcome {
    /// Accepted for processing.
    Submitted,
    InProgress,
    Completed,
    Rejected {
        code: String,
    },
    /// The gateway does not port this direction / country.
    Unsupported,
    /// The request may or may not have been accepted.
    Unknown,
}

/// The provider-neutral porting boundary (a carrier's porting API sits behind it).
#[async_trait]
pub trait PortingGateway: Send + Sync {
    fn supports(&self, direction: PortDirection, country: &str) -> bool;
    async fn submit(&self, port: &PortRow) -> PortOutcome;
    async fn status(&self, port: &PortRow) -> PortOutcome;
}

/// The production default: no porting (every request is refused UNSUPPORTED).
pub struct UnsupportedPorting;

#[async_trait]
impl PortingGateway for UnsupportedPorting {
    fn supports(&self, _: PortDirection, _: &str) -> bool {
        false
    }
    async fn submit(&self, _: &PortRow) -> PortOutcome {
        PortOutcome::Unsupported
    }
    async fn status(&self, _: &PortRow) -> PortOutcome {
        PortOutcome::Unsupported
    }
}

/// Scripted, deterministic porting outcomes (tests / local certification only).
#[derive(Default)]
pub struct DeterministicPorting {
    pub countries: Vec<String>,
    submits: Mutex<VecDeque<PortOutcome>>,
    statuses: Mutex<VecDeque<PortOutcome>>,
    pub submissions: Mutex<u32>,
}

impl DeterministicPorting {
    pub fn new(countries: &[&str]) -> Self {
        Self {
            countries: countries.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        }
    }

    /// The next submission answers `o` (default: Submitted).
    pub fn script_submit(&self, o: PortOutcome) {
        self.submits.lock().unwrap().push_back(o);
    }

    /// The next status query answers `o` (default: InProgress).
    pub fn script_status(&self, o: PortOutcome) {
        self.statuses.lock().unwrap().push_back(o);
    }
}

#[async_trait]
impl PortingGateway for DeterministicPorting {
    fn supports(&self, _: PortDirection, country: &str) -> bool {
        self.countries.iter().any(|c| c == country)
    }
    async fn submit(&self, _: &PortRow) -> PortOutcome {
        *self.submissions.lock().unwrap() += 1;
        self.submits
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(PortOutcome::Submitted)
    }
    async fn status(&self, _: &PortRow) -> PortOutcome {
        self.statuses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(PortOutcome::InProgress)
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortRow {
    pub id: Uuid,
    pub direction: String,
    pub number_resource_id: Option<Uuid>,
    pub assignment_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub country_code: String,
    #[serde(skip)]
    pub number_digest: String,
    pub ownership_class: String,
    pub kyc_case_ref: Option<Uuid>,
    pub evidence_refs: Vec<Uuid>,
    #[serde(skip)]
    pub provider_id: Option<String>,
    #[serde(skip)]
    pub provider_command_id: Option<Uuid>,
    pub status: String,
    pub revision: i64,
    pub status_reason: Option<String>,
    pub idempotency_key: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl PortRow {
    pub fn in_scope(&self, scope: &OwnerScope) -> bool {
        self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
    }

    fn direction(&self) -> PortDirection {
        if self.direction == "OUT" {
            PortDirection::Out
        } else {
            PortDirection::In
        }
    }

    pub fn is_final(&self) -> bool {
        matches!(self.status.as_str(), "COMPLETED" | "REJECTED" | "CANCELLED")
    }
}

/// The allowed port request edges.
pub fn port_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("REQUESTED", "ELIGIBILITY_CHECK" | "CANCELLED" | "REJECTED")
            | (
                "ELIGIBILITY_CHECK",
                "DOCUMENTS_REQUIRED"
                    | "SUBMITTED"
                    | "REJECTED"
                    | "CANCELLED"
                    | "UNKNOWN_PROVIDER_STATE"
            )
            | (
                "DOCUMENTS_REQUIRED",
                "ELIGIBILITY_CHECK" | "CANCELLED" | "REJECTED"
            )
            | (
                "SUBMITTED",
                "IN_PROGRESS" | "COMPLETED" | "REJECTED" | "UNKNOWN_PROVIDER_STATE"
            )
            | (
                "IN_PROGRESS",
                "COMPLETED" | "REJECTED" | "UNKNOWN_PROVIDER_STATE"
            )
            | (
                "UNKNOWN_PROVIDER_STATE",
                "SUBMITTED" | "IN_PROGRESS" | "COMPLETED" | "REJECTED"
            )
    )
}

/// A port request.
#[derive(Debug, Clone)]
pub struct NewPort {
    pub direction: PortDirection,
    /// Port-in: the number to bring in (never stored in clear in the request).
    pub number: Option<String>,
    pub country: Option<String>,
    /// Port-out: the assignment leaving CF Meet.
    pub assignment_id: Option<Uuid>,
    pub ownership_class: OwnershipClass,
    pub kyc_case_id: Option<Uuid>,
    pub evidence_refs: Vec<Uuid>,
}

#[allow(clippy::too_many_arguments)]
async fn move_port(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    row: &PortRow,
    to: &str,
    reason: &str,
    actor: &Actor,
    resource: Option<Uuid>,
    now: DateTime<Utc>,
) -> DomainResult<PortRow> {
    if !port_allowed(&row.status, to) {
        return Err(DomainError::InvalidTransition {
            entity: "port request",
            from: row.status.clone(),
            to: to.into(),
        });
    }
    let next = PortRow::find_by_statement(stmt(
        "UPDATE number_port_requests SET status = $3, status_reason = $4, revision = revision + 1, \
           number_resource_id = COALESCE(number_resource_id, $5), \
           completed_at = CASE WHEN $3 = 'COMPLETED' THEN $6 ELSE completed_at END, updated_at = $6 \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            row.id.into(),
            row.revision.into(),
            to.into(),
            reason.into(),
            resource.into(),
            now.into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("port request"))?;
    append_event(txn, ctx, &next, Some(&row.status), reason, actor, now).await?;
    Ok(next)
}

async fn append_event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    row: &PortRow,
    from: Option<&str>,
    reason: &str,
    actor: &Actor,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    txn.execute(stmt(
        "INSERT INTO number_port_events (id, port_request_id, from_status, to_status, reason_code, actor_kind, \
           actor_ref, occurred_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            Uuid::new_v4().into(),
            row.id.into(),
            from.map(str::to_string).into(),
            row.status.clone().into(),
            reason.into(),
            actor.kind.as_str().into(),
            actor.reference.clone().into(),
            now.into(),
        ],
    ))
    .await?;
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.port.changed",
            aggregate_type: "PORT_REQUEST",
            aggregate_id: row.id,
            scope: Some(EventScope {
                tenant_id: row.tenant_id,
                application_id: row.application_id,
                environment_id: row.environment_id,
            }),
            payload: serde_json::json!({
                "port_request_id": row.id,
                "direction": row.direction,
                "number_resource_id": row.number_resource_id,
                "assignment_id": row.assignment_id,
                "status": row.status,
                "reason_code": reason,
                "revision": row.revision,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

impl Stewardship {
    fn porting_open(&self) -> DomainResult<()> {
        if !self.settings.porting_enabled {
            return Err(DomainError::precondition(
                "PORTING_DISABLED",
                "number porting is not enabled",
            ));
        }
        Ok(())
    }

    /// Opens a port request and runs its eligibility check.
    pub async fn request_port(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        new: NewPort,
        idempotency_key: &str,
    ) -> DomainResult<(PortRow, bool)> {
        self.porting_open()?;
        super::controls::require_open(self.numbers.db(), super::controls::Control::NumberPorting)
            .await?;
        if !crate::infra::idempotency::valid_key(idempotency_key) {
            return Err(DomainError::Validation(
                "an idempotency key is 8–128 of [A-Za-z0-9._:-]".into(),
            ));
        }
        if new.evidence_refs.len() > 16 {
            return Err(DomainError::Validation(
                "at most 16 evidence references".into(),
            ));
        }
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        if let Some(existing) = PortRow::find_by_statement(stmt(
            "SELECT * FROM number_port_requests WHERE tenant_id = $1 AND application_id = $2 \
               AND environment_id = $3 AND idempotency_key = $4",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                idempotency_key.into(),
            ],
        ))
        .one(&txn)
        .await?
        {
            txn.rollback().await?;
            return Ok((existing, true));
        }
        let (resource, assignment, country, digest) = match new.direction {
            PortDirection::In => {
                let number = new.number.as_deref().unwrap_or_default();
                let country = new.country.clone().unwrap_or_default();
                let n = e164::validate(number, &country)?;
                if count(
                    &txn,
                    "SELECT count(*) AS n FROM number_resources r WHERE r.e164 = $1 \
                       AND EXISTS (SELECT 1 FROM dcn_assignments a WHERE a.number_resource_id = r.id AND a.status <> 'CLOSED')",
                    vec![n.e164.clone().into()],
                )
                .await?
                    > 0
                {
                    return Err(DomainError::conflict(
                        "NUMBER_ALREADY_MANAGED",
                        "the number is already a CF Meet number",
                    ));
                }
                (None, None, n.country_code.clone(), super::digest(&n.e164))
            }
            PortDirection::Out => {
                let a = self
                    .numbers
                    .lock_assignment_in(&txn, scope, new.assignment_id.unwrap_or_default())
                    .await?;
                if !matches!(
                    a.status(),
                    AssignmentStatus::Active | AssignmentStatus::Suspended
                ) {
                    return Err(DomainError::precondition(
                        "ASSIGNMENT_NOT_ACTIVE",
                        "only an active or suspended number is ported out",
                    ));
                }
                let r = self.numbers.resource(a.number_resource_id).await?;
                (
                    Some(r.id),
                    Some(a.id),
                    r.country_code.clone(),
                    super::digest(&r.e164),
                )
            }
        };
        let row = PortRow::find_by_statement(stmt(
            "INSERT INTO number_port_requests (id, direction, number_resource_id, assignment_id, tenant_id, \
               application_id, environment_id, country_code, number_digest, ownership_class, kyc_case_ref, \
               evidence_refs, status, idempotency_key, requested_by, requested_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'REQUESTED', $13, $14, $15, $15) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                new.direction.as_str().into(),
                resource.into(),
                assignment.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                country.into(),
                digest.into(),
                new.ownership_class.as_str().into(),
                new.kyc_case_id.into(),
                new.evidence_refs.clone().into(),
                idempotency_key.into(),
                actor.reference.clone().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        append_event(&txn, ctx, &row, None, "PORT_REQUESTED", &actor, now).await?;
        let row = self
            .check_port_eligibility_in(&txn, ctx, scope, &row, &actor, now)
            .await?;
        txn.commit().await?;
        Ok((row, false))
    }

    /// ELIGIBILITY_CHECK: ownership evidence present, the named KYC case APPROVED and in
    /// force in this scope, and a gateway supporting the direction and country.
    async fn check_port_eligibility_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        scope: &OwnerScope,
        row: &PortRow,
        actor: &Actor,
        now: DateTime<Utc>,
    ) -> DomainResult<PortRow> {
        let row = move_port(
            txn,
            ctx,
            row,
            "ELIGIBILITY_CHECK",
            "ELIGIBILITY_CHECK",
            actor,
            None,
            now,
        )
        .await?;
        if !self.porting.supports(row.direction(), &row.country_code) {
            return move_port(
                txn,
                ctx,
                &row,
                "REJECTED",
                "PROVIDER_UNSUPPORTED",
                actor,
                None,
                now,
            )
            .await;
        }
        let evidence_ok = !row.evidence_refs.is_empty()
            && count(
                txn,
                "SELECT count(*) AS n FROM number_kyc_evidence_refs e JOIN number_kyc_cases c ON c.id = e.case_id \
                  WHERE e.id = ANY($1) AND c.tenant_id = $2 AND c.application_id = $3 AND c.environment_id = $4",
                vec![
                    row.evidence_refs.clone().into(),
                    scope.tenant_id.into(),
                    scope.application_id.into(),
                    scope.environment_id.into(),
                ],
            )
            .await?
                == row.evidence_refs.len() as i64;
        let kyc_ok = match row.kyc_case_ref {
            None => true,
            Some(case) => {
                count(
                    txn,
                    "SELECT count(*) AS n FROM number_kyc_cases WHERE id = $1 AND tenant_id = $2 AND application_id = $3 \
                       AND environment_id = $4 AND status = 'APPROVED' AND expires_at > $5",
                    vec![
                        case.into(),
                        scope.tenant_id.into(),
                        scope.application_id.into(),
                        scope.environment_id.into(),
                        now.into(),
                    ],
                )
                .await?
                    == 1
            }
        };
        if !(evidence_ok && kyc_ok) {
            return move_port(
                txn,
                ctx,
                &row,
                "DOCUMENTS_REQUIRED",
                if evidence_ok {
                    "KYC_NOT_IN_FORCE"
                } else {
                    "OWNERSHIP_EVIDENCE_REQUIRED"
                },
                actor,
                None,
                now,
            )
            .await;
        }
        Ok(row)
    }

    /// Adds ownership evidence to a DOCUMENTS_REQUIRED request and re-checks it.
    pub async fn add_port_evidence(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        id: Uuid,
        evidence_refs: Vec<Uuid>,
        kyc_case_id: Option<Uuid>,
    ) -> DomainResult<PortRow> {
        self.porting_open()?;
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let row = self.lock_port(&txn, Some(scope), id).await?;
        if row.status != "DOCUMENTS_REQUIRED" {
            return Err(DomainError::precondition(
                "PORT_NOT_AWAITING_DOCUMENTS",
                "evidence is added while documents are required",
            ));
        }
        let mut refs = row.evidence_refs.clone();
        refs.extend(evidence_refs);
        refs.sort();
        refs.dedup();
        if refs.len() > 16 {
            return Err(DomainError::Validation(
                "at most 16 evidence references".into(),
            ));
        }
        let updated = PortRow::find_by_statement(stmt(
            "UPDATE number_port_requests SET evidence_refs = $3, kyc_case_ref = COALESCE($4, kyc_case_ref), \
               revision = revision + 1, updated_at = $5 WHERE id = $1 AND revision = $2 RETURNING *",
            vec![row.id.into(), row.revision.into(), refs.into(), kyc_case_id.into(), now.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::VersionConflict("port request"))?;
        let out = self
            .check_port_eligibility_in(&txn, ctx, scope, &updated, &actor, now)
            .await?;
        txn.commit().await?;
        Ok(out)
    }

    async fn lock_port(
        &self,
        txn: &DatabaseTransaction,
        scope: Option<&OwnerScope>,
        id: Uuid,
    ) -> DomainResult<PortRow> {
        PortRow::find_by_statement(stmt(
            "SELECT * FROM number_port_requests WHERE id = $1 FOR UPDATE",
            vec![id.into()],
        ))
        .one(txn)
        .await?
        .filter(|p| scope.is_none_or(|s| p.in_scope(s)))
        .ok_or(DomainError::NotFound("port request"))
    }

    /// A port request of the caller's scope.
    pub async fn port_request(
        &self,
        scope: Option<&OwnerScope>,
        id: Uuid,
    ) -> DomainResult<PortRow> {
        PortRow::find_by_statement(stmt(
            "SELECT * FROM number_port_requests WHERE id = $1",
            vec![id.into()],
        ))
        .one(self.numbers.db())
        .await?
        .filter(|p| scope.is_none_or(|s| p.in_scope(s)))
        .ok_or(DomainError::NotFound("port request"))
    }

    /// Submits an eligible request to the gateway (once: a retry of an uncertain
    /// submission asks the gateway's status instead).
    pub async fn submit_port(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        id: Uuid,
    ) -> DomainResult<PortRow> {
        self.porting_open()?;
        super::controls::require_open(self.numbers.db(), super::controls::Control::NumberPorting)
            .await?;
        let row = self.port_request(Some(scope), id).await?;
        if row.status != "ELIGIBILITY_CHECK" {
            return Err(DomainError::precondition(
                "PORT_NOT_ELIGIBLE",
                "only an eligibility-checked request is submitted",
            ));
        }
        let outcome = self.porting.submit(&row).await;
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let row = self.lock_port(&txn, Some(scope), id).await?;
        let out = match outcome {
            PortOutcome::Submitted => {
                move_port(
                    &txn,
                    ctx,
                    &row,
                    "SUBMITTED",
                    "PORT_SUBMITTED",
                    &actor,
                    None,
                    now,
                )
                .await?
            }
            PortOutcome::InProgress => {
                let s = move_port(
                    &txn,
                    ctx,
                    &row,
                    "SUBMITTED",
                    "PORT_SUBMITTED",
                    &actor,
                    None,
                    now,
                )
                .await?;
                move_port(
                    &txn,
                    ctx,
                    &s,
                    "IN_PROGRESS",
                    "PORT_IN_PROGRESS",
                    &actor,
                    None,
                    now,
                )
                .await?
            }
            PortOutcome::Rejected { .. } => {
                move_port(
                    &txn,
                    ctx,
                    &row,
                    "REJECTED",
                    "PROVIDER_REJECTED",
                    &actor,
                    None,
                    now,
                )
                .await?
            }
            PortOutcome::Unsupported => {
                move_port(
                    &txn,
                    ctx,
                    &row,
                    "REJECTED",
                    "PROVIDER_UNSUPPORTED",
                    &actor,
                    None,
                    now,
                )
                .await?
            }
            PortOutcome::Unknown => {
                move_port(
                    &txn,
                    ctx,
                    &row,
                    "UNKNOWN_PROVIDER_STATE",
                    "PORT_OUTCOME_UNKNOWN",
                    &actor,
                    None,
                    now,
                )
                .await?
            }
            PortOutcome::Completed => {
                let s = move_port(
                    &txn,
                    ctx,
                    &row,
                    "SUBMITTED",
                    "PORT_SUBMITTED",
                    &actor,
                    None,
                    now,
                )
                .await?;
                self.complete_port_in(&txn, ctx, &s, &actor, now).await?
            }
        };
        txn.commit().await?;
        Ok(out)
    }

    /// Cancels a request that was not submitted yet.
    pub async fn cancel_port(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        id: Uuid,
    ) -> DomainResult<PortRow> {
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let row = self.lock_port(&txn, Some(scope), id).await?;
        if !matches!(
            row.status.as_str(),
            "REQUESTED" | "ELIGIBILITY_CHECK" | "DOCUMENTS_REQUIRED"
        ) {
            return Err(DomainError::precondition(
                "PORT_SUBMITTED",
                "a submitted port is resolved by the gateway, not cancelled",
            ));
        }
        let out = move_port(
            &txn,
            ctx,
            &row,
            "CANCELLED",
            "PORT_CANCELLED",
            &actor,
            None,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(out)
    }

    /// Authoritative completion. Port-in: the canonical resource is discovered. Port-out:
    /// only now does the release run (routing removed, bindings ended, quarantined as
    /// relinquished).
    async fn complete_port_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        row: &PortRow,
        actor: &Actor,
        now: DateTime<Utc>,
    ) -> DomainResult<PortRow> {
        match row.direction() {
            PortDirection::In => {
                move_port(
                    txn,
                    ctx,
                    row,
                    "COMPLETED",
                    "PORT_COMPLETED",
                    actor,
                    None,
                    now,
                )
                .await
            }
            PortDirection::Out => {
                let done = move_port(
                    txn,
                    ctx,
                    row,
                    "COMPLETED",
                    "PORT_COMPLETED",
                    actor,
                    None,
                    now,
                )
                .await?;
                Ok(done)
            }
        }
    }

    /// After a port-out COMPLETED commit: the Workstream Q release with `PORTED_OUT`.
    async fn finish_port_out(&self, ctx: &RequestContext, row: &PortRow) -> DomainResult<()> {
        let (Some(assignment), Some(resource)) = (row.assignment_id, row.number_resource_id) else {
            return Ok(());
        };
        let actor = Actor::new(ActorKind::System, "numbering.porting");
        let (release, _) = self
            .request_release(
                ctx,
                None,
                actor.clone(),
                super::release::NewRelease {
                    assignment_id: assignment,
                    reason_code: "PORTED_OUT".into(),
                    policy: super::release::ReleasePolicy::PortedOut,
                    effective_at: None,
                },
                &format!("port-out:{}", row.id.simple()),
            )
            .await?;
        if release.status != "ROUTING_REMOVED" {
            return Ok(());
        }
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        for b in self
            .numbers
            .bindings(resource)
            .await?
            .into_iter()
            .filter(|b| b.status == "ACTIVE")
        {
            self.numbers.end_binding_in(&txn, ctx, b.id, now).await?;
        }
        let r = super::release::lock_release(&txn, release.id).await?;
        let evidence =
            serde_json::json!({ "portRequestId": row.id, "portCompletedAt": row.completed_at });
        self.enter_quarantine_in(&txn, ctx, &r, &actor, "RELINQUISHED", Some(evidence), now)
            .await?;
        txn.commit().await?;
        Ok(())
    }

    /// Worker pass: asks the gateway about every submitted / in-progress / uncertain
    /// request (never resubmits) and finishes completed port-outs.
    pub async fn advance_ports(&self, ctx: &RequestContext, limit: u64) -> DomainResult<usize> {
        let ids = IdRow::find_by_statement(stmt(
            "SELECT id FROM number_port_requests WHERE status IN ('SUBMITTED', 'IN_PROGRESS', 'UNKNOWN_PROVIDER_STATE') \
             ORDER BY updated_at LIMIT $1",
            vec![(limit.clamp(1, 500) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let mut n = 0;
        let actor = Actor::reconciliation("numbering.porting");
        for r in ids {
            let row = self.port_request(None, r.id).await?;
            let outcome = self.porting.status(&row).await;
            let now = self.now();
            let txn = self.numbers.db().begin().await?;
            let row = self.lock_port(&txn, None, r.id).await?;
            let next = match (row.status.as_str(), outcome) {
                (_, PortOutcome::Completed) => {
                    Some(self.complete_port_in(&txn, ctx, &row, &actor, now).await?)
                }
                (_, PortOutcome::Rejected { .. }) => Some(
                    move_port(
                        &txn,
                        ctx,
                        &row,
                        "REJECTED",
                        "PROVIDER_REJECTED",
                        &actor,
                        None,
                        now,
                    )
                    .await?,
                ),
                ("SUBMITTED" | "UNKNOWN_PROVIDER_STATE", PortOutcome::InProgress) => Some(
                    move_port(
                        &txn,
                        ctx,
                        &row,
                        "IN_PROGRESS",
                        "PORT_IN_PROGRESS",
                        &actor,
                        None,
                        now,
                    )
                    .await?,
                ),
                ("UNKNOWN_PROVIDER_STATE", PortOutcome::Submitted) => Some(
                    move_port(
                        &txn,
                        ctx,
                        &row,
                        "SUBMITTED",
                        "RECONCILED_SUBMITTED",
                        &actor,
                        None,
                        now,
                    )
                    .await?,
                ),
                ("SUBMITTED" | "IN_PROGRESS", PortOutcome::Unknown) => Some(
                    move_port(
                        &txn,
                        ctx,
                        &row,
                        "UNKNOWN_PROVIDER_STATE",
                        "PORT_OUTCOME_UNKNOWN",
                        &actor,
                        None,
                        now,
                    )
                    .await?,
                ),
                _ => None,
            };
            txn.commit().await?;
            if let Some(next) = next {
                n += 1;
                if next.status == "COMPLETED" && next.direction == "OUT" {
                    self.finish_port_out(ctx, &next).await?;
                }
            }
        }
        // Completed port-outs whose release did not finish (crash between steps).
        let pending = IdRow::find_by_statement(stmt(
            "SELECT p.id FROM number_port_requests p WHERE p.direction = 'OUT' AND p.status = 'COMPLETED' \
               AND p.completed_at > $1 AND NOT EXISTS (SELECT 1 FROM number_release_requests q \
                 WHERE q.assignment_id = p.assignment_id AND q.policy = 'PORTED_OUT' AND q.status = 'QUARANTINED') \
             LIMIT 50",
            vec![(self.now() - Duration::days(30)).into()],
        ))
        .all(self.numbers.db())
        .await?;
        for p in pending {
            let row = self.port_request(None, p.id).await?;
            self.finish_port_out(ctx, &row).await?;
        }
        Ok(n)
    }

    /// The port requests of a scope (newest first).
    pub async fn port_requests(
        &self,
        scope: &OwnerScope,
        limit: u64,
    ) -> DomainResult<Vec<PortRow>> {
        Ok(PortRow::find_by_statement(stmt(
            "SELECT * FROM number_port_requests WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
             ORDER BY requested_at DESC, id LIMIT $4",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                (limit.clamp(1, 200) as i64).into(),
            ],
        ))
        .all(self.numbers.db())
        .await?)
    }

    /// Whether an assignment has a port-out in flight.
    pub async fn port_out_open(&self, assignment: &AssignmentRow) -> DomainResult<bool> {
        Ok(count(
            self.numbers.db(),
            "SELECT count(*) AS n FROM number_port_requests WHERE assignment_id = $1 AND direction = 'OUT' \
               AND status NOT IN ('COMPLETED', 'REJECTED', 'CANCELLED')",
            vec![assignment.id.into()],
        )
        .await?
            > 0)
    }
}
