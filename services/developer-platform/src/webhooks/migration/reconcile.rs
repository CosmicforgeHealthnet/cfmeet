//! Reconciliation: Calling's inventory and per-entity digests against the same digests
//! over the migrated rows here (algorithm `sha256-canonical-lines-v1`, identical canonical
//! lines on both sides), and — for every entity whose digest differs — a record-level
//! comparison naming each missing, extra or different record and the differing fields.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    exact, exact_opt,
    port::{records, CallingAttempt, CallingDelivery, CallingEvent, CallingSubscription, Entity},
    source_error, Migration, MAX_LISTED,
};
use crate::{control::stmt, http::error::ApiError};

pub const DIGEST_ALGORITHM: &str = "sha256-canonical-lines-v1";

/// Field names of each canonical line (for precise drift reports).
fn fields(entity: Entity) -> &'static [&'static str] {
    match entity {
        Entity::Subscriptions => &[
            "webhookId",
            "tenantId",
            "applicationId",
            "environmentId",
            "url",
            "description",
            "eventTypes",
            "status",
            "secretVersion",
            "version",
            "createdAt",
            "updatedAt",
            "deletedAt",
        ],
        Entity::Events => &[
            "eventId",
            "producer",
            "sourceEventId",
            "eventType",
            "tenantId",
            "applicationId",
            "environmentId",
            "occurredAt",
            "createdAt",
            "bodySha256",
        ],
        Entity::Deliveries => &[
            "deliveryId",
            "webhookId",
            "eventId",
            "replayOf",
            "status",
            "attemptCount",
            "nextAttemptAt",
            "lastHttpStatus",
            "failureCategory",
            "createdAt",
            "completedAt",
        ],
        Entity::Attempts => &[
            "deliveryId",
            "attemptNumber",
            "outcome",
            "httpStatus",
            "failureCategory",
            "startedAt",
            "finishedAt",
        ],
    }
}

/// A canonical line as a JSON array (its key is the first element, or the first two for
/// attempts).
type Line = Value;

fn key_of(entity: Entity, line: &Line) -> String {
    match entity {
        // Zero-padded attempt number: key order is (delivery, attempt) order (Calling's).
        Entity::Attempts => format!(
            "{}:{:02}",
            line[0].as_str().unwrap_or(""),
            line[1].as_i64().unwrap_or(0)
        ),
        _ => line[0].as_str().unwrap_or("").to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn subscription_line(
    id: Uuid,
    scope: (Uuid, Uuid, Uuid),
    url: &str,
    description: &Option<String>,
    types: &[String],
    status: &str,
    secret_version: i32,
    version: i64,
    at: (DateTime<Utc>, DateTime<Utc>, Option<DateTime<Utc>>),
) -> Line {
    let mut types = types.to_vec();
    types.sort();
    json!([
        id,
        scope.0,
        scope.1,
        scope.2,
        url,
        description,
        types,
        status,
        secret_version,
        version,
        exact(at.0),
        exact(at.1),
        exact_opt(at.2),
    ])
}

#[derive(Debug, FromQueryResult)]
struct SubRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    url: String,
    description: Option<String>,
    event_types: Vec<String>,
    status: String,
    secret_version: i32,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromQueryResult)]
struct EventRow {
    event_id: String,
    producer: String,
    source_event_id: String,
    event_type: String,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    occurred_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
    body_sha256: String,
}

#[derive(Debug, FromQueryResult)]
struct DeliveryRow {
    id: Uuid,
    subscription_id: Uuid,
    event_id: String,
    replay_of: Option<Uuid>,
    status: String,
    attempt_count: i32,
    next_attempt_at: DateTime<Utc>,
    last_http_status: Option<i32>,
    failure_category: Option<String>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromQueryResult)]
struct AttemptRow {
    delivery_id: Uuid,
    attempt_number: i32,
    outcome: String,
    http_status: Option<i32>,
    error_class: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
}

