//! Section 55 §11.3–§11.4 — the unified usage ledger.
//!
//! Domain services stay the authorities of the facts they produce; the Developer
//! Platform is the cross-domain aggregation / read authority of customer consumption.
//!
//! * [`ingest`]: facts of the AUTHENTICATED producer (`developer-event.publish`; the
//!   producer is never read from a body) are validated against the shared catalogue
//!   (`cf_usage_facts`: type, unit, producer, safe refs, bounded dimensions, digest), their
//!   tenant / application / environment mapping is verified against the anchored
//!   environment or Platform Access (a mismatch is refused), and they are stored once:
//!   `(producer, source_fact_id)` is unique, an identical replay is a DUPLICATE, a
//!   different payload under the same id is refused (CONFLICT, recorded for operators).
//!   Daily rollups are updated in the same transaction; the stream watermark advances in
//!   it too, so a producer restart resumes after the last acknowledged batch.
//! * [`api_operation_facts`]: the API Gateway's API_OPERATION counts become facts inside
//!   the idempotent request-accounting batch transaction.
//! * Rollups are derived data: [`rebuild_rollups`] recomputes them from the facts.
//! * [`reconcile`]: shadow reconciliation of producer totals against the ledger.
//! * [`backfill_api_operations`]: historical API_OPERATION counters become facts for days
//!   the gateway never reported facts for.
//! * Cost-centre attribution: facts of an INTERNAL_COST_CENTER organization carry its
//!   cost-centre reference (first-party cost allocation through the same facts).

pub mod views;

use std::collections::HashMap;

use cf_usage_facts::{UsageFact, MAX_BATCH};
use chrono::{DateTime, NaiveDate, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    control::stmt,
    http::error::{ApiError, ErrorCode},
    platform_access::{Delegated, DelegationCall},
    state::AppState,
};

/// The authoritative mapping of an environment.
#[derive(Debug, Clone)]
pub struct Ownership {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub kind: String,
    pub cost_center_ref: Option<String>,
}

async fn cost_center<C: ConnectionTrait>(db: &C, tenant: Uuid) -> Result<Option<String>, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT cost_center_ref FROM devplat_billing_accounts \
             WHERE tenant_id = $1 AND classification = 'INTERNAL_COST_CENTER'",
            vec![tenant.into()],
        ))
        .await?
        .map(|r| r.try_get::<Option<String>>("", "cost_center_ref"))
        .transpose()?
        .flatten())
}

