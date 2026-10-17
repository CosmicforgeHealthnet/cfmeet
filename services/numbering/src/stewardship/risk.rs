//! Workstream T — number-level abuse, fraud and spend controls.
//!
//! Extends (never duplicates) the platform's existing controls: Calling / Telephony keep
//! destination policy and call spend, the SMS Gateway its rate limits and destination
//! checks. Numbering adds what only the number authority can decide:
//!
//! * **allocation risk** at reservation time — active-number and per-class limits,
//!   daily / weekly allocation velocity, restricted countries, multi-country review, mass
//!   registration / number farming — with an explainable, recorded decision;
//! * **signals** (SMS pumping, abnormal SMS, spam / nuisance calls, rapid release,
//!   premium destinations, cost spikes) reported by the traffic carriers, de-duplicated,
//!   scored per number in a window; a restricting score SUSPENDS the number through the
//!   one Workstream P lifecycle (source RISK, lifted by an operator only);
//! * **spend** — a per-number provider-cost threshold refuses new outbound grants;
//!   a provider cost-spike circuit breaker refuses new provisioning;
//! * **appeals** — appended decisions; the original evidence is never modified.
//!
//! App-to-app calling never consults any of this: only DCN traffic is restricted.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use uuid::Uuid;

use super::{count, release::ReleaseRow, Stewardship};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, EventScope, NewEvent},
    numbers::{
        assignments::AssignmentRow, stmt, Actor, ActorKind, AssignmentStatus, DomainError,
        DomainResult, OwnerScope, OwnershipClass,
    },
    providers::incidents,
};

