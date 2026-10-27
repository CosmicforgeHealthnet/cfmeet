//! Part VI §55 usage producer: Calling's facts for the Developer Platform's unified usage
//! ledger (exported by the shared `cf_usage_facts` exporter).
//!
//! * [`CallFacts`] (event stream over the authoritative, immutable `call_usage_records`):
//!   `CALL_CONNECTED_TIME` → CALL_MINUTE_APP, `LEG_CONNECTED_TIME` →
//!   CALL_MINUTE_PUBLIC_NETWORK (route class, destination country, billable seconds and
//!   provider-independent cost evidence — the cost stays internal in the ledger),
//!   `RECORDING_ARTIFACT` → RECORDING_MINUTE. Participant / session facts stay Calling's
//!   detail; voicemail duration is storage only.
//! * [`ArtifactStorage`] (interval stream): STORAGE_BYTE_TIME of READY call recordings and
//!   voicemail messages until their deletion.
//!
//! Only safe refs (`call:<id>`, `call_leg:<id>`, `call_recording:<id>`, `voicemail:<id>`)
//! leave Calling: never a number, provider call id, object key or media. Off unless
//! `CFMEET_USAGE_EXPORT_ENABLED=true` and the Developer Platform is a configured target.

use std::collections::BTreeMap;

use async_trait::async_trait;
use cf_usage_facts::{
    exporter::{ActiveInterval, EventSource, IntervalSource, Position, Total},
    UsageFact,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

fn err(e: sea_orm::DbErr) -> String {
    format!("usage source unavailable: {e}")
        .chars()
        .take(200)
        .collect()
}

/// `(usage type of the ledger, recorded type)`.
const MAPPING: &str = "CASE usage_type WHEN 'CALL_CONNECTED_TIME' THEN 'CALL_MINUTE_APP' \
     WHEN 'LEG_CONNECTED_TIME' THEN 'CALL_MINUTE_PUBLIC_NETWORK' \
     WHEN 'RECORDING_ARTIFACT' THEN 'RECORDING_MINUTE' END";
const EXPORTED: &str =
    "usage_type IN ('CALL_CONNECTED_TIME', 'LEG_CONNECTED_TIME', 'RECORDING_ARTIFACT') \
     AND finalized_at IS NOT NULL";

pub struct CallFacts {
    db: DatabaseConnection,
}

impl CallFacts {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

fn safe_code(v: &str) -> Option<String> {
    let c: String = v
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(64)
        .collect();
    (!c.is_empty()).then_some(c)
}

#[async_trait]
impl EventSource for CallFacts {
    fn stream(&self) -> &'static str {
        "calling.usage"
    }

    async fn facts_after(
        &self,
        after: Option<&Position>,
        until: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<(Position, UsageFact)>, String> {
        let (at, key) = after
            .map(|p| (p.at, p.key.clone()))
            .unwrap_or((DateTime::<Utc>::UNIX_EPOCH, String::new()));
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT id, call_id, leg_id, tenant_id, application_id, environment_id, usage_type, \
                            {MAPPING} AS ledger_type, quantity, finalized_at, route_class, \
                            destination_country::text AS destination_country, billable_seconds, \
                            cost_status, cost_minor, cost_currency::text AS cost_currency \
                     FROM call_usage_records WHERE {EXPORTED} \
                       AND (finalized_at, id::text) > ($1, $2) AND finalized_at <= $3 \
                     ORDER BY finalized_at, id::text LIMIT $4"
                ),
                [at.into(), key.into(), until.into(), (limit as i64).into()],
            ))
            .await
            .map_err(err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: Uuid = r.try_get("", "id").map_err(err)?;
            let call: Uuid = r.try_get("", "call_id").map_err(err)?;
            let recorded: String = r.try_get("", "usage_type").map_err(err)?;
            let ledger: String = r.try_get("", "ledger_type").map_err(err)?;
            let mut dimensions = BTreeMap::new();
            dimensions.insert("callId".to_string(), json!(call.to_string()));
            let resource = match recorded.as_str() {
                "LEG_CONNECTED_TIME" => {
                    let leg: Option<Uuid> = r.try_get("", "leg_id").map_err(err)?;
                    for (k, col) in [
                        ("routeClass", "route_class"),
                        ("destinationCountry", "destination_country"),
                        ("costEvidence", "cost_status"),
                        ("costCurrency", "cost_currency"),
                    ] {
                        if let Some(v) = r
                            .try_get::<Option<String>>("", col)
                            .map_err(err)?
                            .as_deref()
                            .and_then(safe_code)
                        {
                            dimensions.insert(k.to_string(), json!(v));
                        }
                    }
                    if let Some(s) = r
                        .try_get::<Option<i32>>("", "billable_seconds")
                        .map_err(err)?
                    {
                        dimensions.insert("billableSeconds".to_string(), json!(s.max(0)));
                    }
                    if let Some(c) = r.try_get::<Option<i64>>("", "cost_minor").map_err(err)? {
                        dimensions.insert("costMinor".to_string(), json!(c.max(0)));
                    }
                    format!("call_leg:{}", leg.unwrap_or(call))
                }
                "RECORDING_ARTIFACT" => {
                    dimensions.remove("callId");
                    dimensions.insert("recordingDomain".to_string(), json!("CALL"));
                    format!("call_recording_usage:{id}")
                }
                _ => format!("call:{call}"),
            };
            let finalized: DateTime<Utc> = r.try_get("", "finalized_at").map_err(err)?;
            let fact = UsageFact {
                source_fact_id: format!("call_usage:{id}"),
                usage_type: ledger,
                unit: "MILLISECOND".into(),
                quantity: r.try_get::<i64>("", "quantity").map_err(err)?.max(0) as u64,
                tenant_id: r.try_get("", "tenant_id").map_err(err)?,
                application_id: r.try_get("", "application_id").map_err(err)?,
                environment_id: r.try_get("", "environment_id").map_err(err)?,
                environment_kind: None,
                resource_ref: resource,
                occurred_at: finalized,
                period_start: None,
                period_end: None,
                dimensions,
                digest: String::new(),
            }
            .normalized()
            .sealed();
            out.push((
                Position {
                    at: fact.occurred_at,
                    key: id.to_string(),
                },
                fact,
            ));
        }
        Ok(out)
    }

    async fn totals(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<Total>, String> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT environment_id, {MAPPING} AS t, COALESCE(SUM(quantity), 0)::bigint AS q, \
                            COUNT(*)::bigint AS n \
                     FROM call_usage_records WHERE {EXPORTED} AND finalized_at >= $1 AND finalized_at < $2 \
                     GROUP BY environment_id, 2"
                ),
                [start.into(), end.into()],
            ))
            .await
            .map_err(err)?;
        let mut out = Vec::new();
        for r in rows {
            let t: String = r.try_get("", "t").map_err(err)?;
            let Some(def) = cf_usage_facts::definition(&t) else {
                continue;
            };
            out.push(Total {
                environment_id: r.try_get("", "environment_id").map_err(err)?,
                usage_type: def.name,
                quantity: r.try_get::<i64>("", "q").map_err(err)?.max(0) as u64,
                count: r.try_get::<i64>("", "n").map_err(err)?.max(0) as u64,
            });
        }
        Ok(out)
    }
}