/// The environment's tenant / application / kind: the Developer Platform profile, else
/// the anchored reference completed by Platform Access, else Platform Access itself (then
/// anchored). `None`: unknown to the authority.
pub async fn ownership(
    state: &AppState,
    environment_id: Uuid,
) -> Result<Option<Ownership>, ApiError> {
    let row = state
        .db
        .query_one(stmt(
            "SELECT COALESCE(p.tenant_id, r.tenant_id) AS tenant_id, \
                    COALESCE(p.application_id, r.application_id) AS application_id, p.kind \
             FROM (SELECT $1::uuid AS environment_id) q \
             LEFT JOIN devplat_environment_profiles p ON p.environment_id = q.environment_id \
             LEFT JOIN devplat_environment_refs r ON r.environment_id = q.environment_id",
            vec![environment_id.into()],
        ))
        .await?;
    let (tenant, app, kind) = match row {
        Some(r) => (
            r.try_get::<Option<Uuid>>("", "tenant_id")?,
            r.try_get::<Option<Uuid>>("", "application_id")?,
            r.try_get::<Option<String>>("", "kind")?,
        ),
        None => (None, None, None),
    };
    if let (Some(t), Some(a), Some(k)) = (tenant, app, kind.clone()) {
        return Ok(Some(Ownership {
            tenant_id: t,
            application_id: a,
            kind: k,
            cost_center_ref: cost_center(&state.db, t).await?,
        }));
    }
    let correlation = cf_service_auth::CorrelationId::generate();
    let pa = match state
        .platform_access
        .delegate(
            DelegationCall::GetEnvironment { environment_id },
            &correlation,
        )
        .await
    {
        Ok(Delegated::Ok(v)) => v,
        Ok(Delegated::Refused { status: 404, .. }) => return Ok(None),
        Ok(Delegated::Refused { .. }) | Err(_) => {
            return Err(ApiError(ErrorCode::ServiceUnavailable))
        }
    };
    let id = |k: &str| {
        pa.get(k)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    let (Some(t), Some(a), Some(k)) = (
        id("tenantId"),
        id("applicationId"),
        pa.get("kind").and_then(Value::as_str).map(str::to_string),
    ) else {
        return Err(ApiError(ErrorCode::ServiceUnavailable));
    };
    if tenant.is_some_and(|x| x != t) || app.is_some_and(|x| x != a) {
        tracing::warn!("anchored environment reference disagrees with the identity authority");
        return Err(ApiError(ErrorCode::ServiceUnavailable));
    }
    let now = Utc::now();
    state
        .db
        .execute(stmt(
            "INSERT INTO devplat_environment_refs (id, tenant_id, application_id, environment_id, first_seen_at, last_seen_at) \
             VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT (environment_id) DO NOTHING",
            vec![
                Uuid::new_v4().into(),
                t.into(),
                a.into(),
                environment_id.into(),
                now.into(),
            ],
        ))
        .await?;
    Ok(Some(Ownership {
        tenant_id: t,
        application_id: a,
        kind: k,
        cost_center_ref: cost_center(&state.db, t).await?,
    }))
}

/// The per-fact outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Recorded,
    Duplicate,
    Refused(&'static str),
}

impl Outcome {
    fn as_str(&self) -> &'static str {
        match self {
            Outcome::Recorded => "RECORDED",
            Outcome::Duplicate => "DUPLICATE",
            Outcome::Refused(_) => "REFUSED",
        }
    }
}