/// The canonical lines of the migrated rows here, in canonical (key) order. A subscription
/// awaiting a secret rotation is compared with Calling's status (its DISABLED state here is
/// the expected effect of the migration, not drift).
pub(crate) async fn local_lines<C: ConnectionTrait>(
    db: &C,
    entity: Entity,
) -> Result<BTreeMap<String, Line>, ApiError> {
    let mut out = BTreeMap::new();
    match entity {
        Entity::Subscriptions => {
            for r in SubRow::find_by_statement(stmt(
                "SELECT s.id, s.tenant_id, s.application_id, s.environment_id, s.url, s.description, \
                        s.event_types, \
                        CASE WHEN s.secret_state = 'ROTATION_REQUIRED' \
                              AND s.disabled_reason = 'SECRET_ROTATION_REQUIRED' \
                              AND s.version = m.imported_version \
                             THEN m.calling_status ELSE s.status END AS status, \
                        s.secret_version, s.version, s.created_at, s.updated_at, s.deleted_at \
                   FROM devplat_webhook_subscriptions s \
                   JOIN devplat_webhook_migration_subscriptions m ON m.subscription_id = s.id \
                  ORDER BY s.id",
                vec![],
            ))
            .all(db)
            .await?
            {
                let line = subscription_line(
                    r.id,
                    (r.tenant_id, r.application_id, r.environment_id),
                    &r.url,
                    &r.description,
                    &r.event_types,
                    &r.status,
                    r.secret_version,
                    r.version,
                    (r.created_at, r.updated_at, r.deleted_at),
                );
                out.insert(r.id.to_string(), line);
            }
        }
        Entity::Events => {
            for r in EventRow::find_by_statement(stmt(
                "SELECT event_id, producer, source_event_id, event_type, tenant_id, application_id, \
                        environment_id, occurred_at, received_at, \
                        encode(sha256(convert_to(payload::text, 'UTF8')), 'hex') AS body_sha256 \
                   FROM devplat_webhook_events WHERE origin = 'MIGRATED_FROM_CALLING' ORDER BY event_id COLLATE \"C\"",
                vec![],
            ))
            .all(db)
            .await?
            {
                let line = json!([
                    r.event_id,
                    r.producer,
                    r.source_event_id,
                    r.event_type,
                    r.tenant_id,
                    r.application_id,
                    r.environment_id,
                    exact(r.occurred_at),
                    exact(r.received_at),
                    r.body_sha256,
                ]);
                out.insert(r.event_id, line);
            }
        }
        Entity::Deliveries => {
            for r in DeliveryRow::find_by_statement(stmt(
                "SELECT id, subscription_id, event_id, replay_of, status, attempt_count, next_attempt_at, \
                        last_http_status, failure_category, created_at, completed_at \
                   FROM devplat_webhook_deliveries WHERE origin = 'MIGRATED_FROM_CALLING' ORDER BY id",
                vec![],
            ))
            .all(db)
            .await?
            {
                out.insert(
                    r.id.to_string(),
                    json!([
                        r.id,
                        r.subscription_id,
                        r.event_id,
                        r.replay_of,
                        r.status,
                        r.attempt_count,
                        exact(r.next_attempt_at),
                        r.last_http_status,
                        r.failure_category,
                        exact(r.created_at),
                        exact_opt(r.completed_at),
                    ]),
                );
            }
        }
        Entity::Attempts => {
            for r in AttemptRow::find_by_statement(stmt(
                "SELECT a.delivery_id, a.attempt_number, a.outcome, a.http_status, a.error_class, \
                        a.started_at, a.finished_at \
                   FROM devplat_webhook_attempts a \
                   JOIN devplat_webhook_deliveries d ON d.id = a.delivery_id \
                  WHERE d.origin = 'MIGRATED_FROM_CALLING' ORDER BY a.delivery_id, a.attempt_number",
                vec![],
            ))
            .all(db)
            .await?
            {
                let line = json!([
                    r.delivery_id,
                    r.attempt_number,
                    r.outcome,
                    r.http_status,
                    r.error_class,
                    exact(r.started_at),
                    exact(r.finished_at),
                ]);
                out.insert(key_of(Entity::Attempts, &line), line);
            }
        }
    }
    Ok(out)
}

/// Canonical lines of one Calling export page.
fn export_lines(entity: Entity, page: &super::port::ExportPage) -> Result<Vec<Line>, ApiError> {
    Ok(match entity {
        Entity::Subscriptions => records::<CallingSubscription>(page)
            .map_err(source_error)?
            .iter()
            .map(|s| {
                subscription_line(
                    s.webhook_id,
                    (s.tenant_id, s.application_id, s.environment_id),
                    &s.url,
                    &s.description,
                    &s.event_types,
                    &s.status,
                    s.secret_version,
                    s.version,
                    (s.created_at, s.updated_at, s.deleted_at),
                )
            })
            .collect(),
        Entity::Events => records::<CallingEvent>(page)
            .map_err(source_error)?
            .iter()
            .map(|e| {
                json!([
                    e.event_id,
                    e.producer,
                    e.source_event_id,
                    e.event_type,
                    e.tenant_id,
                    e.application_id,
                    e.environment_id,
                    exact(e.occurred_at),
                    exact(e.created_at),
                    e.body_sha256,
                ])
            })
            .collect(),
        Entity::Deliveries => records::<CallingDelivery>(page)
            .map_err(source_error)?
            .iter()
            .map(|d| {
                json!([
                    d.delivery_id,
                    d.webhook_id,
                    d.event_id,
                    d.replay_of,
                    d.status,
                    d.attempt_count,
                    exact(d.next_attempt_at),
                    d.last_http_status,
                    d.failure_category,
                    exact(d.created_at),
                    exact_opt(d.completed_at),
                ])
            })
            .collect(),
        Entity::Attempts => records::<CallingAttempt>(page)
            .map_err(source_error)?
            .iter()
            .map(|a| {
                json!([
                    a.delivery_id,
                    a.attempt_number,
                    a.outcome,
                    a.http_status,
                    a.failure_category,
                    exact(a.started_at),
                    exact(a.finished_at),
                ])
            })
            .collect(),
    })
}

