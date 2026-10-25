//! Part VI §55 usage producer: SMS_SEGMENT facts from the SMS Gateway's own immutable
//! usage records (`sms_usage_records`), exported to the Developer Platform's unified
//! usage ledger by the shared exporter (`cf_usage_facts::exporter`).
//!
//! * One fact per transport (`source_fact_id` = the usage record id): segment count,
//!   direction and delivery category (SUBMITTED / RECEIVED). No number, address,
//!   content or provider id leaves the SMS Gateway; cost evidence stays here.
//! * VOIDED records (never submitted) are not exported.
//! * Durable: the Developer Platform holds the watermark; a restart resumes after the
//!   last acknowledged batch, a reset watermark replays the history (backfill).
//! * Off unless `CFMEET_USAGE_EXPORT_ENABLED=true` and the Developer Platform is a
//!   configured target.

use std::collections::BTreeMap;

use async_trait::async_trait;
use cf_usage_facts::{
    exporter::{EventSource, Position, Total},
    UsageFact,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

pub const STREAM: &str = "sms.segments";

pub struct SmsSegments {
    db: DatabaseConnection,
}

impl SmsSegments {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

fn err(e: sea_orm::DbErr) -> String {
    format!("usage source unavailable: {e}")
        .chars()
        .take(200)
        .collect()
}

#[async_trait]
impl EventSource for SmsSegments {
    fn stream(&self) -> &'static str {
        STREAM
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
                "SELECT id, tenant_id, application_id, environment_id, direction, segments, created_at \
                 FROM sms_usage_records WHERE status <> 'VOIDED' \
                   AND (created_at, id::text) > ($1, $2) AND created_at <= $3 \
                 ORDER BY created_at, id::text LIMIT $4",
                [at.into(), key.into(), until.into(), (limit as i64).into()],
            ))
            .await
            .map_err(err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: Uuid = r.try_get("", "id").map_err(err)?;
            let created: DateTime<Utc> = r.try_get("", "created_at").map_err(err)?;
            let direction: String = r.try_get("", "direction").map_err(err)?;
            let segments: i16 = r.try_get("", "segments").map_err(err)?;
            let mut dimensions = BTreeMap::new();
            dimensions.insert(
                "deliveryCategory".to_string(),
                json!(if direction == "INBOUND" {
                    "RECEIVED"
                } else {
                    "SUBMITTED"
                }),
            );
            dimensions.insert("direction".to_string(), json!(direction));
            let fact = UsageFact {
                source_fact_id: format!("sms_usage:{id}"),
                usage_type: "SMS_SEGMENT".into(),
                unit: "SEGMENT".into(),
                quantity: segments.max(0) as u64,
                tenant_id: r.try_get("", "tenant_id").map_err(err)?,
                application_id: r.try_get("", "application_id").map_err(err)?,
                environment_id: r.try_get("", "environment_id").map_err(err)?,
                environment_kind: None,
                resource_ref: format!("sms:{id}"),
                occurred_at: created,
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
                "SELECT environment_id, COALESCE(SUM(segments), 0)::bigint AS q, COUNT(*)::bigint AS n \
                 FROM sms_usage_records WHERE status <> 'VOIDED' AND created_at >= $1 AND created_at < $2 \
                 GROUP BY environment_id",
                [start.into(), end.into()],
            ))
            .await
            .map_err(err)?;
        rows.iter()
            .map(|r| {
                Ok(Total {
                    environment_id: r.try_get("", "environment_id").map_err(err)?,
                    usage_type: "SMS_SEGMENT",
                    quantity: r.try_get::<i64>("", "q").map_err(err)?.max(0) as u64,
                    count: r.try_get::<i64>("", "n").map_err(err)?.max(0) as u64,
                })
            })
            .collect()
    }
}