async fn reject<C: ConnectionTrait>(
    db: &C,
    producer: &str,
    source: &str,
    code: &str,
) -> Result<(), ApiError> {
    db.execute(stmt(
        "INSERT INTO devplat_usage_rejections (id, producer, source_fact_id, reason_code, received_at) \
         VALUES ($1, $2, $3, $4, $5)",
        vec![
            Uuid::new_v4().into(),
            producer.into(),
            source.chars().take(160).collect::<String>().into(),
            code.into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Stores one VALIDATED fact with its ownership in `txn` (and its rollup).
pub async fn store(
    txn: &DatabaseTransaction,
    producer: &str,
    fact: &UsageFact,
    own: &Ownership,
) -> Result<Outcome, ApiError> {
    let day = fact.occurred_at.date_naive();
    let inserted = txn
        .execute(stmt(
            "INSERT INTO devplat_usage_facts (id, producer, source_fact_id, payload_digest, usage_type, unit, \
               quantity, tenant_id, application_id, environment_id, environment_kind, resource_ref, \
               occurred_at, usage_day, period_start, period_end, dimensions, cost_center_ref, ingested_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) \
             ON CONFLICT (producer, source_fact_id) DO NOTHING",
            vec![
                Uuid::new_v4().into(),
                producer.into(),
                fact.source_fact_id.clone().into(),
                fact.digest.clone().into(),
                fact.usage_type.clone().into(),
                fact.unit.clone().into(),
                (fact.quantity as i64).into(),
                own.tenant_id.into(),
                own.application_id.into(),
                fact.environment_id.into(),
                own.kind.clone().into(),
                fact.resource_ref.clone().into(),
                fact.occurred_at.into(),
                day.into(),
                fact.period_start.into(),
                fact.period_end.into(),
                serde_json::to_value(&fact.dimensions).unwrap_or(json!({})).into(),
                own.cost_center_ref.clone().into(),
                Utc::now().into(),
            ],
        ))
        .await?
        .rows_affected();
    if inserted == 0 {
        let stored: String = txn
            .query_one(stmt(
                "SELECT payload_digest FROM devplat_usage_facts WHERE producer = $1 AND source_fact_id = $2",
                vec![producer.into(), fact.source_fact_id.clone().into()],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::ServiceUnavailable))?
            .try_get("", "payload_digest")?;
        return Ok(if stored == fact.digest {
            Outcome::Duplicate
        } else {
            Outcome::Refused("CONFLICTING_REPLAY")
        });
    }
    txn.execute(stmt(
        "INSERT INTO devplat_usage_rollups_daily (environment_id, usage_day, usage_type, unit, tenant_id, \
           application_id, environment_kind, cost_center_ref, quantity, fact_count, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10) \
         ON CONFLICT (environment_id, usage_day, usage_type) DO UPDATE SET \
           quantity = devplat_usage_rollups_daily.quantity + EXCLUDED.quantity, \
           fact_count = devplat_usage_rollups_daily.fact_count + 1, \
           cost_center_ref = EXCLUDED.cost_center_ref, updated_at = EXCLUDED.updated_at",
        vec![
            fact.environment_id.into(),
            day.into(),
            fact.usage_type.clone().into(),
            fact.unit.clone().into(),
            own.tenant_id.into(),
            own.application_id.into(),
            own.kind.clone().into(),
            own.cost_center_ref.clone().into(),
            (fact.quantity as i64).into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    Ok(Outcome::Recorded)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PositionBody {
    pub at: DateTime<Utc>,
    pub key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngestBody {
    #[serde(default)]
    pub stream: Option<String>,
    pub facts: Vec<UsageFact>,
    #[serde(default)]
    pub advance_to: Option<PositionBody>,
}

fn valid_stream(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(c))
}

/// `POST /internal/v1/usage/facts`.
pub async fn ingest(state: &AppState, producer: &str, body: IngestBody) -> Result<Value, ApiError> {
    if body.facts.len() > MAX_BATCH
        || body.stream.as_deref().is_some_and(|s| !valid_stream(s))
        || body.advance_to.is_some() != body.stream.is_some()
        || body
            .advance_to
            .as_ref()
            .is_some_and(|p| p.key.is_empty() || p.key.len() > 160)
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    // Validation and ownership first (outside the transaction: Platform Access may be
    // asked about an environment the Developer Platform has not anchored yet).
    let mut owners: HashMap<Uuid, Option<Ownership>> = HashMap::new();
    let mut checked = Vec::with_capacity(body.facts.len());
    for fact in body.facts {
        let verdict = match fact.validate(producer) {
            Err(r) => Err(r.code()),
            Ok(_) => {
                if let std::collections::hash_map::Entry::Vacant(e) =
                    owners.entry(fact.environment_id)
                {
                    e.insert(ownership(state, fact.environment_id).await?);
                }
                match owners.get(&fact.environment_id).cloned().flatten() {
                    None => Err("UNKNOWN_ENVIRONMENT"),
                    Some(o)
                        if o.tenant_id != fact.tenant_id
                            || o.application_id != fact.application_id =>
                    {
                        Err("OWNERSHIP_MISMATCH")
                    }
                    Some(o)
                        if fact
                            .environment_kind
                            .as_deref()
                            .is_some_and(|k| k != o.kind) =>
                    {
                        Err("ENVIRONMENT_KIND_MISMATCH")
                    }
                    Some(o) => Ok(o),
                }
            }
        };
        checked.push((fact, verdict));
    }
    let txn = state.db.begin().await?;
    let mut results = Vec::with_capacity(checked.len());
    let (mut recorded, mut duplicates, mut refused) = (0u64, 0u64, 0u64);
    for (fact, verdict) in &checked {
        let outcome = match verdict {
            Err(code) => Outcome::Refused(code),
            Ok(own) => store(&txn, producer, fact, own).await?,
        };
        match outcome {
            Outcome::Recorded => recorded += 1,
            Outcome::Duplicate => duplicates += 1,
            Outcome::Refused(code) => {
                refused += 1;
                reject(&txn, producer, &fact.source_fact_id, code).await?;
                tracing::warn!(producer, code, "usage fact refused");
            }
        }
        let mut r = json!({ "sourceFactId": fact.source_fact_id, "outcome": outcome.as_str() });
        if let Outcome::Refused(code) = outcome {
            r["reasonCode"] = Value::from(code);
        }
        results.push(r);
    }
    let mut position = Value::Null;
    if let (Some(stream), Some(p)) = (&body.stream, &body.advance_to) {
        txn.execute(stmt(
            "INSERT INTO devplat_usage_streams (producer, stream, position_at, position_key, facts_total, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (producer, stream) DO UPDATE SET \
               position_at = GREATEST(devplat_usage_streams.position_at, EXCLUDED.position_at), \
               position_key = CASE WHEN devplat_usage_streams.position_at IS NULL \
                    OR (EXCLUDED.position_at, EXCLUDED.position_key) >= (devplat_usage_streams.position_at, devplat_usage_streams.position_key) \
                    THEN EXCLUDED.position_key ELSE devplat_usage_streams.position_key END, \
               facts_total = devplat_usage_streams.facts_total + EXCLUDED.facts_total, \
               updated_at = EXCLUDED.updated_at",
            vec![
                producer.into(),
                stream.clone().into(),
                p.at.into(),
                p.key.clone().into(),
                (recorded as i64).into(),
                Utc::now().into(),
            ],
        ))
        .await?;
        position = stream_position(&txn, producer, stream).await?;
    }
    txn.commit().await?;
    Ok(json!({
        "recorded": recorded, "duplicates": duplicates, "refused": refused,
        "results": results, "position": position,
    }))
}

/// The acknowledged watermark of a producer stream (`null`: none yet — full replay).
pub async fn stream_position<C: ConnectionTrait>(
    db: &C,
    producer: &str,
    stream: &str,
) -> Result<Value, ApiError> {
    if !valid_stream(stream) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let row = db
        .query_one(stmt(
            "SELECT position_at, position_key FROM devplat_usage_streams WHERE producer = $1 AND stream = $2",
            vec![producer.into(), stream.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => match (
            r.try_get::<Option<DateTime<Utc>>>("", "position_at")?,
            r.try_get::<Option<String>>("", "position_key")?,
        ) {
            (Some(at), Some(key)) => json!({
                "at": at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true), "key": key,
            }),
            _ => Value::Null,
        },
        None => Value::Null,
    })
}

/// One normalized gateway operation count of a request-accounting batch.
pub struct OperationFact<'a> {
    pub index: usize,
    pub environment_id: Uuid,
    pub day: NaiveDate,
    pub domain: &'a str,
    pub operation_id: &'a str,
    pub status_class: &'a str,
    pub credential_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
    pub count: u64,
}

/// The gateway's API_OPERATION facts of batch `batch_id`, stored inside the batch's own
/// (exactly-once) transaction. Environments without an authoritative mapping are
/// counted in the legacy counters only (logged).
pub async fn api_operation_facts(
    txn: &DatabaseTransaction,
    owners: &HashMap<Uuid, Ownership>,
    batch_id: Uuid,
    ops: &[OperationFact<'_>],
) -> Result<u64, ApiError> {
    let producer = cf_usage_facts::producers::API_GATEWAY;
    let mut n = 0;
    for o in ops {
        let Some(own) = owners.get(&o.environment_id) else {
            tracing::warn!("API_OPERATION usage of an environment without ownership mapping");
            continue;
        };
        let mut dimensions = std::collections::BTreeMap::new();
        dimensions.insert("domain".to_string(), json!(o.domain));
        dimensions.insert("operationId".to_string(), json!(o.operation_id));
        dimensions.insert("statusClass".to_string(), json!(o.status_class));
        if let Some(c) = o.credential_id {
            dimensions.insert("credentialId".to_string(), json!(c.to_string()));
        }
        if let Some(s) = o.service_account_id {
            dimensions.insert("serviceAccountId".to_string(), json!(s.to_string()));
        }
        let mut resource = format!("api:{}:{}", o.domain, o.operation_id);
        resource.truncate(160);
        let fact = UsageFact {
            source_fact_id: format!("{}:{}", batch_id.simple(), o.index),
            usage_type: "API_OPERATION".into(),
            unit: "OPERATION".into(),
            quantity: o.count,
            tenant_id: own.tenant_id,
            application_id: own.application_id,
            environment_id: o.environment_id,
            environment_kind: Some(own.kind.clone()),
            resource_ref: resource,
            occurred_at: o.day.and_hms_opt(0, 0, 0).expect("midnight").and_utc(),
            period_start: None,
            period_end: None,
            dimensions,
            digest: String::new(),
        }
        .sealed();
        if fact.validate(producer).is_err() {
            continue;
        }
        if store(txn, producer, &fact, own).await? == Outcome::Recorded {
            n += 1;
        }
    }
    Ok(n)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileEntry {
    pub environment_id: Uuid,
    pub usage_type: String,
    pub quantity: u64,
    pub fact_count: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileBody {
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub entries: Vec<ReconcileEntry>,
}

/// Compares `producer`'s totals with the ledger for one window (shadow reconciliation)
/// and records every comparison.
pub async fn reconcile<C: ConnectionTrait>(
    db: &C,
    producer: &str,
    body: ReconcileBody,
) -> Result<Value, ApiError> {
    if body.window_end <= body.window_start
        || body.entries.len() > 1000
        || body
            .entries
            .iter()
            .any(|e| cf_usage_facts::definition(&e.usage_type).is_none())
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let mut results = Vec::new();
    let mut mismatches = 0u64;
    for e in &body.entries {
        let row = db
            .query_one(stmt(
                "SELECT COALESCE(SUM(quantity), 0)::bigint AS q, COUNT(*)::bigint AS n FROM devplat_usage_facts \
                 WHERE producer = $1 AND environment_id = $2 AND usage_type = $3 \
                   AND occurred_at >= $4 AND occurred_at < $5",
                vec![
                    producer.into(),
                    e.environment_id.into(),
                    e.usage_type.clone().into(),
                    body.window_start.into(),
                    body.window_end.into(),
                ],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::InternalError))?;
        let (q, n): (i64, i64) = (row.try_get("", "q")?, row.try_get("", "n")?);
        let matched = q as u64 == e.quantity && n as u64 == e.fact_count;
        if !matched {
            mismatches += 1;
        }
        record_reconciliation(
            db,
            producer,
            e.environment_id,
            &e.usage_type,
            body.window_start,
            body.window_end,
            (e.quantity as i64, e.fact_count as i64),
            (q, n),
        )
        .await?;
        results.push(json!({
            "environmentId": e.environment_id, "usageType": e.usage_type,
            "producerQuantity": e.quantity, "producerCount": e.fact_count,
            "ledgerQuantity": q, "ledgerCount": n,
            "outcome": if matched { "MATCH" } else { "MISMATCH" },
        }));
    }
    if mismatches > 0 {
        tracing::warn!(
            producer,
            mismatches,
            "usage reconciliation found differences"
        );
    }
    Ok(json!({ "results": results, "mismatches": mismatches }))
}

#[allow(clippy::too_many_arguments)]
async fn record_reconciliation<C: ConnectionTrait>(
    db: &C,
    producer: &str,
    environment_id: Uuid,
    usage_type: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    producer_totals: (i64, i64),
    ledger: (i64, i64),
) -> Result<(), ApiError> {
    let matched = producer_totals == ledger;
    db.execute(stmt(
        "INSERT INTO devplat_usage_reconciliations (id, producer, environment_id, usage_type, window_start, \
           window_end, producer_quantity, producer_count, ledger_quantity, ledger_count, outcome, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            Uuid::new_v4().into(),
            producer.into(),
            environment_id.into(),
            usage_type.into(),
            start.into(),
            end.into(),
            producer_totals.0.into(),
            producer_totals.1.into(),
            ledger.0.into(),
            ledger.1.into(),
            if matched { "MATCH" } else { "MISMATCH" }.into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Shadow reconciliation of the gateway's legacy API counters against the ledger for
/// `day` (every environment with counters that day).
pub async fn reconcile_api_operations<C: ConnectionTrait>(
    db: &C,
    day: NaiveDate,
) -> Result<Value, ApiError> {
    let rows = db
        .query_all(stmt(
            "SELECT c.environment_id, c.q, COALESCE(f.q, 0)::bigint AS lq, COALESCE(f.n, 0)::bigint AS ln \
             FROM (SELECT environment_id, SUM(request_count)::bigint AS q FROM devplat_api_operation_usage \
                    WHERE usage_day = $1 GROUP BY environment_id) c \
             LEFT JOIN (SELECT environment_id, SUM(quantity)::bigint AS q, COUNT(*)::bigint AS n \
                          FROM devplat_usage_facts WHERE producer = $2 AND usage_day = $1 \
                          GROUP BY environment_id) f ON f.environment_id = c.environment_id",
            vec![day.into(), cf_usage_facts::producers::API_GATEWAY.into()],
        ))
        .await?;
    let start = day.and_hms_opt(0, 0, 0).expect("midnight").and_utc();
    let end = start + chrono::Duration::days(1);
    let mut results = Vec::new();
    let mut mismatches = 0;
    for r in rows {
        let env: Uuid = r.try_get("", "environment_id")?;
        let (q, lq, ln): (i64, i64, i64) = (
            r.try_get("", "q")?,
            r.try_get("", "lq")?,
            r.try_get("", "ln")?,
        );
        // The legacy counters carry no fact count: quantities are compared.
        record_reconciliation(
            db,
            cf_usage_facts::producers::API_GATEWAY,
            env,
            "API_OPERATION",
            start,
            end,
            (q, ln),
            (lq, ln),
        )
        .await?;
        if q != lq {
            mismatches += 1;
        }
        results.push(
            json!({ "environmentId": env, "legacyQuantity": q, "ledgerQuantity": lq,
                             "outcome": if q == lq { "MATCH" } else { "MISMATCH" } }),
        );
    }
    Ok(json!({ "day": day.to_string(), "results": results, "mismatches": mismatches }))
}

/// Historical API_OPERATION counters (before the ledger existed) become facts for every
/// completed day without gateway facts of that environment. Idempotent.
pub async fn backfill_api_operations(
    state: &AppState,
    before: NaiveDate,
) -> Result<Value, ApiError> {
    let producer = cf_usage_facts::producers::API_GATEWAY;
    let rows = state
        .db
        .query_all(stmt(
            "SELECT u.environment_id, u.usage_day, u.domain, u.operation_id, u.status_class, u.request_count \
             FROM devplat_api_operation_usage u \
             WHERE u.usage_day < $1 AND u.request_count > 0 AND NOT EXISTS ( \
                 SELECT 1 FROM devplat_usage_facts f WHERE f.producer = $2 AND f.environment_id = u.environment_id \
                   AND f.usage_day = u.usage_day AND f.source_fact_id NOT LIKE 'legacy:%') \
             ORDER BY u.environment_id, u.usage_day LIMIT 5000",
            vec![before.into(), producer.into()],
        ))
        .await?;
    let mut owners: HashMap<Uuid, Option<Ownership>> = HashMap::new();
    let (mut recorded, mut duplicates, mut skipped) = (0u64, 0u64, 0u64);
    for r in rows {
        let env: Uuid = r.try_get("", "environment_id")?;
        if let std::collections::hash_map::Entry::Vacant(e) = owners.entry(env) {
            e.insert(ownership(state, env).await?);
        }
        let Some(own) = owners.get(&env).cloned().flatten() else {
            skipped += 1;
            continue;
        };
        let day: NaiveDate = r.try_get("", "usage_day")?;
        let domain: String = r.try_get("", "domain")?;
        let op: String = r.try_get("", "operation_id")?;
        let class: String = r.try_get("", "status_class")?;
        let count: i64 = r.try_get("", "request_count")?;
        let key = crate::control::idempotency::digest(&[
            env.as_bytes(),
            day.to_string().as_bytes(),
            domain.as_bytes(),
            op.as_bytes(),
            class.as_bytes(),
        ]);
        let mut resource = format!("api:{domain}:{op}");
        resource.truncate(160);
        let fact = UsageFact {
            source_fact_id: format!("legacy:{}", &key[..48]),
            usage_type: "API_OPERATION".into(),
            unit: "OPERATION".into(),
            quantity: count as u64,
            tenant_id: own.tenant_id,
            application_id: own.application_id,
            environment_id: env,
            environment_kind: Some(own.kind.clone()),
            resource_ref: resource,
            occurred_at: day.and_hms_opt(0, 0, 0).expect("midnight").and_utc(),
            period_start: None,
            period_end: None,
            dimensions: [
                ("domain".to_string(), json!(domain)),
                ("operationId".to_string(), json!(op)),
                ("statusClass".to_string(), json!(class)),
            ]
            .into(),
            digest: String::new(),
        }
        .sealed();
        if fact.validate(producer).is_err() {
            skipped += 1;
            continue;
        }
        let txn = state.db.begin().await?;
        match store(&txn, producer, &fact, &own).await? {
            Outcome::Recorded => recorded += 1,
            Outcome::Duplicate => duplicates += 1,
            Outcome::Refused(_) => skipped += 1,
        }
        txn.commit().await?;
    }
    Ok(
        json!({ "recorded": recorded, "duplicates": duplicates, "skipped": skipped, "before": before.to_string() }),
    )
}

/// Recomputes the daily rollups of `[from, to]` (optionally one environment) from the
/// immutable facts.
pub async fn rebuild_rollups<C: TransactionTrait>(
    db: &C,
    from: NaiveDate,
    to: NaiveDate,
    environment_id: Option<Uuid>,
) -> Result<Value, ApiError> {
    if to < from || (to - from).num_days() > 400 {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let txn = db.begin().await?;
    let removed = txn
        .execute(stmt(
            "DELETE FROM devplat_usage_rollups_daily WHERE usage_day BETWEEN $1 AND $2 \
               AND ($3::uuid IS NULL OR environment_id = $3)",
            vec![from.into(), to.into(), environment_id.into()],
        ))
        .await?
        .rows_affected();
    let inserted = txn
        .execute(stmt(
            "INSERT INTO devplat_usage_rollups_daily (environment_id, usage_day, usage_type, unit, tenant_id, \
               application_id, environment_kind, cost_center_ref, quantity, fact_count, updated_at) \
             SELECT environment_id, usage_day, usage_type, MIN(unit), MIN(tenant_id::text)::uuid, \
                    MIN(application_id::text)::uuid, MIN(environment_kind), MAX(cost_center_ref), \
                    SUM(quantity)::bigint, COUNT(*)::bigint, $4 \
               FROM devplat_usage_facts WHERE usage_day BETWEEN $1 AND $2 \
                 AND ($3::uuid IS NULL OR environment_id = $3) \
              GROUP BY environment_id, usage_day, usage_type",
            vec![from.into(), to.into(), environment_id.into(), Utc::now().into()],
        ))
        .await?
        .rows_affected();
    txn.commit().await?;
    Ok(
        json!({ "from": from.to_string(), "to": to.to_string(), "removed": removed, "rebuilt": inserted }),
    )
}

/// Operator view of streams / rejections / reconciliations.
pub async fn operator_overview<C: ConnectionTrait>(db: &C) -> Result<Value, ApiError> {
    let q = |sql: &'static str| stmt(sql, vec![]);
    let streams = db
        .query_all(q("SELECT jsonb_build_object('producer', producer, 'stream', stream, 'positionAt', position_at, \
                        'positionKey', position_key, 'factsTotal', facts_total, 'updatedAt', updated_at) AS v \
                      FROM devplat_usage_streams ORDER BY producer, stream"))
        .await?;
    let rejections = db
        .query_all(q(
            "SELECT jsonb_build_object('producer', producer, 'sourceFactId', source_fact_id, \
                        'reasonCode', reason_code, 'receivedAt', received_at) AS v \
                      FROM devplat_usage_rejections ORDER BY received_at DESC LIMIT 100",
        ))
        .await?;
    let reconciliations = db
        .query_all(q("SELECT jsonb_build_object('producer', producer, 'environmentId', environment_id, \
                        'usageType', usage_type, 'windowStart', window_start, 'windowEnd', window_end, \
                        'producerQuantity', producer_quantity, 'producerCount', producer_count, \
                        'ledgerQuantity', ledger_quantity, 'ledgerCount', ledger_count, 'outcome', outcome, \
                        'recordedAt', recorded_at) AS v \
                      FROM devplat_usage_reconciliations ORDER BY recorded_at DESC LIMIT 200"))
        .await?;
    let v = |rows: Vec<sea_orm::QueryResult>| -> Result<Vec<Value>, ApiError> {
        rows.iter()
            .map(|r| r.try_get::<Value>("", "v").map_err(ApiError::from))
            .collect()
    };
    Ok(
        json!({ "streams": v(streams)?, "rejections": v(rejections)?, "reconciliations": v(reconciliations)? }),
    )
}

/// Resets a producer stream's watermark (historical backfill: the producer re-exports
/// its full history; duplicates are absorbed by the ledger's dedupe).
pub async fn reset_stream<C: ConnectionTrait>(
    db: &C,
    producer: &str,
    stream: &str,
) -> Result<Value, ApiError> {
    if !valid_stream(stream) || producer.len() > 64 {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let n = db
        .execute(stmt(
            "UPDATE devplat_usage_streams SET position_at = NULL, position_key = NULL, updated_at = $3 \
             WHERE producer = $1 AND stream = $2",
            vec![producer.into(), stream.into(), Utc::now().into()],
        ))
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError(ErrorCode::NotFound));
    }
    Ok(json!({ "producer": producer, "stream": stream, "position": null }))
}

/// First-party cost allocation: usage of one cost centre in `[from, to]`.
pub async fn cost_center_usage<C: ConnectionTrait>(
    db: &C,
    cost_center: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Value, ApiError> {
    if to < from || cost_center.is_empty() || cost_center.len() > 64 {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let rows = db
        .query_all(stmt(
            "SELECT jsonb_build_object('organizationId', tenant_id, 'projectId', application_id, \
               'environmentId', environment_id, 'environmentKind', environment_kind, 'usageType', usage_type, \
               'unit', unit, 'quantity', SUM(quantity)::bigint, 'factCount', SUM(fact_count)::bigint) AS v \
             FROM devplat_usage_rollups_daily WHERE cost_center_ref = $1 AND usage_day BETWEEN $2 AND $3 \
             GROUP BY tenant_id, application_id, environment_id, environment_kind, usage_type, unit \
             ORDER BY 1",
            vec![cost_center.into(), from.into(), to.into()],
        ))
        .await?;
    let items: Result<Vec<Value>, _> = rows.iter().map(|r| r.try_get::<Value>("", "v")).collect();
    Ok(
        json!({ "costCenterRef": cost_center, "from": from.to_string(), "to": to.to_string(), "items": items? }),
    )
}