/// sha256 over `line + "\n"` in key order, with the count.
pub(crate) fn digest(lines: &BTreeMap<String, Line>) -> Value {
    let text: Vec<String> = lines.values().map(Value::to_string).collect();
    json!({
        "count": lines.len(),
        "digest": crate::digest::sha256_lines(text.iter().map(String::as_str)),
    })
}

impl Migration<'_> {
    /// Every Calling record of `entity` as canonical lines keyed like [`local_lines`].
    pub(crate) async fn calling_lines(
        &self,
        entity: Entity,
    ) -> Result<BTreeMap<String, Line>, ApiError> {
        let mut out = BTreeMap::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .calling
                .page(entity, cursor.as_deref(), 500, false)
                .await
                .map_err(source_error)?;
            for line in export_lines(entity, &page)? {
                out.insert(key_of(entity, &line), line);
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    /// Calling vs here: inventory, per-entity digests and, where a digest differs, every
    /// drifted record (bounded list) with the fields that differ.
    pub async fn reconcile(&self) -> Result<Value, ApiError> {
        let calling_inventory = self.calling_inventory().await?;
        let calling_digest = self.calling.digest().await.map_err(source_error)?;
        let mut digests = serde_json::Map::new();
        let mut drift = Vec::new();
        let mut drift_count = 0usize;
        let mut clean =
            calling_digest.get("algorithm").and_then(Value::as_str) == Some(DIGEST_ALGORITHM);
        for entity in Entity::ALL {
            let local = local_lines(self.db, entity).await?;
            let ours = digest(&local);
            let theirs = calling_digest
                .get(entity.as_str())
                .cloned()
                .unwrap_or(Value::Null);
            let matches = ours == theirs;
            digests.insert(
                entity.as_str().to_string(),
                json!({ "calling": theirs, "developerPlatform": ours, "match": matches }),
            );
            if matches {
                continue;
            }
            clean = false;
            let remote = self.calling_lines(entity).await?;
            let mut push = |v: Value| {
                drift_count += 1;
                if drift.len() < MAX_LISTED {
                    drift.push(v);
                }
            };
            for (k, line) in &remote {
                match local.get(k) {
                    None => push(
                        json!({ "entity": entity.as_str(), "id": k, "kind": "MISSING_IN_DEVELOPER_PLATFORM" }),
                    ),
                    Some(l) if l != line => {
                        let names = fields(entity);
                        let differing: Vec<&str> = (0..names.len())
                            .filter(|i| l[*i] != line[*i])
                            .map(|i| names[i])
                            .collect();
                        push(
                            json!({ "entity": entity.as_str(), "id": k, "kind": "DIFFERENT", "fields": differing }),
                        );
                    }
                    Some(_) => {}
                }
            }
            for k in local.keys().filter(|k| !remote.contains_key(*k)) {
                push(
                    json!({ "entity": entity.as_str(), "id": k, "kind": "EXTRA_IN_DEVELOPER_PLATFORM" }),
                );
            }
        }
        let local_counts: serde_json::Map<String, Value> = digests
            .iter()
            .map(|(k, v)| (k.clone(), v["developerPlatform"]["count"].clone()))
            .collect();
        tracing::info!(operator = %self.operator, clean, drift = drift_count, "webhook migration reconcile");
        Ok(json!({
            "clean": clean,
            "algorithm": DIGEST_ALGORITHM,
            "digests": digests,
            "inventory": {
                "calling": {
                    "subscriptions": calling_inventory.pointer("/subscriptions/total"),
                    "events": calling_inventory.pointer("/events/total"),
                    "deliveries": calling_inventory.pointer("/deliveries/total"),
                    "attempts": calling_inventory.pointer("/attempts/total"),
                },
                "developerPlatform": local_counts,
            },
            "driftCount": drift_count,
            "drift": drift,
        }))
    }
}