macro_rules! text_kind {
    ($name:ident { $($variant:ident = $s:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
        pub enum $name { $( #[serde(rename = $s)] $variant, )+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            pub fn as_str(&self) -> &'static str { match self { $($name::$variant => $s),+ } }
            pub fn parse(v: &str) -> Option<Self> { match v { $($s => Some($name::$variant),)+ _ => None } }
        }
    };
}

pub const POLICY_VERSION: &str = "NRP-2026.10";

/// The number risk policy (configurable per deployment).
#[derive(Debug, Clone)]
pub struct RiskPolicy {
    /// Open assignments + current reservations per application environment.
    pub max_active_numbers: i64,
    /// Per ownership class (default for unlisted classes: `max_active_numbers`).
    pub max_per_class: BTreeMap<String, i64>,
    pub daily_allocations: i64,
    pub weekly_allocations: i64,
    /// ISO countries where allocation is refused.
    pub restricted_countries: BTreeSet<String>,
    /// Distinct countries above which a new allocation needs review.
    pub multi_country_review: i64,
    /// Score at which a number is suspended; at which a decision needs review.
    pub restrict_score: i32,
    pub review_score: i32,
    pub signal_window: Duration,
    /// Provider cost (minor units) per number in 24h above which outbound grants stop.
    pub number_spend_threshold_minor: i64,
    /// Provider cost (minor units) per provider in one hour that opens the breaker.
    pub provider_cost_spike_minor: i64,
    /// Releases per scope in 7 days that count as rapid release / reassignment.
    pub rapid_release_threshold: i64,
}

impl Default for RiskPolicy {
    fn default() -> Self {
        Self {
            max_active_numbers: 100,
            max_per_class: BTreeMap::new(),
            daily_allocations: 50,
            weekly_allocations: 200,
            restricted_countries: BTreeSet::new(),
            multi_country_review: 10,
            restrict_score: 70,
            review_score: 40,
            signal_window: Duration::hours(24),
            number_spend_threshold_minor: 50_000,
            provider_cost_spike_minor: 1_000_000,
            rapid_release_threshold: 10,
        }
    }
}

text_kind!(SignalKind {
    SmsPumping = "SMS_PUMPING",
    AbnormalSmsTraffic = "ABNORMAL_SMS_TRAFFIC",
    SpamCalls = "SPAM_CALLS",
    NuisanceCalls = "NUISANCE_CALLS",
    RapidRelease = "RAPID_RELEASE",
    MassRegistration = "MASS_REGISTRATION",
    NumberFarming = "NUMBER_FARMING",
    CostSpike = "COST_SPIKE",
    PremiumDestination = "PREMIUM_DESTINATION",
});

impl SignalKind {
    /// Weight (percent) of the signal's magnitude in the number's score.
    fn weight(&self) -> i64 {
        match self {
            SignalKind::SmsPumping | SignalKind::CostSpike => 100,
            SignalKind::PremiumDestination => 90,
            SignalKind::SpamCalls | SignalKind::AbnormalSmsTraffic => 70,
            SignalKind::NuisanceCalls => 50,
            _ => 40,
        }
    }

    /// The decision reason a signal contributes.
    fn reason(&self) -> &'static str {
        self.as_str()
    }

    /// Which carriers may report it.
    pub fn reportable_by(&self, service: &str) -> bool {
        match self {
            SignalKind::SmsPumping | SignalKind::AbnormalSmsTraffic => {
                service == "cfmeet.sms-gateway"
            }
            SignalKind::SpamCalls | SignalKind::NuisanceCalls | SignalKind::PremiumDestination => {
                matches!(service, "cfmeet.telephony" | "cfmeet.calling")
            }
            SignalKind::CostSpike => {
                matches!(service, "cfmeet.telephony" | "cfmeet.sms-gateway")
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRow {
    pub id: Uuid,
    pub sequence: i64,
    pub decision_kind: String,
    pub number_resource_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub outcome: String,
    pub score: i32,
    pub reasons: Vec<String>,
    pub policy_version: String,
    pub supersedes_id: Option<Uuid>,
    pub suspension_id: Option<Uuid>,
    pub actor_kind: String,
    pub actor_ref: String,
    pub statement_ref: Option<Uuid>,
    pub decided_at: DateTime<Utc>,
    pub correlation_id: String,
}

impl DecisionRow {
    pub fn in_scope(&self, scope: &OwnerScope) -> bool {
        self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
    }
}

/// An allocation request evaluated before a reservation.
#[derive(Debug, Clone)]
pub struct AllocationRequest {
    pub country: String,
    pub ownership_class: Option<OwnershipClass>,
}

#[allow(clippy::too_many_arguments)]
async fn insert_decision<C: ConnectionTrait>(
    db: &C,
    ctx: &RequestContext,
    kind: &'static str,
    number: Option<Uuid>,
    scope: (Uuid, Uuid, Uuid),
    outcome: &str,
    score: i32,
    reasons: &[&str],
    supersedes: Option<Uuid>,
    suspension: Option<Uuid>,
    actor: &Actor,
    statement: Option<Uuid>,
    now: DateTime<Utc>,
) -> DomainResult<DecisionRow> {
    let mut reasons: Vec<String> = reasons.iter().map(|r| r.to_string()).collect();
    reasons.sort();
    reasons.dedup();
    let row = DecisionRow::find_by_statement(stmt(
        "INSERT INTO number_risk_decisions (id, decision_kind, number_resource_id, tenant_id, application_id, \
           environment_id, outcome, score, reasons, policy_version, supersedes_id, suspension_id, actor_kind, \
           actor_ref, statement_ref, decided_at, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) RETURNING *",
        vec![
            Uuid::new_v4().into(),
            kind.into(),
            number.into(),
            scope.0.into(),
            scope.1.into(),
            scope.2.into(),
            outcome.into(),
            score.into(),
            reasons.into(),
            POLICY_VERSION.into(),
            supersedes.into(),
            suspension.into(),
            actor.kind.as_str().into(),
            actor.reference.clone().into(),
            statement.into(),
            now.into(),
            ctx.correlation_id.as_str().into(),
        ],
    ))
    .one(db)
    .await?
    .expect("insert returns the row");
    outbox::enqueue(
        db,
        NewEvent {
            event_type: "number.risk.decided",
            aggregate_type: "RISK_DECISION",
            aggregate_id: row.id,
            scope: Some(EventScope {
                tenant_id: row.tenant_id,
                application_id: row.application_id,
                environment_id: row.environment_id,
            }),
            payload: serde_json::json!({
                "risk_decision_id": row.id,
                "decision_kind": row.decision_kind,
                "number_resource_id": row.number_resource_id,
                "outcome": row.outcome,
                "score": row.score,
                "reasons": row.reasons,
                "policy_version": row.policy_version,
                "supersedes_id": row.supersedes_id,
            }),
        },
        ctx,
        now,
    )
    .await?;
    crate::metrics::inc("numbering_risk_decisions_total", &[("kind", kind)]);
    Ok(row)
}

/// The latest number-level decision that sets or lifts a restriction.
async fn latest_restriction<C: ConnectionTrait>(
    db: &C,
    resource_id: Uuid,
) -> DomainResult<Option<DecisionRow>> {
    Ok(DecisionRow::find_by_statement(stmt(
        "SELECT * FROM number_risk_decisions WHERE number_resource_id = $1 \
           AND decision_kind IN ('SCORE', 'APPEAL_FILED', 'APPEAL_UPHELD', 'APPEAL_REJECTED', 'RESTRICTION_LIFTED') \
           AND outcome IN ('RESTRICT', 'PENDING', 'ALLOW') \
         ORDER BY sequence DESC LIMIT 1",
        vec![resource_id.into()],
    ))
    .one(db)
    .await?)
}

/// Whether a risk restriction is in force on a number (restoration is refused).
pub(crate) async fn restriction_in_force<C: ConnectionTrait>(
    db: &C,
    resource_id: Uuid,
) -> DomainResult<bool> {
    Ok(latest_restriction(db, resource_id)
        .await?
        .is_some_and(|d| matches!(d.outcome.as_str(), "RESTRICT" | "PENDING")))
}

/// Rapid release / reassignment: many releases of one scope in a week is a signal.
pub(crate) async fn note_release_in(
    txn: &DatabaseTransaction,
    policy: &RiskPolicy,
    row: &ReleaseRow,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    let recent = count(
        txn,
        "SELECT count(*) AS n FROM number_release_requests WHERE tenant_id = $1 AND application_id = $2 \
           AND environment_id = $3 AND requested_at > $4",
        vec![
            row.tenant_id.into(),
            row.application_id.into(),
            row.environment_id.into(),
            (now - Duration::days(7)).into(),
        ],
    )
    .await?;
    if recent >= policy.rapid_release_threshold {
        txn.execute(stmt(
            "INSERT INTO number_risk_signals (id, number_resource_id, tenant_id, application_id, environment_id, \
               signal_kind, magnitude, source, source_key, observed_at) \
             VALUES ($1, $2, $3, $4, $5, 'RAPID_RELEASE', 40, 'numbering', $6, $7) ON CONFLICT (source_key) DO NOTHING",
            vec![
                Uuid::new_v4().into(),
                row.number_resource_id.into(),
                row.tenant_id.into(),
                row.application_id.into(),
                row.environment_id.into(),
                format!("rapid-release:{}", row.id).into(),
                now.into(),
            ],
        ))
        .await?;
    }
    Ok(())
}

/// A signal reported by a traffic carrier.
#[derive(Debug, Clone)]
pub struct NewSignal {
    pub number_resource_id: Uuid,
    pub kind: SignalKind,
    pub magnitude: i32,
    /// The reporter's de-duplication key (a retry never counts twice).
    pub source_key: String,
}

/// The outcome of scoring after a signal.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalOutcome {
    pub recorded: bool,
    pub score: i32,
    pub outcome: &'static str,
    pub reasons: Vec<String>,
    pub decision_id: Option<Uuid>,
    pub suspension_id: Option<Uuid>,
}

impl Stewardship {
    /// The allocation decision for a new reservation in `scope` (recorded; REVIEW / DENY
    /// refuse the reservation with an explainable rule code).
    pub async fn evaluate_allocation(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        req: &AllocationRequest,
    ) -> DomainResult<DecisionRow> {
        evaluate_allocation_in(
            self.numbers.db(),
            ctx,
            &self.settings.risk,
            scope,
            req,
            self.now(),
        )
        .await
    }

    /// Records a signal (idempotent per source key), scores the number and — above the
    /// restricting score — suspends it through the Workstream P lifecycle.
    pub async fn report_signal(
        &self,
        ctx: &RequestContext,
        reporter: &str,
        signal: NewSignal,
    ) -> DomainResult<SignalOutcome> {
        if !signal.kind.reportable_by(reporter) {
            return Err(DomainError::precondition(
                "SIGNAL_NOT_PERMITTED",
                "this service does not report this signal",
            ));
        }
        if !(0..=100).contains(&signal.magnitude)
            || signal.source_key.is_empty()
            || signal.source_key.len() > 160
            || !signal
                .source_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(DomainError::Validation(
                "a signal has a 0–100 magnitude and a 1–160 character source key".into(),
            ));
        }
        let policy = &self.settings.risk;
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let assignment = AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE number_resource_id = $1 AND status <> 'CLOSED' FOR UPDATE",
            vec![signal.number_resource_id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("number"))?;
        let source = reporter.trim_start_matches("cfmeet.").to_string();
        let inserted = txn
            .execute(stmt(
                "INSERT INTO number_risk_signals (id, number_resource_id, tenant_id, application_id, environment_id, \
                   signal_kind, magnitude, source, source_key, observed_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (source_key) DO NOTHING",
                vec![
                    Uuid::new_v4().into(),
                    signal.number_resource_id.into(),
                    assignment.tenant_id.into(),
                    assignment.application_id.into(),
                    assignment.environment_id.into(),
                    signal.kind.as_str().into(),
                    signal.magnitude.into(),
                    source.into(),
                    format!("{reporter}:{}", signal.source_key).into(),
                    now.into(),
                ],
            ))
            .await?
            .rows_affected()
            == 1;
        #[derive(FromQueryResult)]
        struct S {
            signal_kind: String,
            magnitude: i64,
        }
        let window = S::find_by_statement(stmt(
            "SELECT signal_kind, max(magnitude)::bigint AS magnitude FROM number_risk_signals \
             WHERE number_resource_id = $1 AND observed_at > $2 GROUP BY signal_kind",
            vec![
                signal.number_resource_id.into(),
                (now - policy.signal_window).into(),
            ],
        ))
        .all(&txn)
        .await?;
        let mut score: i64 = 0;
        let mut reasons = Vec::new();
        for s in &window {
            if let Some(k) = SignalKind::parse(&s.signal_kind) {
                score += s.magnitude * k.weight() / 100;
                reasons.push(k.reason());
            }
        }
        let score = score.clamp(0, 100) as i32;
        let scope = (
            assignment.tenant_id,
            assignment.application_id,
            assignment.environment_id,
        );
        let actor = Actor::new(ActorKind::System, "numbering.risk");
        let mut out = SignalOutcome {
            recorded: inserted,
            score,
            outcome: "ALLOW",
            reasons: reasons.iter().map(|r| r.to_string()).collect(),
            decision_id: None,
            suspension_id: None,
        };
        if score >= policy.restrict_score
            && !restriction_in_force(&txn, signal.number_resource_id).await?
        {
            let mut suspension = None;
            if assignment.status() == AssignmentStatus::Active {
                let s = self
                    .suspend_in(
                        &txn,
                        ctx,
                        &assignment,
                        actor.clone(),
                        &super::suspension::SuspendRequest {
                            assignment_id: assignment.id,
                            reason_code: "RISK_RESTRICTED".into(),
                            source: super::suspension::SuspensionSource::Risk,
                            evidence_ref: None,
                        },
                        now,
                    )
                    .await?;
                suspension = Some(s.id);
            }
            let d = insert_decision(
                &txn,
                ctx,
                "SCORE",
                Some(signal.number_resource_id),
                scope,
                "RESTRICT",
                score,
                &reasons,
                None,
                suspension,
                &actor,
                None,
                now,
            )
            .await?;
            incidents::raise(
                &txn,
                incidents::Category::RiskRestriction,
                &format!("risk:{}", signal.number_resource_id),
                incidents::Subject {
                    provider_id: None,
                    number_resource_id: Some(signal.number_resource_id),
                    command_id: None,
                },
                serde_json::json!({ "score": score, "decision": d.id }),
                now,
            )
            .await?;
            out.outcome = "RESTRICT";
            out.decision_id = Some(d.id);
            out.suspension_id = suspension;
        } else if score >= policy.review_score && inserted {
            let d = insert_decision(
                &txn,
                ctx,
                "SCORE",
                Some(signal.number_resource_id),
                scope,
                "REVIEW",
                score,
                &reasons,
                None,
                None,
                &actor,
                None,
                now,
            )
            .await?;
            out.outcome = "REVIEW";
            out.decision_id = Some(d.id);
        } else if restriction_in_force(&txn, signal.number_resource_id).await? {
            out.outcome = "RESTRICT";
        }
        txn.commit().await?;
        crate::metrics::inc(
            "numbering_risk_signals_total",
            &[("kind", signal.kind.as_str())],
        );
        Ok(out)
    }

    /// A customer appeal of a decision of its own scope (appended; PENDING until decided).
    pub async fn file_appeal(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        decision_id: Uuid,
        statement_ref: Option<Uuid>,
    ) -> DomainResult<DecisionRow> {
        let original = self.decision(Some(scope), decision_id).await?;
        if !matches!(original.outcome.as_str(), "RESTRICT" | "DENY" | "REVIEW") {
            return Err(DomainError::precondition(
                "NOT_APPEALABLE",
                "only a restricting, denying or review decision is appealed",
            ));
        }
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let open = count(
            &txn,
            "SELECT count(*) AS n FROM number_risk_decisions a WHERE a.supersedes_id = $1 AND a.decision_kind = 'APPEAL_FILED' \
               AND NOT EXISTS (SELECT 1 FROM number_risk_decisions d WHERE d.supersedes_id = a.id)",
            vec![original.id.into()],
        )
        .await?;
        if open > 0 {
            return Err(DomainError::conflict(
                "APPEAL_PENDING",
                "an appeal of this decision is pending",
            ));
        }
        let row = insert_decision(
            &txn,
            ctx,
            "APPEAL_FILED",
            original.number_resource_id,
            (
                original.tenant_id,
                original.application_id,
                original.environment_id,
            ),
            "PENDING",
            original.score,
            &["APPEAL"],
            Some(original.id),
            None,
            &actor,
            statement_ref,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// An operator decides an appeal: upheld (restriction lifted) or rejected. Both are
    /// appended; neither touches the original decision or its evidence.
    pub async fn decide_appeal(
        &self,
        ctx: &RequestContext,
        operator: &Actor,
        appeal_id: Uuid,
        upheld: bool,
    ) -> DomainResult<DecisionRow> {
        if operator.kind != ActorKind::Operator {
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "appeals are decided by an operator",
            ));
        }
        let appeal = self.decision(None, appeal_id).await?;
        if appeal.decision_kind != "APPEAL_FILED" {
            return Err(DomainError::precondition(
                "NOT_AN_APPEAL",
                "the decision is not an appeal",
            ));
        }
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        if count(
            &txn,
            "SELECT count(*) AS n FROM number_risk_decisions WHERE supersedes_id = $1",
            vec![appeal.id.into()],
        )
        .await?
            > 0
        {
            return Err(DomainError::conflict(
                "APPEAL_DECIDED",
                "the appeal is already decided",
            ));
        }
        let original = self
            .decision(None, appeal.supersedes_id.expect("an appeal supersedes"))
            .await?;
        let (kind, outcome) = if upheld {
            ("APPEAL_UPHELD", "ALLOW")
        } else {
            ("APPEAL_REJECTED", original.outcome.as_str())
        };
        let row = insert_decision(
            &txn,
            ctx,
            kind,
            appeal.number_resource_id,
            (
                appeal.tenant_id,
                appeal.application_id,
                appeal.environment_id,
            ),
            outcome,
            original.score,
            &["APPEAL", "OPERATOR_DECISION"],
            Some(appeal.id),
            None,
            operator,
            None,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// A decision (scope-checked for customers).
    pub async fn decision(
        &self,
        scope: Option<&OwnerScope>,
        id: Uuid,
    ) -> DomainResult<DecisionRow> {
        DecisionRow::find_by_statement(stmt(
            "SELECT * FROM number_risk_decisions WHERE id = $1",
            vec![id.into()],
        ))
        .one(self.numbers.db())
        .await?
        .filter(|d| scope.is_none_or(|s| d.in_scope(s)))
        .ok_or(DomainError::NotFound("risk decision"))
    }

    /// Every decision of a number, oldest first (operators).
    pub async fn decisions_of(&self, resource_id: Uuid) -> DomainResult<Vec<DecisionRow>> {
        Ok(DecisionRow::find_by_statement(stmt(
            "SELECT * FROM number_risk_decisions WHERE number_resource_id = $1 ORDER BY sequence",
            vec![resource_id.into()],
        ))
        .all(self.numbers.db())
        .await?)
    }
}

/// The allocation decision in any connection (reservation creation calls it).
pub async fn evaluate_allocation_in<C: ConnectionTrait>(
    db: &C,
    ctx: &RequestContext,
    policy: &RiskPolicy,
    scope: &OwnerScope,
    req: &AllocationRequest,
    now: DateTime<Utc>,
) -> DomainResult<DecisionRow> {
    let sc = |extra: Vec<sea_orm::Value>| {
        let mut v: Vec<sea_orm::Value> = vec![
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
        ];
        v.extend(extra);
        v
    };
    let active = count(
        db,
        "SELECT (SELECT count(*) FROM dcn_assignments WHERE tenant_id = $1 AND application_id = $2 \
                  AND environment_id = $3 AND status <> 'CLOSED') \
              + (SELECT count(*) FROM number_reservations r WHERE r.tenant_id = $1 AND r.application_id = $2 \
                  AND r.environment_id = $3 AND r.status IN ('PENDING', 'HELD', 'UNKNOWN') \
                  AND NOT EXISTS (SELECT 1 FROM dcn_assignments a WHERE a.reservation_id = r.id)) AS n",
        sc(vec![]),
    )
    .await?;
    let since = |d: Duration| {
        count(
            db,
            "SELECT count(*) AS n FROM number_reservations WHERE tenant_id = $1 AND application_id = $2 \
               AND environment_id = $3 AND created_at > $4",
            sc(vec![(now - d).into()]),
        )
    };
    let daily = since(Duration::hours(24)).await?;
    let weekly = since(Duration::days(7)).await?;
    let countries = count(
        db,
        "SELECT count(DISTINCT n.country_code) AS n FROM dcn_assignments a JOIN number_resources n ON n.id = a.number_resource_id \
          WHERE a.tenant_id = $1 AND a.application_id = $2 AND a.environment_id = $3 AND a.status <> 'CLOSED' \
            AND n.country_code <> $4",
        sc(vec![req.country.clone().into()]),
    )
    .await?
        + 1;
    let class_count = match req.ownership_class {
        Some(c) => count(
            db,
            "SELECT count(*) AS n FROM dcn_assignments WHERE tenant_id = $1 AND application_id = $2 \
               AND environment_id = $3 AND status <> 'CLOSED' AND ownership_class = $4",
            sc(vec![c.as_str().into()]),
        )
        .await?,
        None => 0,
    };
    let mut deny = Vec::new();
    let mut review = Vec::new();
    if policy.restricted_countries.contains(&req.country) {
        deny.push("COUNTRY_RESTRICTED");
    }
    if active >= policy.max_active_numbers {
        deny.push("ACTIVE_NUMBER_LIMIT");
    }
    if let Some(c) = req.ownership_class {
        let limit = policy
            .max_per_class
            .get(c.as_str())
            .copied()
            .unwrap_or(policy.max_active_numbers);
        if class_count >= limit {
            deny.push("CLASS_LIMIT");
        }
    }
    if daily >= policy.daily_allocations {
        deny.push("DAILY_VELOCITY");
        if daily >= policy.daily_allocations * 2 {
            deny.push("MASS_REGISTRATION");
        }
    }
    if weekly >= policy.weekly_allocations {
        deny.push("WEEKLY_VELOCITY");
        if active * 2 < weekly {
            // Many allocations, few kept: numbers are being farmed.
            deny.push("NUMBER_FARMING");
        }
    }
    if countries > policy.multi_country_review {
        review.push("MULTI_COUNTRY_REVIEW");
    }
    let (outcome, reasons, score) = if !deny.is_empty() {
        ("DENY", deny, 90)
    } else if !review.is_empty() {
        ("REVIEW", review, 50)
    } else {
        ("ALLOW", vec!["WITHIN_POLICY"], 0)
    };
    let actor = Actor::new(ActorKind::System, "numbering.risk");
    let row = if outcome == "ALLOW" {
        // Allowed allocations are not recorded one by one (the reservation is the
        // record); an explainable value is still returned.
        DecisionRow {
            id: Uuid::nil(),
            sequence: 0,
            decision_kind: "ALLOCATION".into(),
            number_resource_id: None,
            tenant_id: scope.tenant_id,
            application_id: scope.application_id,
            environment_id: scope.environment_id,
            outcome: outcome.into(),
            score,
            reasons: reasons.iter().map(|r| r.to_string()).collect(),
            policy_version: POLICY_VERSION.into(),
            supersedes_id: None,
            suspension_id: None,
            actor_kind: actor.kind.as_str().into(),
            actor_ref: actor.reference.clone(),
            statement_ref: None,
            decided_at: now,
            correlation_id: ctx.correlation_id.as_str().into(),
        }
    } else {
        insert_decision(
            db,
            ctx,
            "ALLOCATION",
            None,
            (scope.tenant_id, scope.application_id, scope.environment_id),
            outcome,
            score,
            &reasons,
            None,
            None,
            &actor,
            None,
            now,
        )
        .await?
    };
    Ok(row)
}

/// Whether new outbound use of a number is refused by its spend (provider cost in the
/// last 24 hours at or above the threshold).
pub async fn spend_exceeded<C: ConnectionTrait>(
    db: &C,
    policy: &RiskPolicy,
    resource_id: Uuid,
    now: DateTime<Utc>,
) -> DomainResult<bool> {
    #[derive(FromQueryResult)]
    struct Sum {
        total: Option<i64>,
    }
    let total = Sum::find_by_statement(stmt(
        "SELECT sum(provider_cost_minor)::bigint AS total FROM number_usage_records \
          WHERE number_resource_id = $1 AND recorded_at > $2 \
            AND usage_kind IN ('VOICE_OUTBOUND_SECONDS', 'SMS_OUTBOUND_SEGMENTS', 'VOICE_INBOUND_SECONDS', 'SMS_INBOUND_SEGMENTS')",
        vec![resource_id.into(), (now - Duration::hours(24)).into()],
    ))
    .one(db)
    .await?
    .and_then(|s| s.total)
    .unwrap_or(0);
    Ok(total >= policy.number_spend_threshold_minor)
}

/// Whether the provider's cost in the last hour opened the circuit breaker (new
/// provisioning refused; an incident is raised once per window).
pub async fn cost_breaker_open<C: ConnectionTrait>(
    db: &C,
    policy: &RiskPolicy,
    provider_id: &str,
    now: DateTime<Utc>,
) -> DomainResult<bool> {
    #[derive(FromQueryResult)]
    struct Sum {
        total: Option<i64>,
    }
    let total = Sum::find_by_statement(stmt(
        "SELECT sum(u.provider_cost_minor)::bigint AS total FROM number_usage_records u \
          WHERE u.recorded_at > $2 AND EXISTS (SELECT 1 FROM number_provider_bindings b \
                 WHERE b.number_resource_id = u.number_resource_id AND b.provider_id = $1)",
        vec![provider_id.into(), (now - Duration::hours(1)).into()],
    ))
    .one(db)
    .await?
    .and_then(|s| s.total)
    .unwrap_or(0);
    let open = total >= policy.provider_cost_spike_minor;
    if open {
        incidents::raise(
            db,
            incidents::Category::ProviderCostSpike,
            &format!("cost-spike:{provider_id}:{}", now.format("%Y%m%d%H")),
            incidents::Subject {
                provider_id: Some(provider_id.to_string()),
                number_resource_id: None,
                command_id: None,
            },
            serde_json::json!({ "costMinorLastHour": total }),
            now,
        )
        .await?;
    }
    Ok(open)
}
