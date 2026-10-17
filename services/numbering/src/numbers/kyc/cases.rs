//! KYC cases (Part IV Workstream F §6–§9): lifecycle, protected evidence, verification,
//! manual review, expiry / revalidation, revocation and appeals.
//!
//! Every status change goes through [`transition_case`] inside one transaction that also
//! appends the immutable decision record (`number_kyc_decisions`, one per case revision)
//! and the outbox event; the database refuses changes made any other way. Cases belong to
//! the TRUSTED scope that opened them; other scopes see "not found". Decisions and events
//! carry ids, statuses and reason codes only — never evidence content or identifiers.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    evidence::{check_content, EvidenceBytes, EvidenceVault},
    requirements::{resolve_requirements_in, RequirementItem, RequirementQuery},
    types::{
        kyc_allowed, AppealStatus, DecisionKind, EvidenceKind, KycStatus, RequirementType,
        ReviewDecision, SubjectKind, TenantClassification, VerifierKind,
    },
    verifier::{
        missing_requirements, EvidenceSummary, KycVerifier, VerificationRequest, VerifierOutcome,
    },
};
use crate::{
    domain::{RequestContext, SharedClock},
    infra::{
        idempotency::{self, Begin, IdempotencyScope},
        jobs::{self, ClaimedJob, JobHandler, JobOutcome, NewJob, RetrySafety},
        outbox::{self, EventScope, NewEvent},
    },
    numbers::{
        stmt, types::reason_code, Actor, ActorKind, DomainError, DomainResult, InventoryClass,
        NumberDomain, NumberType, OwnerScope, OwnershipClass,
    },
};