/// STORAGE_BYTE_TIME of READY recordings and voicemail messages.
pub struct ArtifactStorage {
    db: DatabaseConnection,
}

impl ArtifactStorage {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

const ARTIFACTS: &str =
    "SELECT 'call_recording' AS kind, 'CALL_RECORDING' AS domain, id, tenant_id, \
        application_id, environment_id, ready_at, deleted_at, size_bytes \
     FROM call_recordings WHERE ready_at IS NOT NULL AND size_bytes IS NOT NULL \
     UNION ALL \
     SELECT 'voicemail', 'VOICEMAIL', id, tenant_id, application_id, environment_id, ready_at, \
        deleted_at, size_bytes \
     FROM voicemail_messages WHERE ready_at IS NOT NULL AND size_bytes IS NOT NULL";

#[async_trait]
impl IntervalSource for ArtifactStorage {
    fn stream(&self) -> &'static str {
        "calling.artifact-storage"
    }

    async fn earliest(&self) -> Result<Option<DateTime<Utc>>, String> {
        let row = self
            .db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT MIN(ready_at) AS t FROM ({ARTIFACTS}) a"),
            ))
            .await
            .map_err(err)?;
        Ok(row.and_then(|r| r.try_get::<Option<DateTime<Utc>>>("", "t").ok().flatten()))
    }

    async fn active(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<ActiveInterval>, String> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT * FROM ({ARTIFACTS}) a WHERE a.ready_at < $2 \
                       AND (a.deleted_at IS NULL OR a.deleted_at > $1)"
                ),
                [start.into(), end.into()],
            ))
            .await
            .map_err(err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: Uuid = r.try_get("", "id").map_err(err)?;
            let kind: String = r.try_get("", "kind").map_err(err)?;
            let domain: String = r.try_get("", "domain").map_err(err)?;
            let bytes = r.try_get::<i64>("", "size_bytes").map_err(err)?.max(0) as u64;
            let mut dimensions = BTreeMap::new();
            dimensions.insert("storageDomain".to_string(), json!(domain));
            dimensions.insert("storedBytes".to_string(), json!(bytes));
            out.push(ActiveInterval {
                resource_ref: format!("{kind}:{id}"),
                usage_type: "STORAGE_BYTE_TIME",
                tenant_id: r.try_get("", "tenant_id").map_err(err)?,
                application_id: r.try_get("", "application_id").map_err(err)?,
                environment_id: r.try_get("", "environment_id").map_err(err)?,
                environment_kind: None,
                active_from: r.try_get("", "ready_at").map_err(err)?,
                active_until: r.try_get("", "deleted_at").map_err(err)?,
                weight: bytes,
                dimensions,
            });
        }
        Ok(out)
    }
}
