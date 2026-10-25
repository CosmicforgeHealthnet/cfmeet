//! Part VI §55 usage producer: STORAGE_BYTE_TIME facts of Messaging attachments — the
//! committed bytes of every attachment that became READY, stored from its finalization
//! until it was deleted / expired, metered per completed UTC day and exported to the
//! Developer Platform's unified usage ledger by the shared exporter.
//!
//! Only the attachment's safe reference and byte count leave Messaging: never an object
//! key, file name, media metadata, ciphertext or E2EE material. Off unless
//! `CFMEET_USAGE_EXPORT_ENABLED=true` and the Developer Platform is a configured target.

use std::collections::BTreeMap;

use async_trait::async_trait;
use cf_usage_facts::exporter::{ActiveInterval, IntervalSource};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

pub const STREAM: &str = "messaging.attachment-storage";

pub struct AttachmentStorage {
    db: DatabaseConnection,
}

impl AttachmentStorage {
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

/// The end of an attachment's storage interval (`None`: still stored).
const UNTIL: &str = "CASE WHEN state IN ('DELETED', 'EXPIRED', 'REJECTED', 'QUARANTINED') \
                     THEN state_changed_at END";

#[async_trait]
impl IntervalSource for AttachmentStorage {
    fn stream(&self) -> &'static str {
        STREAM
    }

    async fn earliest(&self) -> Result<Option<DateTime<Utc>>, String> {
        let row = self
            .db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT MIN(finalized_at) AS t FROM messaging_attachments WHERE finalized_at IS NOT NULL",
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
                    "SELECT id, tenant_id, application_id, environment_id, finalized_at, \
                            {UNTIL} AS until, GREATEST(committed_bytes, COALESCE(verified_size, 0)) AS bytes \
                     FROM messaging_attachments \
                     WHERE finalized_at IS NOT NULL AND finalized_at < $2 \
                       AND ({UNTIL} IS NULL OR {UNTIL} > $1)"
                ),
                [start.into(), end.into()],
            ))
            .await
            .map_err(err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: Uuid = r.try_get("", "id").map_err(err)?;
            let bytes = r.try_get::<i64>("", "bytes").map_err(err)?.max(0) as u64;
            if bytes == 0 {
                continue;
            }
            let mut dimensions = BTreeMap::new();
            dimensions.insert("storageDomain".to_string(), json!("MESSAGING_ATTACHMENT"));
            dimensions.insert("storedBytes".to_string(), json!(bytes));
            out.push(ActiveInterval {
                resource_ref: format!("attachment:{id}"),
                usage_type: "STORAGE_BYTE_TIME",
                tenant_id: r.try_get("", "tenant_id").map_err(err)?,
                application_id: r.try_get("", "application_id").map_err(err)?,
                environment_id: r.try_get("", "environment_id").map_err(err)?,
                environment_kind: None,
                active_from: r.try_get("", "finalized_at").map_err(err)?,
                active_until: r.try_get("", "until").map_err(err)?,
                weight: bytes,
                dimensions,
            });
        }
        Ok(out)
    }
}
