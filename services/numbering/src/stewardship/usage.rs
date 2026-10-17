//! Workstream S — usage and billing readiness.
//!
//! `number_usage_records` is INTERNAL cost and usage evidence for a future billing system:
//! provider costs (never customer prices), attributed to the number, its assignment and
//! the owning scope. Every record has a source key, so a retry, a repeated callback or a
//! restarted worker never adds a second entry. Records are PROVISIONAL until finalized
//! (a provider's cost reconciliation may still correct the cost), then FINAL and
//! immutable.
//!
//! * number rental (one record per assignment per calendar month), setup (once per
//!   assignment) and renewal (per renewal date) accrue from the number's commercial facts;
//! * voice seconds and SMS segments are reported by their carriers (Telephony, SMS
//!   Gateway), which keep their own metering — Numbering keeps the per-number attribution;
//! * customers see quantities of their own scope only; operators also see costs.
//!
//! No retail pricing, invoice, checkout or Part VI commercial system lives here.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::Stewardship;
use crate::{
    domain::RequestContext,
    infra::outbox::{self, EventScope, NewEvent},
    numbers::{
        assignments::AssignmentRow, stmt, CostEvidence, DomainError, DomainResult, OwnerScope,
    },
};

/// What a usage record measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UsageKind {
    NumberRental,
    NumberSetup,
    NumberRenewal,
    VoiceInboundSeconds,
    VoiceOutboundSeconds,
    SmsInboundSegments,
    SmsOutboundSegments,
    Porting,
}

impl UsageKind {
    pub const ALL: &'static [UsageKind] = &[
        UsageKind::NumberRental,
        UsageKind::NumberSetup,
        UsageKind::NumberRenewal,
        UsageKind::VoiceInboundSeconds,
        UsageKind::VoiceOutboundSeconds,
        UsageKind::SmsInboundSegments,
        UsageKind::SmsOutboundSegments,
        UsageKind::Porting,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            UsageKind::NumberRental => "NUMBER_RENTAL",
            UsageKind::NumberSetup => "NUMBER_SETUP",
            UsageKind::NumberRenewal => "NUMBER_RENEWAL",
            UsageKind::VoiceInboundSeconds => "VOICE_INBOUND_SECONDS",
            UsageKind::VoiceOutboundSeconds => "VOICE_OUTBOUND_SECONDS",
            UsageKind::SmsInboundSegments => "SMS_INBOUND_SEGMENTS",
            UsageKind::SmsOutboundSegments => "SMS_OUTBOUND_SEGMENTS",
            UsageKind::Porting => "PORTING",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == v)
    }

    pub fn unit(&self) -> &'static str {
        match self {
            UsageKind::NumberRental => "MONTH",
            UsageKind::NumberSetup | UsageKind::NumberRenewal | UsageKind::Porting => "EVENT",
            UsageKind::VoiceInboundSeconds | UsageKind::VoiceOutboundSeconds => "SECOND",
            UsageKind::SmsInboundSegments | UsageKind::SmsOutboundSegments => "SEGMENT",
        }
    }

    /// The carrier allowed to report the kind (accrued kinds: Numbering itself).
    pub fn reporter(&self) -> &'static str {
        match self {
            UsageKind::VoiceInboundSeconds | UsageKind::VoiceOutboundSeconds => "cfmeet.telephony",
            UsageKind::SmsInboundSegments | UsageKind::SmsOutboundSegments => "cfmeet.sms-gateway",
            _ => "cfmeet.numbering",
        }
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRow {
    pub id: Uuid,
    pub source_key: String,
    pub number_resource_id: Uuid,
    pub assignment_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub usage_kind: String,
    pub quantity: i64,
    pub unit: String,
    pub provider_cost_minor: Option<i64>,
    pub currency: Option<String>,
    pub cost_evidence: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub reported_by: String,
    pub status: String,
    pub recorded_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
}

/// One usage fact.
#[derive(Debug, Clone)]
pub struct NewUsage {
    pub source_key: String,
    pub number_resource_id: Uuid,
    pub assignment_id: Option<Uuid>,
    pub kind: UsageKind,
    pub quantity: i64,
    pub provider_cost_minor: Option<i64>,
    pub currency: Option<String>,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
}