pub const VERIFY_JOB: &str = "kyc.verify";

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KycCaseRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub created_by: Uuid,
    pub subject_kind: String,
    pub subject_ref: Uuid,
    pub country_code: String,
    pub inventory_class: String,
    pub number_type: String,
    pub ownership_class: String,
    pub purpose_code: String,
    #[serde(skip)]
    pub provider_id: Option<String>,
    pub requirement_set_ids: Vec<Uuid>,
    pub requirement_snapshot: Value,
    pub status: String,
    pub revision: i64,
    pub submitted_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    pub verifier_kind: Option<String>,
    pub reviewer_kind: Option<String>,
    pub reviewer_ref: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revalidate_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub correlation_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl KycCaseRow {
    pub fn status(&self) -> KycStatus {
        KycStatus::parse(&self.status).expect("CHECK-constrained status")
    }

    pub fn in_scope(&self, scope: &OwnerScope) -> bool {
        self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
    }

    pub fn items(&self) -> Vec<RequirementItem> {
        serde_json::from_value(self.requirement_snapshot["items"].clone()).unwrap_or_default()
    }

    fn validity_days(&self) -> i64 {
        self.requirement_snapshot["approvalValidityDays"]
            .as_i64()
            .unwrap_or(365)
    }

    fn revalidation_days(&self) -> Option<i64> {
        self.requirement_snapshot["revalidationDays"].as_i64()
    }

    /// Whether the case is an approval in force at `now`.
    pub fn approved_at_time(&self, now: DateTime<Utc>) -> bool {
        self.status() == KycStatus::Approved && self.expires_at.is_some_and(|t| t > now)
    }

    fn event_scope(&self) -> EventScope {
        EventScope {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRow {
    pub id: Uuid,
    pub case_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub evidence_kind: String,
    pub requirement_type: String,
    /// Internal: never returned.
    #[serde(skip)]
    pub object_key: String,
    pub content_type: String,
    pub size_bytes: i64,
    /// Internal integrity value: never returned.
    #[serde(skip)]
    pub sha256: String,
    #[serde(skip)]
    pub sealed_kid: String,
    pub status: String,
    pub submitted_by: Uuid,
    pub submitted_at: DateTime<Utc>,
    pub retention_until: DateTime<Utc>,
    pub purged_at: Option<DateTime<Utc>>,
}

impl EvidenceRow {
    fn summary(&self) -> Option<EvidenceSummary> {
        (self.status == "ACTIVE").then(|| EvidenceSummary {
            evidence_id: self.id,
            kind: EvidenceKind::parse(&self.evidence_kind).expect("CHECK-constrained"),
            requirement_type: RequirementType::parse(&self.requirement_type)
                .expect("CHECK-constrained"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRow {
    pub id: Uuid,
    pub sequence: i64,
    pub case_id: Uuid,
    pub case_revision: i64,
    pub from_status: Option<String>,
    pub to_status: String,
    pub decision_kind: String,
    pub verifier_kind: String,
    pub actor_kind: String,
    pub actor_ref: String,
    pub reason_code: String,
    pub evidence_ids: Vec<Uuid>,
    pub detail: Value,
    pub correlation_id: String,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppealRow {
    pub id: Uuid,
    pub case_id: Uuid,
    pub appealed_decision_id: Uuid,
    pub status: String,
    pub reason_code: String,
    pub submitted_by: Uuid,
    pub submitted_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolution_decision_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, FromQueryResult)]
pub struct VerificationRow {
    pub id: Uuid,
    pub case_id: Uuid,
    pub case_revision: i64,
    pub purpose: String,
    pub verifier_kind: String,
    pub request_ref: Uuid,
    pub status: String,
    pub outcome: Option<String>,
    pub response_ref: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// The KYC standing of an assignment (internal view; no evidence, no subject data).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentKycStatus {
    pub assignment_id: Uuid,
    pub eligibility_decision_id: Option<Uuid>,
    pub kyc_required: bool,
    /// The assignment's KYC basis is in force now (not required, or approved and unexpired).
    pub in_force: bool,
    pub kyc_case_id: Option<Uuid>,
    pub status: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// A case to open.
#[derive(Debug, Clone)]
pub struct NewCase {
    pub subject_kind: SubjectKind,
    /// Opaque subject (principal id or organization ref) — never personal data.
    pub subject_ref: Uuid,
    pub country: String,
    pub number_type: NumberType,
    pub ownership_class: OwnershipClass,
    pub purpose_code: String,
    pub provider_id: Option<String>,
    pub tenant_classification: TenantClassification,
}

/// A manual reviewer's decision.
#[derive(Debug, Clone)]
pub struct ManualReview {
    pub decision: ReviewDecision,
    pub reason_code: String,
    /// The ACTIVE evidence of the case the reviewer examined (required).
    pub evidence_ids: Vec<Uuid>,
    pub expected_revision: i64,
    /// For REQUEST_EVIDENCE: what is missing.
    pub missing: Vec<RequirementType>,
    /// Shorter validity than the policy (never longer).
    pub valid_for_days: Option<i64>,
}

/// Tunables of the KYC service.
#[derive(Debug, Clone, Copy)]
pub struct KycSettings {
    pub evidence_retention: Duration,
    /// Unfinished cases (DRAFT / EVIDENCE_REQUIRED / MORE_EVIDENCE_REQUIRED) expire.
    pub abandonment: Duration,
    pub verification_attempts: i32,
    pub idempotency_ttl: Duration,
}

impl Default for KycSettings {
    fn default() -> Self {
        Self {
            evidence_retention: Duration::days(730),
            abandonment: Duration::days(90),
            verification_attempts: 8,
            idempotency_ttl: Duration::hours(24),
        }
    }
}

/// The KYC service (cases, evidence, verification).
#[derive(Clone)]
pub struct KycService {
    numbers: NumberDomain,
    vault: Arc<EvidenceVault>,
    verifier: Arc<dyn KycVerifier>,
    settings: KycSettings,
}

/// Result of an idempotent operation.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome<T> {
    pub value: T,
    /// The request replayed an earlier one with the same idempotency key.
    pub replayed: bool,
}

struct Change<'a> {
    to: KycStatus,
    kind: DecisionKind,
    verifier: VerifierKind,
    actor: &'a Actor,
    reason: &'a str,
    evidence_ids: Vec<Uuid>,
    detail: Value,
    approval_expiry: Option<DateTime<Utc>>,
    revalidate_at: Option<DateTime<Utc>>,
}

fn not_found() -> DomainError {
    DomainError::NotFound("KYC case")
}

async fn lock_case(txn: &DatabaseTransaction, id: Uuid) -> DomainResult<KycCaseRow> {
    KycCaseRow::find_by_statement(stmt(
        "SELECT * FROM number_kyc_cases WHERE id = $1 FOR UPDATE",
        vec![id.into()],
    ))
    .one(txn)
    .await?
    .ok_or_else(not_found)
}

async fn active_evidence(
    txn: &impl ConnectionTrait,
    case_id: Uuid,
) -> DomainResult<Vec<EvidenceRow>> {
    Ok(EvidenceRow::find_by_statement(stmt(
        "SELECT * FROM number_kyc_evidence_refs WHERE case_id = $1 AND status = 'ACTIVE' ORDER BY submitted_at",
        vec![case_id.into()],
    ))
    .all(txn)
    .await?)
}

/// The only status-changing path of a case (inside `txn`); returns the updated row and
/// the decision record.
async fn transition_case(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    current: &KycCaseRow,
    change: Change<'_>,
    now: DateTime<Utc>,
) -> DomainResult<(KycCaseRow, DecisionRow)> {
    reason_code(change.reason)?;
    let from = current.status();
    if !kyc_allowed(from, change.to) {
        return Err(DomainError::InvalidTransition {
            entity: "KYC case",
            from: from.as_str().into(),
            to: change.to.as_str().into(),
        });
    }
    let decided = matches!(
        change.to,
        KycStatus::Approved
            | KycStatus::Rejected
            | KycStatus::MoreEvidenceRequired
            | KycStatus::Revoked
            | KycStatus::Expired
    );
    let approved = change.to == KycStatus::Approved;
    txn.execute(stmt(
        "SELECT set_config('numbering.kyc', 'transition', true)",
        vec![],
    ))
    .await?;
    let row = KycCaseRow::find_by_statement(stmt(
        "UPDATE number_kyc_cases SET status = $3, revision = revision + 1, updated_at = $4, \
           submitted_at = CASE WHEN $3 = 'SUBMITTED' THEN $4 ELSE submitted_at END, \
           decided_at = CASE WHEN $5 THEN $4 ELSE decided_at END, \
           decision_reason = CASE WHEN $5 THEN $6 ELSE decision_reason END, \
           verifier_kind = COALESCE($7, verifier_kind), \
           reviewer_kind = CASE WHEN $5 THEN $8 ELSE reviewer_kind END, \
           reviewer_ref = CASE WHEN $5 THEN $9 ELSE reviewer_ref END, \
           approved_at = CASE WHEN $10 THEN $4 ELSE approved_at END, \
           expires_at = CASE WHEN $10 THEN $11 ELSE expires_at END, \
           revalidate_at = CASE WHEN $10 THEN $12 ELSE revalidate_at END, \
           revoked_at = CASE WHEN $3 = 'REVOKED' THEN $4 ELSE revoked_at END \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            current.id.into(),
            current.revision.into(),
            change.to.as_str().into(),
            now.into(),
            decided.into(),
            change.reason.into(),
            (change.verifier != VerifierKind::System)
                .then(|| change.verifier.as_str().to_string())
                .into(),
            change.actor.kind.as_str().into(),
            change.actor.reference.clone().into(),
            approved.into(),
            change.approval_expiry.into(),
            change.revalidate_at.into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("KYC case"))?;
    txn.execute(stmt(
        "SELECT set_config('numbering.kyc', 'off', true)",
        vec![],
    ))
    .await?;
    let decision = insert_decision(txn, ctx, &row, Some(from), &change, now).await?;
    crate::metrics::inc(
        "numbering_kyc_transitions_total",
        &[("to", change.to.as_str())],
    );
    Ok((row, decision))
}

async fn insert_decision(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    row: &KycCaseRow,
    from: Option<KycStatus>,
    change: &Change<'_>,
    now: DateTime<Utc>,
) -> DomainResult<DecisionRow> {
    let decision = DecisionRow::find_by_statement(stmt(
        "INSERT INTO number_kyc_decisions (id, case_id, case_revision, from_status, to_status, decision_kind, \
           verifier_kind, actor_kind, actor_ref, reason_code, evidence_ids, detail, correlation_id, decided_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING *",
        vec![
            Uuid::new_v4().into(),
            row.id.into(),
            row.revision.into(),
            from.map(|f| f.as_str().to_string()).into(),
            row.status.clone().into(),
            change.kind.as_str().into(),
            change.verifier.as_str().into(),
            change.actor.kind.as_str().into(),
            change.actor.reference.clone().into(),
            change.reason.into(),
            change.evidence_ids.clone().into(),
            change.detail.clone().into(),
            ctx.correlation_id.as_str().into(),
            now.into(),
        ],
    ))
    .one(txn)
    .await?
    .expect("insert returns the row");
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.kyc-case.changed",
            aggregate_type: "KYC_CASE",
            aggregate_id: row.id,
            scope: Some(row.event_scope()),
            payload: json!({
                "kyc_case_id": row.id,
                "from_status": from.map(|f| f.as_str()),
                "to_status": row.status,
                "revision": row.revision,
                "decision_kind": change.kind.as_str(),
                "verifier_kind": change.verifier.as_str(),
                "reason_code": change.reason,
                "expires_at": row.expires_at,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(decision)
}

fn customer_scope(scope: &OwnerScope) -> IdempotencyScope {
    IdempotencyScope::Customer {
        tenant_id: scope.tenant_id,
        application_id: scope.application_id,
        environment_id: scope.environment_id,
    }
}

/// Starts idempotency for `operation` inside `txn`: `Ok(Some(existing_id))` on replay.
async fn idempotent_begin(
    txn: &DatabaseTransaction,
    scope: &OwnerScope,
    operation: &str,
    key: Option<&str>,
    fingerprint: &str,
    now: DateTime<Utc>,
    ttl: Duration,
) -> DomainResult<(Option<Uuid>, Option<Uuid>)> {
    let Some(key) = key else {
        return Ok((None, None));
    };
    if !idempotency::valid_key(key) {
        return Err(DomainError::Validation(
            "an idempotency key is 8–128 of [A-Za-z0-9._:-]".into(),
        ));
    }
    match idempotency::begin(
        txn,
        &customer_scope(scope),
        operation,
        key,
        fingerprint,
        now,
        ttl,
    )
    .await?
    {
        Begin::New(record) => Ok((None, Some(record))),
        Begin::Replay { resource_id, .. } => Ok((resource_id, None)),
        Begin::Mismatch => Err(DomainError::conflict(
            "IDEMPOTENCY_KEY_REUSED",
            "the idempotency key was used for a different request",
        )),
        Begin::InProgress => Err(DomainError::conflict(
            "IDEMPOTENCY_IN_PROGRESS",
            "the request is still in progress",
        )),
    }
}

impl KycService {
    pub fn new(
        numbers: NumberDomain,
        vault: Arc<EvidenceVault>,
        verifier: Arc<dyn KycVerifier>,
        settings: KycSettings,
    ) -> Self {
        Self {
            numbers,
            vault,
            verifier,
            settings,
        }
    }

    pub fn numbers(&self) -> &NumberDomain {
        &self.numbers
    }

    pub fn verifier_kind(&self) -> VerifierKind {
        self.verifier.kind()
    }

    fn clock(&self) -> &SharedClock {
        self.numbers.clock()
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock().now()
    }

    /// Opens a case (DRAFT) against the requirements in force now; their exact snapshot is
    /// preserved on the case. Unknown requirements fail closed.
    pub async fn create_case(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        new: NewCase,
        idempotency_key: Option<&str>,
    ) -> DomainResult<Outcome<KycCaseRow>> {
        reason_code(&new.purpose_code)?;
        let owner_is_individual = matches!(
            new.ownership_class,
            OwnershipClass::Personal | OwnershipClass::Professional
        );
        if owner_is_individual != (new.subject_kind == SubjectKind::Individual) {
            return Err(DomainError::precondition(
                "OWNERSHIP_SUBJECT_MISMATCH",
                "personal / professional numbers are verified for an individual, organizational ones for an organization",
            ));
        }
        if new.number_type == NumberType::Unclassified {
            return Err(DomainError::Validation(
                "a case is opened for a number type".into(),
            ));
        }
        let class = match scope.environment_kind {
            crate::numbers::EnvironmentKind::Production => InventoryClass::Real,
            _ => InventoryClass::Synthetic,
        };
        let now = self.now();
        let fingerprint = idempotency::fingerprint(&[
            b"create_case",
            new.subject_ref.as_bytes(),
            new.country.as_bytes(),
            new.number_type.as_str().as_bytes(),
            new.ownership_class.as_str().as_bytes(),
            new.purpose_code.as_bytes(),
        ]);
        let txn = self.numbers.db().begin().await?;
        let (replay, record) = idempotent_begin(
            &txn,
            scope,
            "kyc.create_case",
            idempotency_key,
            &fingerprint,
            now,
            self.settings.idempotency_ttl,
        )
        .await?;
        if let Some(id) = replay {
            txn.commit().await?;
            return Ok(Outcome {
                value: self.case(scope, id).await?,
                replayed: true,
            });
        }
        let requirements = resolve_requirements_in(
            &txn,
            &RequirementQuery {
                country: new.country.clone(),
                inventory_class: class,
                number_type: new.number_type,
                ownership_class: new.ownership_class,
                provider_id: new.provider_id.clone(),
                tenant_classification: new.tenant_classification,
            },
            now,
        )
        .await?;
        if !requirements.kyc_mandatory {
            return Err(DomainError::precondition(
                "KYC_NOT_REQUIRED",
                "no KYC is required for this request",
            ));
        }
        let row = KycCaseRow::find_by_statement(stmt(
            "INSERT INTO number_kyc_cases (id, tenant_id, application_id, environment_id, environment_kind, created_by, \
               subject_kind, subject_ref, country_code, inventory_class, number_type, ownership_class, purpose_code, \
               provider_id, requirement_set_ids, requirement_snapshot, status, revision, correlation_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, 'DRAFT', 1, $17, $18, $18) \
             RETURNING *",
            vec![
                Uuid::new_v4().into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                scope.environment_kind.as_str().into(),
                scope.service_account_id.into(),
                new.subject_kind.as_str().into(),
                new.subject_ref.into(),
                new.country.clone().into(),
                class.as_str().into(),
                new.number_type.as_str().into(),
                new.ownership_class.as_str().into(),
                new.purpose_code.clone().into(),
                new.provider_id.clone().into(),
                requirements.set_ids.clone().into(),
                requirements.snapshot.clone().into(),
                ctx.correlation_id.as_str().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        let actor = Actor::new(ActorKind::Customer, scope.service_account_id.to_string());
        insert_decision(
            &txn,
            ctx,
            &row,
            None,
            &Change {
                to: KycStatus::Draft,
                kind: DecisionKind::Created,
                verifier: VerifierKind::System,
                actor: &actor,
                reason: "CASE_OPENED",
                evidence_ids: vec![],
                detail: json!({ "requirementSetIds": requirements.set_ids }),
                approval_expiry: None,
                revalidate_at: None,
            },
            now,
        )
        .await?;
        if let Some(record) = record {
            idempotency::complete(&txn, record, 201, None, Some(row.id), now).await?;
        }
        txn.commit().await?;
        Ok(Outcome {
            value: row,
            replayed: false,
        })
    }

    /// Attaches protected evidence (sealed, stored privately; only a reference is kept).
    /// The first evidence moves a DRAFT case to EVIDENCE_REQUIRED.
    #[allow(clippy::too_many_arguments)]
    pub async fn attach_evidence(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        case_id: Uuid,
        kind: EvidenceKind,
        content_type: &str,
        content: EvidenceBytes,
        idempotency_key: Option<&str>,
    ) -> DomainResult<Outcome<EvidenceRow>> {
        check_content(content_type, content.expose())?;
        if !self.vault.is_configured() {
            return Err(DomainError::precondition(
                "EVIDENCE_STORAGE_UNAVAILABLE",
                "protected evidence storage is not available",
            ));
        }
        let now = self.now();
        let sha = cf_object_storage::sha256_hex(content.expose());
        let fingerprint = idempotency::fingerprint(&[
            b"attach_evidence",
            case_id.as_bytes(),
            kind.as_str().as_bytes(),
            sha.as_bytes(),
        ]);
        let txn = self.numbers.db().begin().await?;
        let case = lock_case(&txn, case_id).await?;
        if !case.in_scope(scope) {
            return Err(not_found());
        }
        let (replay, record) = idempotent_begin(
            &txn,
            scope,
            "kyc.attach_evidence",
            idempotency_key,
            &fingerprint,
            now,
            self.settings.idempotency_ttl,
        )
        .await?;
        if let Some(id) = replay {
            txn.commit().await?;
            let row = EvidenceRow::find_by_statement(stmt(
                "SELECT * FROM number_kyc_evidence_refs WHERE id = $1",
                vec![id.into()],
            ))
            .one(self.numbers.db())
            .await?
            .ok_or(DomainError::NotFound("evidence"))?;
            return Ok(Outcome {
                value: row,
                replayed: true,
            });
        }
        if !matches!(
            case.status(),
            KycStatus::Draft | KycStatus::EvidenceRequired | KycStatus::MoreEvidenceRequired
        ) {
            return Err(DomainError::precondition(
                "CASE_NOT_COLLECTING_EVIDENCE",
                "evidence is accepted while a case collects evidence",
            ));
        }
        let requirement = case
            .items()
            .into_iter()
            .find(|i| i.accepted_evidence.contains(&kind))
            .map(|i| i.requirement_type)
            .ok_or_else(|| {
                DomainError::precondition(
                    "EVIDENCE_NOT_REQUIRED",
                    "no requirement of this case accepts this evidence kind",
                )
            })?;
        let evidence_id = Uuid::new_v4();
        let key = EvidenceVault::object_key(case.id, evidence_id);
        let kid = self.vault.put(&key, evidence_id, content.expose()).await?;
        let inserted = async {
            let row = EvidenceRow::find_by_statement(stmt(
                "INSERT INTO number_kyc_evidence_refs (id, case_id, tenant_id, application_id, environment_id, evidence_kind, \
                   requirement_type, object_key, content_type, size_bytes, sha256, sealed_kid, submitted_by, submitted_at, \
                   retention_until) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING *",
                vec![
                    evidence_id.into(),
                    case.id.into(),
                    case.tenant_id.into(),
                    case.application_id.into(),
                    case.environment_id.into(),
                    kind.as_str().into(),
                    requirement.as_str().into(),
                    key.clone().into(),
                    content_type.into(),
                    (content.len() as i64).into(),
                    sha.clone().into(),
                    kid.into(),
                    scope.service_account_id.into(),
                    now.into(),
                    (now + self.settings.evidence_retention).into(),
                ],
            ))
            .one(&txn)
            .await?
            .expect("insert returns the row");
            if case.status() == KycStatus::Draft {
                let actor = Actor::new(ActorKind::Customer, scope.service_account_id.to_string());
                transition_case(
                    &txn,
                    ctx,
                    &case,
                    Change {
                        to: KycStatus::EvidenceRequired,
                        kind: DecisionKind::EvidenceCollection,
                        verifier: VerifierKind::System,
                        actor: &actor,
                        reason: "EVIDENCE_RECEIVED",
                        evidence_ids: vec![evidence_id],
                        detail: json!({}),
                        approval_expiry: None,
                        revalidate_at: None,
                    },
                    now,
                )
                .await?;
            }
            outbox::enqueue(
                &txn,
                NewEvent {
                    event_type: "number.kyc-evidence.received",
                    aggregate_type: "KYC_CASE",
                    aggregate_id: case.id,
                    scope: Some(case.event_scope()),
                    payload: json!({
                        "kyc_case_id": case.id,
                        "evidence_id": evidence_id,
                        "evidence_kind": kind.as_str(),
                        "requirement_type": requirement.as_str(),
                    }),
                },
                ctx,
                now,
            )
            .await?;
            if let Some(record) = record {
                idempotency::complete(&txn, record, 201, None, Some(evidence_id), now).await?;
            }
            txn.commit().await?;
            Ok::<_, DomainError>(row)
        }
        .await;
        match inserted {
            Ok(row) => Ok(Outcome {
                value: row,
                replayed: false,
            }),
            Err(e) => {
                // Never leave an unreferenced object behind.
                let _ = self.vault.delete(&key).await;
                Err(e)
            }
        }
    }

    /// Submits a case for verification (all mandatory evidence present) and schedules the
    /// verification job atomically.
    pub async fn submit_case(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        case_id: Uuid,
        expected_revision: i64,
        idempotency_key: Option<&str>,
    ) -> DomainResult<Outcome<KycCaseRow>> {
        let now = self.now();
        let fingerprint = idempotency::fingerprint(&[
            b"submit_case",
            case_id.as_bytes(),
            &expected_revision.to_be_bytes(),
        ]);
        let txn = self.numbers.db().begin().await?;
        let case = lock_case(&txn, case_id).await?;
        if !case.in_scope(scope) {
            return Err(not_found());
        }
        let (replay, record) = idempotent_begin(
            &txn,
            scope,
            "kyc.submit_case",
            idempotency_key,
            &fingerprint,
            now,
            self.settings.idempotency_ttl,
        )
        .await?;
        if replay.is_some() {
            txn.commit().await?;
            return Ok(Outcome {
                value: self.case(scope, case_id).await?,
                replayed: true,
            });
        }
        if case.revision != expected_revision {
            return Err(DomainError::VersionConflict("KYC case"));
        }
        if !matches!(
            case.status(),
            KycStatus::EvidenceRequired | KycStatus::MoreEvidenceRequired
        ) {
            return Err(DomainError::InvalidTransition {
                entity: "KYC case",
                from: case.status.clone(),
                to: "SUBMITTED".into(),
            });
        }
        let evidence = active_evidence(&txn, case_id).await?;
        let summaries: Vec<EvidenceSummary> =
            evidence.iter().filter_map(EvidenceRow::summary).collect();
        let missing = missing_requirements(&case.items(), &summaries);
        if !missing.is_empty() {
            return Err(DomainError::precondition(
                "MANDATORY_EVIDENCE_MISSING",
                format!(
                    "mandatory evidence is missing: {}",
                    missing
                        .iter()
                        .map(|m| m.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        let actor = Actor::new(ActorKind::Customer, scope.service_account_id.to_string());
        let (row, _) = transition_case(
            &txn,
            ctx,
            &case,
            Change {
                to: KycStatus::Submitted,
                kind: DecisionKind::Submission,
                verifier: VerifierKind::System,
                actor: &actor,
                reason: "CASE_SUBMITTED",
                evidence_ids: summaries.iter().map(|s| s.evidence_id).collect(),
                detail: json!({}),
                approval_expiry: None,
                revalidate_at: None,
            },
            now,
        )
        .await?;
        self.schedule_verification(&txn, ctx, &row, false, now)
            .await?;
        if let Some(record) = record {
            idempotency::complete(&txn, record, 200, None, Some(case_id), now).await?;
        }
        txn.commit().await?;
        Ok(Outcome {
            value: row,
            replayed: false,
        })
    }

    async fn schedule_verification(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        case: &KycCaseRow,
        revalidation: bool,
        now: DateTime<Utc>,
    ) -> DomainResult<Uuid> {
        let id = Uuid::new_v4();
        txn.execute(stmt(
            "INSERT INTO number_kyc_verifications (id, case_id, case_revision, purpose, verifier_kind, request_ref, status, started_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 'PENDING', $7)",
            vec![
                id.into(),
                case.id.into(),
                case.revision.into(),
                (if revalidation { "REVALIDATION" } else { "SUBMISSION" }).into(),
                self.verifier.kind().as_str().into(),
                Uuid::new_v4().into(),
                now.into(),
            ],
        ))
        .await?;
        jobs::enqueue(
            txn,
            NewJob {
                kind: VERIFY_JOB,
                dedupe_key: Some(id.to_string()),
                payload: json!({ "verification_id": id }),
                retry_safety: RetrySafety::Idempotent,
                max_attempts: self.settings.verification_attempts,
                run_at: now,
            },
            ctx,
            now,
        )
        .await?;
        Ok(id)
    }

    /// Runs one verification attempt (the `kyc.verify` job): REVIEW_STARTED, then the
    /// verifier (or, after an unknown outcome, its `status` — never a second check), then
    /// the outcome. Late or duplicate responses are ignored.
    pub async fn run_verification(
        &self,
        ctx: &RequestContext,
        verification_id: Uuid,
    ) -> DomainResult<JobOutcome> {
        let db = self.numbers.db();
        let Some(v) = VerificationRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_verifications WHERE id = $1",
            vec![verification_id.into()],
        ))
        .one(db)
        .await?
        else {
            return Ok(JobOutcome::Failed {
                code: "UNKNOWN_VERIFICATION",
                message: "no such verification".into(),
            });
        };
        if !matches!(v.status.as_str(), "PENDING" | "UNKNOWN") {
            return Ok(JobOutcome::Succeeded);
        }
        let revalidation = v.purpose == "REVALIDATION";
        // Review starts before the verifier is asked (visible state).
        let now = self.now();
        {
            let txn = db.begin().await?;
            let case = lock_case(&txn, v.case_id).await?;
            if !revalidation && case.status() == KycStatus::Submitted {
                let actor = Actor::system("kyc-verifier");
                transition_case(
                    &txn,
                    ctx,
                    &case,
                    Change {
                        to: KycStatus::UnderReview,
                        kind: DecisionKind::ReviewStarted,
                        verifier: self.verifier.kind(),
                        actor: &actor,
                        reason: "REVIEW_STARTED",
                        evidence_ids: vec![],
                        detail: json!({}),
                        approval_expiry: None,
                        revalidate_at: None,
                    },
                    now,
                )
                .await?;
            }
            txn.commit().await?;
        }
        let case = KycCaseRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_cases WHERE id = $1",
            vec![v.case_id.into()],
        ))
        .one(db)
        .await?
        .ok_or_else(not_found)?;
        let evidence = active_evidence(db, case.id).await?;
        let request = VerificationRequest {
            request_ref: v.request_ref,
            case_id: case.id,
            revalidation,
            subject_kind: SubjectKind::parse(&case.subject_kind).expect("CHECK-constrained"),
            subject_ref: case.subject_ref,
            country: case.country_code.clone(),
            items: case.items(),
            evidence: evidence.iter().filter_map(EvidenceRow::summary).collect(),
        };
        let outcome = if v.status == "UNKNOWN" {
            self.verifier.status(v.request_ref).await
        } else {
            self.verifier.verify(&request).await
        };
        self.apply_outcome(ctx, &v, outcome).await
    }

    async fn apply_outcome(
        &self,
        ctx: &RequestContext,
        v: &VerificationRow,
        outcome: VerifierOutcome,
    ) -> DomainResult<JobOutcome> {
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let case = lock_case(&txn, v.case_id).await?;
        let revalidation = v.purpose == "REVALIDATION";
        let expected = if revalidation {
            KycStatus::Approved
        } else {
            KycStatus::UnderReview
        };
        let close = |status: &'static str,
                     outcome_code: Option<&'static str>,
                     response: Option<String>| {
            stmt(
                "UPDATE number_kyc_verifications SET status = $2, outcome = $3, response_ref = $4, \
                   completed_at = CASE WHEN $2 = 'COMPLETED' THEN $5::timestamptz ELSE completed_at END \
                 WHERE id = $1 AND status IN ('PENDING', 'UNKNOWN')",
                vec![v.id.into(), status.into(), outcome_code.into(), response.into(), now.into()],
            )
        };
        if case.status() != expected {
            // The case moved on (manual decision, revocation, expiry): a late response.
            txn.execute(close("SUPERSEDED", None, None)).await?;
            txn.commit().await?;
            return Ok(JobOutcome::Succeeded);
        }
        let actor = Actor::system("kyc-verifier");
        let verifier = self.verifier.kind();
        let kind = if revalidation {
            DecisionKind::Revalidation
        } else {
            DecisionKind::Verification
        };
        let rejection_reason: String;
        let (status_code, change, response) = match outcome {
            VerifierOutcome::Unknown { .. } => {
                txn.execute(close("UNKNOWN", None, None)).await?;
                txn.commit().await?;
                return Ok(JobOutcome::Retry {
                    code: "VERIFIER_OUTCOME_UNKNOWN",
                    message: "the verifier outcome is unknown; its status will be polled".into(),
                });
            }
            VerifierOutcome::PendingReview => {
                txn.execute(close("COMPLETED", Some("PENDING_REVIEW"), None))
                    .await?;
                if revalidation {
                    // A manual revalidation asks the subject for current evidence.
                    let detail = json!({ "missingRequirements": case.items().iter().filter(|i| i.mandatory).map(|i| i.requirement_type).collect::<Vec<_>>() });
                    transition_case(
                        &txn,
                        ctx,
                        &case,
                        Change {
                            to: KycStatus::MoreEvidenceRequired,
                            kind,
                            verifier,
                            actor: &actor,
                            reason: "REVALIDATION_EVIDENCE_REQUIRED",
                            evidence_ids: vec![],
                            detail,
                            approval_expiry: None,
                            revalidate_at: None,
                        },
                        now,
                    )
                    .await?;
                }
                txn.commit().await?;
                return Ok(JobOutcome::Succeeded);
            }
            VerifierOutcome::Approved {
                response_ref,
                valid_for_days,
            } => {
                let policy = case.validity_days();
                let days = valid_for_days.map(|d| d.clamp(1, policy)).unwrap_or(policy);
                (
                    "APPROVED",
                    Change {
                        to: KycStatus::Approved,
                        kind,
                        verifier,
                        actor: &actor,
                        reason: if revalidation {
                            "REVALIDATED"
                        } else {
                            "VERIFIED"
                        },
                        evidence_ids: vec![],
                        detail: json!({ "responseRef": response_ref }),
                        approval_expiry: Some(now + Duration::days(days)),
                        revalidate_at: case
                            .revalidation_days()
                            .map(|r| now + Duration::days(r.min(days))),
                    },
                    response_ref,
                )
            }
            VerifierOutcome::Rejected {
                response_ref,
                reason,
            } => {
                // A verifier reason is kept only when it is a well-formed reason code.
                rejection_reason = if reason_code(&reason).is_ok() {
                    reason
                } else {
                    "VERIFICATION_REJECTED".into()
                };
                (
                    "REJECTED",
                    Change {
                        to: if revalidation {
                            KycStatus::Revoked
                        } else {
                            KycStatus::Rejected
                        },
                        kind,
                        verifier,
                        actor: &actor,
                        reason: if revalidation {
                            "REVALIDATION_FAILED"
                        } else {
                            &rejection_reason
                        },
                        evidence_ids: vec![],
                        detail: json!({ "responseRef": response_ref }),
                        approval_expiry: None,
                        revalidate_at: None,
                    },
                    response_ref,
                )
            }
            VerifierOutcome::MoreEvidence {
                response_ref,
                missing,
            }
            | VerifierOutcome::MissingEvidence {
                response_ref,
                missing,
            } => (
                "MORE_EVIDENCE_REQUIRED",
                Change {
                    to: KycStatus::MoreEvidenceRequired,
                    kind,
                    verifier,
                    actor: &actor,
                    reason: "ADDITIONAL_EVIDENCE_REQUIRED",
                    evidence_ids: vec![],
                    detail: json!({ "responseRef": response_ref, "missingRequirements": missing }),
                    approval_expiry: None,
                    revalidate_at: None,
                },
                response_ref,
            ),
        };
        // A response applied once is ignored when delivered again.
        let applied = txn
            .execute(close("COMPLETED", Some(status_code), Some(response)))
            .await;
        match applied {
            Ok(r) if r.rows_affected() == 1 => {}
            Ok(_) => {
                txn.commit().await?;
                return Ok(JobOutcome::Succeeded);
            }
            Err(e) => {
                if crate::numbers::error::constraint(&e).as_deref()
                    == Some("uq_number_kyc_verifications_response")
                {
                    return Ok(JobOutcome::Succeeded);
                }
                return Err(e.into());
            }
        }
        transition_case(&txn, ctx, &case, change, now).await?;
        txn.commit().await?;
        Ok(JobOutcome::Succeeded)
    }

    /// A privileged reviewer's explicit decision on a case UNDER_REVIEW (manual adapter).
    pub async fn review_case(
        &self,
        ctx: &RequestContext,
        reviewer: &Actor,
        case_id: Uuid,
        review: ManualReview,
    ) -> DomainResult<(KycCaseRow, DecisionRow)> {
        if reviewer.kind != ActorKind::Operator {
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "KYC decisions are made by an operator",
            ));
        }
        reason_code(&review.reason_code)?;
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let case = lock_case(&txn, case_id).await?;
        if case.revision != review.expected_revision {
            return Err(DomainError::VersionConflict("KYC case"));
        }
        if case.status() != KycStatus::UnderReview {
            return Err(DomainError::InvalidTransition {
                entity: "KYC case",
                from: case.status.clone(),
                to: review.decision.as_str().into(),
            });
        }
        let evidence = active_evidence(&txn, case_id).await?;
        if review.evidence_ids.is_empty()
            || review
                .evidence_ids
                .iter()
                .any(|id| !evidence.iter().any(|e| e.id == *id))
        {
            return Err(DomainError::precondition(
                "EVIDENCE_NOT_IN_CASE",
                "a decision references the ACTIVE evidence of this case it was based on",
            ));
        }
        let summaries: Vec<EvidenceSummary> =
            evidence.iter().filter_map(EvidenceRow::summary).collect();
        let (to, detail, expiry, revalidate) = match review.decision {
            ReviewDecision::Approve => {
                let missing = missing_requirements(&case.items(), &summaries);
                if !missing.is_empty() {
                    return Err(DomainError::precondition(
                        "MANDATORY_EVIDENCE_MISSING",
                        "a manual approval never bypasses a mandatory requirement",
                    ));
                }
                let policy = case.validity_days();
                let days = review
                    .valid_for_days
                    .map(|d| d.clamp(1, policy))
                    .unwrap_or(policy);
                (
                    KycStatus::Approved,
                    json!({}),
                    Some(now + Duration::days(days)),
                    case.revalidation_days()
                        .map(|r| now + Duration::days(r.min(days))),
                )
            }
            ReviewDecision::Reject => (KycStatus::Rejected, json!({}), None, None),
            ReviewDecision::RequestEvidence => {
                if review.missing.is_empty() {
                    return Err(DomainError::Validation(
                        "an evidence request names what is missing".into(),
                    ));
                }
                (
                    KycStatus::MoreEvidenceRequired,
                    json!({ "missingRequirements": review.missing }),
                    None,
                    None,
                )
            }
        };
        txn.execute(stmt(
            "UPDATE number_kyc_verifications SET status = 'SUPERSEDED' WHERE case_id = $1 AND status IN ('PENDING', 'UNKNOWN')",
            vec![case_id.into()],
        ))
        .await?;
        let (row, decision) = transition_case(
            &txn,
            ctx,
            &case,
            Change {
                to,
                kind: DecisionKind::ManualReview,
                verifier: VerifierKind::Manual,
                actor: reviewer,
                reason: &review.reason_code,
                evidence_ids: review.evidence_ids.clone(),
                detail,
                approval_expiry: expiry,
                revalidate_at: revalidate,
            },
            now,
        )
        .await?;
        // An open appeal is resolved by the final decision.
        if to != KycStatus::MoreEvidenceRequired {
            txn.execute(stmt(
                "UPDATE number_kyc_appeals SET status = $2, resolved_at = $3, resolution_decision_id = $4 \
                 WHERE case_id = $1 AND status = 'OPEN'",
                vec![
                    case_id.into(),
                    (if to == KycStatus::Approved { AppealStatus::Overturned } else { AppealStatus::Upheld })
                        .as_str()
                        .into(),
                    now.into(),
                    decision.id.into(),
                ],
            ))
            .await?;
        }
        txn.commit().await?;
        Ok((row, decision))
    }

    /// Revokes an approval (operator): eligibility changes immediately.
    pub async fn revoke_case(
        &self,
        ctx: &RequestContext,
        operator: &Actor,
        case_id: Uuid,
        reason: &str,
    ) -> DomainResult<KycCaseRow> {
        if operator.kind != ActorKind::Operator {
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "KYC approvals are revoked by an operator",
            ));
        }
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let case = lock_case(&txn, case_id).await?;
        txn.execute(stmt(
            "UPDATE number_kyc_verifications SET status = 'SUPERSEDED' WHERE case_id = $1 AND status IN ('PENDING', 'UNKNOWN')",
            vec![case_id.into()],
        ))
        .await?;
        let (row, _) = transition_case(
            &txn,
            ctx,
            &case,
            Change {
                to: KycStatus::Revoked,
                kind: DecisionKind::Revocation,
                verifier: VerifierKind::Manual,
                actor: operator,
                reason,
                evidence_ids: vec![],
                detail: json!({}),
                approval_expiry: None,
                revalidate_at: None,
            },
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Appeals the latest decision of a REJECTED / REVOKED case: back to UNDER_REVIEW for a
    /// human reviewer, the appeal linked to the decision it contests.
    pub async fn open_appeal(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        case_id: Uuid,
        reason: &str,
        idempotency_key: Option<&str>,
    ) -> DomainResult<Outcome<AppealRow>> {
        reason_code(reason)?;
        let now = self.now();
        let fingerprint =
            idempotency::fingerprint(&[b"open_appeal", case_id.as_bytes(), reason.as_bytes()]);
        let txn = self.numbers.db().begin().await?;
        let case = lock_case(&txn, case_id).await?;
        if !case.in_scope(scope) {
            return Err(not_found());
        }
        let (replay, record) = idempotent_begin(
            &txn,
            scope,
            "kyc.open_appeal",
            idempotency_key,
            &fingerprint,
            now,
            self.settings.idempotency_ttl,
        )
        .await?;
        if let Some(id) = replay {
            txn.commit().await?;
            let appeal = AppealRow::find_by_statement(stmt(
                "SELECT * FROM number_kyc_appeals WHERE id = $1",
                vec![id.into()],
            ))
            .one(self.numbers.db())
            .await?
            .ok_or(DomainError::NotFound("appeal"))?;
            return Ok(Outcome {
                value: appeal,
                replayed: true,
            });
        }
        if !matches!(case.status(), KycStatus::Rejected | KycStatus::Revoked) {
            return Err(DomainError::precondition(
                "NOT_APPEALABLE",
                "only a rejection or revocation can be appealed",
            ));
        }
        let latest = DecisionRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_decisions WHERE case_id = $1 ORDER BY sequence DESC LIMIT 1",
            vec![case_id.into()],
        ))
        .one(&txn)
        .await?
        .expect("a case has decisions");
        let appeal = AppealRow::find_by_statement(stmt(
            "INSERT INTO number_kyc_appeals (id, case_id, appealed_decision_id, status, reason_code, submitted_by, submitted_at) \
             VALUES ($1, $2, $3, 'OPEN', $4, $5, $6) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                case_id.into(),
                latest.id.into(),
                reason.into(),
                scope.service_account_id.into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        let actor = Actor::new(ActorKind::Customer, scope.service_account_id.to_string());
        transition_case(
            &txn,
            ctx,
            &case,
            Change {
                to: KycStatus::UnderReview,
                kind: DecisionKind::AppealOpened,
                verifier: VerifierKind::Manual,
                actor: &actor,
                reason,
                evidence_ids: vec![],
                detail: json!({ "appealId": appeal.id, "appealedDecisionId": latest.id }),
                approval_expiry: None,
                revalidate_at: None,
            },
            now,
        )
        .await?;
        if let Some(record) = record {
            idempotency::complete(&txn, record, 201, None, Some(appeal.id), now).await?;
        }
        txn.commit().await?;
        Ok(Outcome {
            value: appeal,
            replayed: false,
        })
    }

    /// Expires approvals past `expires_at` and abandoned unfinished cases (bounded batch).
    pub async fn expire_due(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<Vec<KycCaseRow>> {
        let now = self.now();
        #[derive(FromQueryResult)]
        struct Id {
            id: Uuid,
        }
        let due = Id::find_by_statement(stmt(
            "SELECT id FROM number_kyc_cases WHERE (status = 'APPROVED' AND expires_at <= $1) \
               OR (status IN ('DRAFT', 'EVIDENCE_REQUIRED', 'MORE_EVIDENCE_REQUIRED') AND updated_at <= $2) \
             ORDER BY updated_at LIMIT $3",
            vec![now.into(), (now - self.settings.abandonment).into(), (limit.clamp(1, 1_000) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let actor = Actor::system("kyc-expiry");
        let mut out = Vec::new();
        for d in due {
            let txn = self.numbers.db().begin().await?;
            let case = lock_case(&txn, d.id).await?;
            let approval_expired =
                case.status() == KycStatus::Approved && case.expires_at.is_some_and(|t| t <= now);
            let abandoned = matches!(
                case.status(),
                KycStatus::Draft | KycStatus::EvidenceRequired | KycStatus::MoreEvidenceRequired
            ) && case.updated_at <= now - self.settings.abandonment;
            if !approval_expired && !abandoned {
                continue;
            }
            let (row, _) = transition_case(
                &txn,
                ctx,
                &case,
                Change {
                    to: KycStatus::Expired,
                    kind: DecisionKind::Expiry,
                    verifier: VerifierKind::System,
                    actor: &actor,
                    reason: if approval_expired {
                        "APPROVAL_EXPIRED"
                    } else {
                        "CASE_ABANDONED"
                    },
                    evidence_ids: vec![],
                    detail: json!({}),
                    approval_expiry: None,
                    revalidate_at: None,
                },
                now,
            )
            .await?;
            txn.commit().await?;
            out.push(row);
        }
        Ok(out)
    }

    /// Schedules revalidation of approvals whose `revalidate_at` is due (bounded batch).
    pub async fn schedule_revalidations(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<usize> {
        let now = self.now();
        #[derive(FromQueryResult)]
        struct Id {
            id: Uuid,
        }
        let due = Id::find_by_statement(stmt(
            "SELECT c.id FROM number_kyc_cases c WHERE c.status = 'APPROVED' AND c.revalidate_at <= $1 \
               AND NOT EXISTS (SELECT 1 FROM number_kyc_verifications v WHERE v.case_id = c.id \
                                AND (v.status IN ('PENDING', 'UNKNOWN') OR (v.purpose = 'REVALIDATION' AND v.case_revision = c.revision))) \
             ORDER BY c.revalidate_at LIMIT $2",
            vec![now.into(), (limit.clamp(1, 1_000) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let mut n = 0;
        for d in due {
            let txn = self.numbers.db().begin().await?;
            let case = lock_case(&txn, d.id).await?;
            if case.status() == KycStatus::Approved {
                self.schedule_verification(&txn, ctx, &case, true, now)
                    .await?;
                n += 1;
            }
            txn.commit().await?;
        }
        Ok(n)
    }

    /// Deletes evidence content past its retention (the reference stays, PURGED).
    pub async fn purge_expired_evidence(&self, limit: u64) -> DomainResult<usize> {
        let now = self.now();
        let due = EvidenceRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_evidence_refs WHERE status = 'ACTIVE' AND retention_until <= $1 \
             ORDER BY retention_until LIMIT $2",
            vec![now.into(), (limit.clamp(1, 1_000) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let mut n = 0;
        for e in due {
            if self.vault.delete(&e.object_key).await.is_err() {
                continue; // retried on the next pass
            }
            self.numbers
                .db()
                .execute(stmt(
                    "UPDATE number_kyc_evidence_refs SET status = 'PURGED', purged_at = $2 WHERE id = $1 AND status = 'ACTIVE'",
                    vec![e.id.into(), now.into()],
                ))
                .await?;
            n += 1;
        }
        Ok(n)
    }

    /// Privileged, audited read of evidence content (operator only).
    pub async fn read_evidence(
        &self,
        ctx: &RequestContext,
        operator: &Actor,
        evidence_id: Uuid,
        purpose: &str,
    ) -> DomainResult<(EvidenceRow, EvidenceBytes)> {
        reason_code(purpose)?;
        let db = self.numbers.db();
        let e = EvidenceRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_evidence_refs WHERE id = $1",
            vec![evidence_id.into()],
        ))
        .one(db)
        .await?
        .ok_or(DomainError::NotFound("evidence"))?;
        let log = |outcome: &'static str| {
            stmt(
                "INSERT INTO number_kyc_evidence_access (id, evidence_id, case_id, accessor_kind, accessor_ref, purpose_code, \
                   outcome, correlation_id, accessed_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                vec![
                    Uuid::new_v4().into(),
                    e.id.into(),
                    e.case_id.into(),
                    (if operator.kind == ActorKind::Operator { "OPERATOR" } else { "SERVICE" }).into(),
                    operator.reference.clone().into(),
                    purpose.into(),
                    outcome.into(),
                    ctx.correlation_id.as_str().into(),
                    self.now().into(),
                ],
            )
        };
        if operator.kind != ActorKind::Operator {
            db.execute(log("DENIED")).await?;
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "evidence is read by an operator only",
            ));
        }
        if e.status != "ACTIVE" {
            db.execute(log("PURGED")).await?;
            return Err(DomainError::precondition(
                "EVIDENCE_PURGED",
                "the evidence content was purged after its retention",
            ));
        }
        let bytes = self
            .vault
            .get(&e.object_key, e.id, &e.sha256)
            .await?
            .ok_or_else(|| {
                DomainError::precondition("EVIDENCE_INTEGRITY", "the evidence object is missing")
            })?;
        db.execute(log("GRANTED")).await?;
        crate::metrics::inc("numbering_kyc_evidence_reads_total", &[]);
        Ok((e, bytes))
    }

    /// A case of the caller's scope.
    pub async fn case(&self, scope: &OwnerScope, id: Uuid) -> DomainResult<KycCaseRow> {
        self.case_any(id)
            .await?
            .filter(|c| c.in_scope(scope))
            .ok_or_else(not_found)
    }

    /// A case by id, any scope (privileged callers only).
    pub async fn case_any(&self, id: Uuid) -> DomainResult<Option<KycCaseRow>> {
        Ok(KycCaseRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_cases WHERE id = $1",
            vec![id.into()],
        ))
        .one(self.numbers.db())
        .await?)
    }

    pub async fn evidence(&self, case_id: Uuid) -> DomainResult<Vec<EvidenceRow>> {
        Ok(EvidenceRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_evidence_refs WHERE case_id = $1 ORDER BY submitted_at",
            vec![case_id.into()],
        ))
        .all(self.numbers.db())
        .await?)
    }

    pub async fn decisions(&self, case_id: Uuid) -> DomainResult<Vec<DecisionRow>> {
        Ok(DecisionRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_decisions WHERE case_id = $1 ORDER BY sequence",
            vec![case_id.into()],
        ))
        .all(self.numbers.db())
        .await?)
    }

    pub async fn appeals(&self, case_id: Uuid) -> DomainResult<Vec<AppealRow>> {
        Ok(AppealRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_appeals WHERE case_id = $1 ORDER BY submitted_at",
            vec![case_id.into()],
        ))
        .all(self.numbers.db())
        .await?)
    }

    pub async fn verifications(&self, case_id: Uuid) -> DomainResult<Vec<VerificationRow>> {
        Ok(VerificationRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_verifications WHERE case_id = $1 ORDER BY started_at",
            vec![case_id.into()],
        ))
        .all(self.numbers.db())
        .await?)
    }

    /// The missing mandatory requirements of a case (from its snapshot and ACTIVE evidence).
    pub async fn missing_requirements(
        &self,
        case: &KycCaseRow,
    ) -> DomainResult<Vec<RequirementType>> {
        let evidence = active_evidence(self.numbers.db(), case.id).await?;
        let summaries: Vec<EvidenceSummary> =
            evidence.iter().filter_map(EvidenceRow::summary).collect();
        Ok(missing_requirements(&case.items(), &summaries))
    }

    /// The KYC standing of an assignment for internal consumers: through the eligibility
    /// decision that admitted it, the KYC case that decision relied on (if any).
    pub async fn assignment_kyc_status(
        &self,
        assignment_id: Uuid,
    ) -> DomainResult<AssignmentKycStatus> {
        #[derive(FromQueryResult)]
        struct Link {
            eligibility_decision_ref: Option<Uuid>,
            kyc_case_id: Option<Uuid>,
        }
        let link = Link::find_by_statement(stmt(
            "SELECT a.eligibility_decision_ref, d.kyc_case_id FROM dcn_assignments a \
               LEFT JOIN number_eligibility_decisions d ON d.id = a.eligibility_decision_ref WHERE a.id = $1",
            vec![assignment_id.into()],
        ))
        .one(self.numbers.db())
        .await?
        .ok_or(DomainError::NotFound("assignment"))?;
        let case = match link.kyc_case_id {
            Some(id) => self.case_any(id).await?,
            None => None,
        };
        let now = self.now();
        Ok(AssignmentKycStatus {
            assignment_id,
            eligibility_decision_id: link.eligibility_decision_ref,
            kyc_required: case.is_some(),
            in_force: match (&link.eligibility_decision_ref, &case) {
                (None, _) => false,
                (Some(_), None) => true,
                (Some(_), Some(c)) => c.approved_at_time(now),
            },
            kyc_case_id: case.as_ref().map(|c| c.id),
            status: case.as_ref().map(|c| c.status.clone()),
            expires_at: case.as_ref().and_then(|c| c.expires_at),
        })
    }

    /// The latest case of a subject for a country / class / type / ownership in a scope.
    pub async fn latest_case<C: ConnectionTrait>(
        db: &C,
        scope: &OwnerScope,
        subject_ref: Uuid,
        country: &str,
        class: InventoryClass,
        number_type: NumberType,
        ownership: OwnershipClass,
    ) -> DomainResult<Option<KycCaseRow>> {
        Ok(KycCaseRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_cases WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND subject_ref = $4 AND country_code = $5 AND inventory_class = $6 AND number_type = $7 \
               AND ownership_class = $8 ORDER BY created_at DESC, id LIMIT 1",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                subject_ref.into(),
                country.into(),
                class.as_str().into(),
                number_type.as_str().into(),
                ownership.as_str().into(),
            ],
        ))
        .one(db)
        .await?)
    }
}

/// The `kyc.verify` job handler.
pub struct VerificationHandler {
    pub kyc: KycService,
}

#[async_trait::async_trait]
impl JobHandler for VerificationHandler {
    fn kind(&self) -> &'static str {
        VERIFY_JOB
    }

    async fn run(&self, job: &ClaimedJob) -> JobOutcome {
        let Some(id) = job
            .payload
            .get("verification_id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return JobOutcome::Failed {
                code: "MALFORMED_JOB",
                message: "no verification id".into(),
            };
        };
        match self.kyc.run_verification(&job.context(), id).await {
            Ok(outcome) => outcome,
            Err(DomainError::Database { summary, .. }) => JobOutcome::Retry {
                code: "DATABASE",
                message: summary,
            },
            Err(e) => JobOutcome::Failed {
                code: "VERIFICATION_REFUSED",
                message: e.code().to_string(),
            },
        }
    }
}
