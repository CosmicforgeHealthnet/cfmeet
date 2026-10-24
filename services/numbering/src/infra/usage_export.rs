//! Part VI §55 usage producer: NUMBER_ACTIVE facts — the active duration of every DCN
//! assignment, metered per completed UTC day from Numbering's own assignment records
//! (`activated_at` until `closed_at` / release), exported to the Developer Platform's
//! unified usage ledger by the shared exporter.
//!
//! Each fact carries the assignment's safe reference and its number type, country and
//! capability CLASSES only — never the number itself, a provider id or provider cost.
//! Off unless `CFMEET_USAGE_EXPORT_ENABLED=true` and the Developer Platform is a
//! configured target.

use std::collections::BTreeMap;

use async_trait::async_trait;
use cf_usage_facts::exporter::{ActiveInterval, IntervalSource};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

pub const STREAM: &str = "numbers.active";

pub struct ActiveNumbers {
    db: DatabaseConnection,
}

impl ActiveNumbers {
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

fn class(v: &str) -> String {
    let c: String = v
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        .take(64)
        .collect();
    if c.is_empty() {
        "UNCLASSIFIED".into()
    } else {
        c
    }
}

#[async_trait]
impl IntervalSource for ActiveNumbers {
    fn stream(&self) -> &'static str {
        STREAM
    }

    async fn earliest(&self) -> Result<Option<DateTime<Utc>>, String> {
        let row = self
            .db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT MIN(activated_at) AS t FROM dcn_assignments WHERE activated_at IS NOT NULL",
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
                "SELECT a.id, a.tenant_id, a.application_id, a.environment_id, a.environment_kind, \
                        a.activated_at, COALESCE(a.closed_at, a.release_requested_at) AS until, \
                        array_to_string(ARRAY(SELECT unnest(a.capability_snapshot) ORDER BY 1), '.') AS caps, \
                        r.number_type, r.country_code::text AS country \
                 FROM dcn_assignments a JOIN number_resources r ON r.id = a.number_resource_id \
                 WHERE a.activated_at IS NOT NULL AND a.activated_at < $2 \
                   AND (COALESCE(a.closed_at, a.release_requested_at) IS NULL \
                        OR COALESCE(a.closed_at, a.release_requested_at) > $1)",
                [start.into(), end.into()],
            ))
            .await
            .map_err(err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: Uuid = r.try_get("", "id").map_err(err)?;
            let mut dimensions = BTreeMap::new();
            let caps: Option<String> = r.try_get("", "caps").map_err(err)?;
            dimensions.insert(
                "capabilityClass".to_string(),
                json!(class(caps.as_deref().unwrap_or_default())),
            );
            dimensions.insert(
                "countryClass".to_string(),
                json!(class(&r.try_get::<String>("", "country").map_err(err)?)),
            );
            dimensions.insert(
                "numberType".to_string(),
                json!(class(&r.try_get::<String>("", "number_type").map_err(err)?)),
            );
            out.push(ActiveInterval {
                resource_ref: format!("dcn_assignment:{id}"),
                usage_type: "NUMBER_ACTIVE",
                tenant_id: r.try_get("", "tenant_id").map_err(err)?,
                application_id: r.try_get("", "application_id").map_err(err)?,
                environment_id: r.try_get("", "environment_id").map_err(err)?,
                environment_kind: Some(r.try_get("", "environment_kind").map_err(err)?),
                active_from: r.try_get("", "activated_at").map_err(err)?,
                active_until: r.try_get("", "until").map_err(err)?,
                weight: 1,
                dimensions,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn classes_are_safe_dimension_values() {
        assert_eq!(super::class("SMS.VOICE"), "SMS.VOICE");
        assert_eq!(super::class("+1 (555)"), "1555");
        assert_eq!(super::class(""), "UNCLASSIFIED");
    }
}
