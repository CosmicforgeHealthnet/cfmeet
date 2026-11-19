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

/// ADR 0062 / build contract §5: the `MESSAGE_SENT` stream (one OPERATION fact per
/// APPLICATION message accepted in an EXTERNAL environment; dimensions `contextClass`
/// and `securityProfile` only). Exported only with `MESSAGING_MESSAGE_SENT_EXPORT_ENABLED`
/// and once the usage catalogue knows the type ([`MessageSent::catalogued`]), so a fact is
/// never sent before the Developer Platform can accept it.
pub const MESSAGE_SENT_STREAM: &str = "messaging.message-sent";
/// The usage type name (packages/usage-facts catalogue).
pub const MESSAGE_SENT: &str = "MESSAGE_SENT";

pub struct MessageSent {
    db: DatabaseConnection,
}

impl MessageSent {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// Whether this build's usage catalogue knows `MESSAGE_SENT` (unit OPERATION,
    /// producer Messaging).
    pub fn catalogued() -> bool {
        cf_usage_facts::definition(MESSAGE_SENT).is_some_and(|d| {
            d.unit == cf_usage_facts::Unit::Operation
                && d.producers.contains(&cf_usage_facts::producers::MESSAGING)
        })
    }
}

/// The fact of one recorded message.
#[allow(clippy::too_many_arguments)]
pub fn message_sent_fact(
    message_id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    environment_kind: &str,
    context_class: &str,
    security_profile: &str,
    occurred_at: DateTime<Utc>,
) -> cf_usage_facts::UsageFact {
    let mut dimensions = BTreeMap::new();
    dimensions.insert("contextClass".to_string(), json!(context_class));
    dimensions.insert("securityProfile".to_string(), json!(security_profile));
    cf_usage_facts::UsageFact {
        source_fact_id: format!("message-sent:{}", message_id.simple()),
        usage_type: MESSAGE_SENT.to_string(),
        unit: cf_usage_facts::Unit::Operation.as_str().to_string(),
        quantity: 1,
        tenant_id,
        application_id,
        environment_id,
        environment_kind: Some(environment_kind.to_string()),
        resource_ref: format!("message:{message_id}"),
        occurred_at,
        period_start: None,
        period_end: None,
        dimensions,
        digest: String::new(),
    }
    .normalized()
    .sealed()
}

#[async_trait]
impl cf_usage_facts::exporter::EventSource for MessageSent {
    fn stream(&self) -> &'static str {
        MESSAGE_SENT_STREAM
    }

    async fn facts_after(
        &self,
        after: Option<&cf_usage_facts::exporter::Position>,
        until: DateTime<Utc>,
        limit: usize,
    ) -> Result<
        Vec<(
            cf_usage_facts::exporter::Position,
            cf_usage_facts::UsageFact,
        )>,
        String,
    > {
        let (at, key) = match after {
            Some(p) => (Some(p.at), Some(p.key.clone())),
            None => (None, None),
        };
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT message_id, tenant_id, application_id, environment_id, environment_kind, context_class, \
                        security_profile, occurred_at \
                   FROM messaging_message_sent_facts \
                  WHERE occurred_at <= $1 \
                    AND ($2::timestamptz IS NULL OR (occurred_at, message_id::text) > ($2, $3)) \
                  ORDER BY occurred_at, message_id::text LIMIT $4",
                [until.into(), at.into(), key.into(), (limit.max(1) as i64).into()],
            ))
            .await
            .map_err(err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: Uuid = r.try_get("", "message_id").map_err(err)?;
            let occurred: DateTime<Utc> = r.try_get("", "occurred_at").map_err(err)?;
            let fact = message_sent_fact(
                id,
                r.try_get("", "tenant_id").map_err(err)?,
                r.try_get("", "application_id").map_err(err)?,
                r.try_get("", "environment_id").map_err(err)?,
                &r.try_get::<String>("", "environment_kind").map_err(err)?,
                &r.try_get::<String>("", "context_class").map_err(err)?,
                &r.try_get::<String>("", "security_profile").map_err(err)?,
                occurred,
            );
            out.push((
                cf_usage_facts::exporter::Position {
                    at: occurred,
                    key: id.to_string(),
                },
                fact,
            ));
        }
        Ok(out)
    }

    async fn totals(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<cf_usage_facts::exporter::Total>, String> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT environment_id, count(*) AS n FROM messaging_message_sent_facts \
                  WHERE occurred_at >= $1 AND occurred_at < $2 GROUP BY environment_id ORDER BY environment_id",
                [start.into(), end.into()],
            ))
            .await
            .map_err(err)?;
        rows.iter()
            .map(|r| {
                let n = r.try_get::<i64>("", "n").map_err(err)?.max(0) as u64;
                Ok(cf_usage_facts::exporter::Total {
                    environment_id: r.try_get("", "environment_id").map_err(err)?,
                    usage_type: MESSAGE_SENT,
                    quantity: n,
                    count: n,
                })
            })
            .collect()
    }
}