/// The aggregate view of a scope's usage (per number and kind).
#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub number_resource_id: Uuid,
    pub assignment_id: Option<Uuid>,
    pub usage_kind: String,
    pub unit: String,
    pub quantity: i64,
    pub provider_cost_minor: Option<i64>,
    pub currency: Option<String>,
    pub records: i64,
    pub final_records: i64,
}

fn month_start(t: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(t.year(), t.month(), 1, 0, 0, 0)
        .single()
        .expect("valid month start")
}

fn next_month(t: DateTime<Utc>) -> DateTime<Utc> {
    let (y, m) = if t.month() == 12 {
        (t.year() + 1, 1)
    } else {
        (t.year(), t.month() + 1)
    };
    Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0)
        .single()
        .expect("valid month start")
}

/// Records a usage fact (idempotent per source key). Returns the record and whether it
/// was new. The same key with different facts is a conflict, never an overwrite.
pub async fn record_usage_in<C: ConnectionTrait>(
    db: &C,
    ctx: &RequestContext,
    reporter: &str,
    new: &NewUsage,
    now: DateTime<Utc>,
) -> DomainResult<(UsageRow, bool)> {
    if new.quantity < 0 || new.provider_cost_minor.is_some_and(|c| c < 0) {
        return Err(DomainError::Validation("usage is never negative".into()));
    }
    if new.provider_cost_minor.is_some() != new.currency.is_some() {
        return Err(DomainError::Validation(
            "a cost needs its ISO 4217 currency".into(),
        ));
    }
    if new.source_key.is_empty() || new.source_key.len() > 200 {
        return Err(DomainError::Validation(
            "a source key is 1–200 characters".into(),
        ));
    }
    let scope = match new.assignment_id {
        Some(a) => {
            let a = AssignmentRow::find_by_statement(stmt(
                "SELECT * FROM dcn_assignments WHERE id = $1",
                vec![a.into()],
            ))
            .one(db)
            .await?
            .filter(|a| a.number_resource_id == new.number_resource_id)
            .ok_or(DomainError::NotFound("assignment"))?;
            Some((a.tenant_id, a.application_id, a.environment_id))
        }
        None => None,
    };
    let evidence = match new.provider_cost_minor {
        Some(_) => CostEvidence::Known,
        None => CostEvidence::Unknown,
    };
    let inserted = UsageRow::find_by_statement(stmt(
        "INSERT INTO number_usage_records (id, source_key, number_resource_id, assignment_id, tenant_id, \
           application_id, environment_id, usage_kind, quantity, unit, provider_cost_minor, currency, cost_evidence, \
           period_start, period_end, reported_by, status, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, 'PROVISIONAL', $17) \
         ON CONFLICT (source_key) DO NOTHING RETURNING *",
        vec![
            Uuid::new_v4().into(),
            new.source_key.clone().into(),
            new.number_resource_id.into(),
            new.assignment_id.into(),
            scope.map(|s| s.0).into(),
            scope.map(|s| s.1).into(),
            scope.map(|s| s.2).into(),
            new.kind.as_str().into(),
            new.quantity.into(),
            new.kind.unit().into(),
            new.provider_cost_minor.into(),
            new.currency.clone().into(),
            evidence.as_str().into(),
            new.period_start.into(),
            new.period_end.into(),
            reporter.trim_start_matches("cfmeet.").into(),
            now.into(),
        ],
    ))
    .one(db)
    .await?;
    if let Some(row) = inserted {
        outbox::enqueue(
            db,
            NewEvent {
                event_type: "number.usage.recorded",
                aggregate_type: "NUMBER_RESOURCE",
                aggregate_id: row.number_resource_id,
                scope: scope.map(|s| EventScope {
                    tenant_id: s.0,
                    application_id: s.1,
                    environment_id: s.2,
                }),
                payload: serde_json::json!({
                    "usage_record_id": row.id,
                    "number_resource_id": row.number_resource_id,
                    "assignment_id": row.assignment_id,
                    "usage_kind": row.usage_kind,
                    "quantity": row.quantity,
                    "unit": row.unit,
                    "status": row.status,
                    "period_start": row.period_start,
                    "period_end": row.period_end,
                }),
            },
            ctx,
            now,
        )
        .await?;
        crate::metrics::inc(
            "numbering_usage_records_total",
            &[("kind", new.kind.as_str())],
        );
        return Ok((row, true));
    }
    let existing = UsageRow::find_by_statement(stmt(
        "SELECT * FROM number_usage_records WHERE source_key = $1",
        vec![new.source_key.clone().into()],
    ))
    .one(db)
    .await?
    .ok_or(DomainError::NotFound("usage record"))?;
    if existing.number_resource_id != new.number_resource_id
        || existing.usage_kind != new.kind.as_str()
        || existing.quantity != new.quantity
    {
        return Err(DomainError::conflict(
            "USAGE_SOURCE_CONFLICT",
            "the source key was recorded with different usage",
        ));
    }
    Ok((existing, false))
}

impl Stewardship {
    /// A carrier's usage report (voice seconds / SMS segments of a number).
    pub async fn report_usage(
        &self,
        ctx: &RequestContext,
        reporter: &str,
        new: NewUsage,
    ) -> DomainResult<(UsageRow, bool)> {
        if new.kind.reporter() != reporter {
            return Err(DomainError::precondition(
                "USAGE_KIND_NOT_PERMITTED",
                "this service does not report this usage kind",
            ));
        }
        let key = NewUsage {
            source_key: format!("{reporter}:{}", new.source_key),
            ..new
        };
        record_usage_in(self.numbers.db(), ctx, reporter, &key, self.now()).await
    }

    /// Worker pass: accrues rental (per assignment and calendar month), setup (once per
    /// assignment) and renewal (per renewal date) from the numbers' commercial facts.
    pub async fn accrue_usage(&self, ctx: &RequestContext, limit: u64) -> DomainResult<usize> {
        #[derive(FromQueryResult)]
        struct Due {
            assignment_id: Uuid,
            number_resource_id: Uuid,
            activated_at: DateTime<Utc>,
            monthly_cost_minor: Option<i64>,
            setup_cost_minor: Option<i64>,
            currency: Option<String>,
            renewal_at: Option<DateTime<Utc>>,
        }
        let now = self.now();
        let due = Due::find_by_statement(stmt(
            "SELECT a.id AS assignment_id, a.number_resource_id, a.activated_at, n.monthly_cost_minor, \
                    n.setup_cost_minor, n.currency, n.renewal_at \
               FROM dcn_assignments a JOIN number_resources n ON n.id = a.number_resource_id \
              WHERE a.status IN ('ACTIVE', 'SUSPENDED') AND a.activated_at IS NOT NULL \
              ORDER BY a.activated_at LIMIT $1",
            vec![(limit.clamp(1, 1000) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let mut n = 0;
        for d in due {
            let start = month_start(now);
            let cost = |c: Option<i64>| c.and(d.currency.clone()).map(|cur| (c.unwrap(), cur));
            let mut facts = vec![NewUsage {
                source_key: format!(
                    "rental:{}:{}",
                    d.assignment_id.simple(),
                    start.format("%Y-%m")
                ),
                number_resource_id: d.number_resource_id,
                assignment_id: Some(d.assignment_id),
                kind: UsageKind::NumberRental,
                quantity: 1,
                provider_cost_minor: cost(d.monthly_cost_minor).map(|c| c.0),
                currency: cost(d.monthly_cost_minor).map(|c| c.1),
                period_start: start.max(d.activated_at),
                period_end: next_month(now),
            }];
            if d.setup_cost_minor.is_some() {
                facts.push(NewUsage {
                    source_key: format!("setup:{}", d.assignment_id.simple()),
                    number_resource_id: d.number_resource_id,
                    assignment_id: Some(d.assignment_id),
                    kind: UsageKind::NumberSetup,
                    quantity: 1,
                    provider_cost_minor: cost(d.setup_cost_minor).map(|c| c.0),
                    currency: cost(d.setup_cost_minor).map(|c| c.1),
                    period_start: d.activated_at,
                    period_end: d.activated_at,
                });
            }
            if let Some(r) = d.renewal_at.filter(|r| *r <= now) {
                facts.push(NewUsage {
                    source_key: format!(
                        "renewal:{}:{}",
                        d.number_resource_id.simple(),
                        r.format("%Y-%m-%d")
                    ),
                    number_resource_id: d.number_resource_id,
                    assignment_id: Some(d.assignment_id),
                    kind: UsageKind::NumberRenewal,
                    quantity: 1,
                    provider_cost_minor: None,
                    currency: None,
                    period_start: r,
                    period_end: r,
                });
            }
            for f in facts {
                if record_usage_in(self.numbers.db(), ctx, "cfmeet.numbering", &f, now)
                    .await?
                    .1
                {
                    n += 1;
                }
            }
        }
        Ok(n)
    }

    /// Finalizes every PROVISIONAL record whose period ended before `cutoff`.
    pub async fn finalize_usage(&self, cutoff: DateTime<Utc>) -> DomainResult<u64> {
        let now = self.now();
        let r = self
            .numbers
            .db()
            .execute(stmt(
                "UPDATE number_usage_records SET status = 'FINAL', finalized_at = $2 \
                 WHERE status = 'PROVISIONAL' AND period_end < $1",
                vec![cutoff.min(now).into(), now.into()],
            ))
            .await?;
        Ok(r.rows_affected())
    }

    /// The provider's reconciled cost of a provisional record (a FINAL one is refused by
    /// the database).
    pub async fn reconcile_usage_cost(
        &self,
        source_key: &str,
        cost_minor: i64,
        currency: &str,
    ) -> DomainResult<UsageRow> {
        if cost_minor < 0
            || currency.len() != 3
            || !currency.bytes().all(|b| b.is_ascii_uppercase())
        {
            return Err(DomainError::Validation(
                "a cost is a non-negative amount and its ISO 4217 currency".into(),
            ));
        }
        UsageRow::find_by_statement(stmt(
            "UPDATE number_usage_records SET provider_cost_minor = $2, currency = $3, cost_evidence = 'KNOWN' \
             WHERE source_key = $1 AND status = 'PROVISIONAL' RETURNING *",
            vec![source_key.into(), cost_minor.into(), currency.into()],
        ))
        .one(self.numbers.db())
        .await?
        .ok_or(DomainError::precondition(
            "USAGE_FINAL",
            "only a provisional usage record is reconciled",
        ))
    }

    /// The usage of a scope in `[from, to)`, per number and kind.
    pub async fn usage_summary(
        &self,
        scope: &OwnerScope,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> DomainResult<Vec<UsageSummary>> {
        if to <= from || to - from > Duration::days(400) {
            return Err(DomainError::Validation(
                "a usage window is 1 second to 400 days".into(),
            ));
        }
        Ok(UsageSummary::find_by_statement(stmt(
            "SELECT number_resource_id, assignment_id, usage_kind, unit, sum(quantity)::bigint AS quantity, \
                    sum(provider_cost_minor)::bigint AS provider_cost_minor, min(currency) AS currency, \
                    count(*) AS records, count(*) FILTER (WHERE status = 'FINAL') AS final_records \
               FROM number_usage_records \
              WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
                AND period_start >= $4 AND period_start < $5 \
              GROUP BY number_resource_id, assignment_id, usage_kind, unit \
              ORDER BY number_resource_id, usage_kind",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                from.into(),
                to.into(),
            ],
        ))
        .all(self.numbers.db())
        .await?)
    }

    /// Every record of a number (operators).
    pub async fn usage_of(&self, resource_id: Uuid) -> DomainResult<Vec<UsageRow>> {
        Ok(UsageRow::find_by_statement(stmt(
            "SELECT * FROM number_usage_records WHERE number_resource_id = $1 ORDER BY period_start, source_key",
            vec![resource_id.into()],
        ))
        .all(self.numbers.db())
        .await?)
    }
}
