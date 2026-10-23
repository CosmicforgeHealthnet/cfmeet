//! Event ingestion: a committed producer envelope (`cfmeet.platform.event-envelope` v1)
//! becomes at most ONE public event and one delivery per matching subscription.
//!
//! * The envelope's producer must be the authenticated calling service (no spoofing) and
//!   its type must belong to that producer's own domain (unknown families are refused).
//! * Internal-only facts of the producer's domain are acknowledged and never stored.
//! * `(producer, source event id)` is the dedupe key: a re-published, replayed or
//!   out-of-order duplicate returns the SAME `whe_` event id and never fans out again —
//!   also when the event is Calling's history imported by the Section 53 migration (its
//!   original `whe_` id is returned, so a late re-publication never re-delivers it).
//! * Fan-out targets ACTIVE subscriptions of the SAME tenant / application / environment
//!   whose event types match — a sandbox subscription never receives a production event.
//!   In shadow mode the deliveries are recorded SHADOW (nothing will be sent).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use super::{catalog, ts, DeliveryMode, WebhookService};
use crate::{
    control::stmt,
    http::error::{ApiError, ErrorCode},
};

/// Largest envelope accepted.
pub const MAX_ENVELOPE_BYTES: usize = 16 * 1024;

const ENVELOPE_KEYS: &[&str] = &[
    "event_id",
    "event_type",
    "schema_version",
    "occurred_at",
    "producer_service",
    "runtime_environment",
    "correlation_id",
    "causation_id",
    "ownership",
    "payload",
];

/// The outcome of one publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// The public event id (`None`: an internal-only fact, acknowledged).
    pub event_id: Option<String>,
    /// The source event was already ingested (same event id, no new delivery).
    pub duplicate: bool,
    /// Deliveries created by this publication.
    pub deliveries: u64,
}

/// What a valid envelope says about routing.
#[derive(Debug, Clone)]
pub struct EnvelopeFacts {
    pub source_event_id: String,
    /// (tenant, application, environment).
    pub scope: (Uuid, Uuid, Uuid),
    pub occurred_at: DateTime<Utc>,
    pub correlation_id: Option<String>,
}

fn invalid() -> ApiError {
    ApiError(ErrorCode::ValidationError)
}

fn token(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn envelope_type(s: &str) -> bool {
    s.len() <= 128
        && s.split('.').count() >= 2
        && s.split('.').all(|seg| {
            seg.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Validates the envelope shape, the producer binding and the runtime environment.
pub fn validate_envelope(
    envelope: &Value,
    producer: &str,
    runtime_environment: &str,
) -> Result<EnvelopeFacts, ApiError> {
    let obj = envelope.as_object().ok_or_else(invalid)?;
    if envelope.to_string().len() > MAX_ENVELOPE_BYTES
        || obj.keys().any(|k| !ENVELOPE_KEYS.contains(&k.as_str()))
    {
        return Err(invalid());
    }
    let text = |k: &str| obj.get(k).and_then(Value::as_str);
    // The producer is the authenticated service: never what the body claims.
    if text("producer_service") != Some(producer) {
        return Err(invalid());
    }
    if text("runtime_environment") != Some(runtime_environment) {
        return Err(invalid());
    }
    let source = text("event_id")
        .filter(|e| e.starts_with("evt_") && token(e, 64))
        .ok_or_else(invalid)?;
    if !text("event_type").is_some_and(envelope_type)
        || !obj
            .get("schema_version")
            .and_then(Value::as_i64)
            .is_some_and(|v| v >= 1)
        || !obj.get("payload").is_some_and(Value::is_object)
    {
        return Err(invalid());
    }
    let occurred_at = text("occurred_at")
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(invalid)?;
    let correlation = text("correlation_id").ok_or_else(invalid)?;
    let ownership = obj.get("ownership").ok_or_else(invalid)?;
    let id = |k: &str| {
        ownership
            .get(k)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(invalid)
    };
    Ok(EnvelopeFacts {
        source_event_id: source.to_string(),
        scope: (
            id("tenant_id")?,
            id("application_id")?,
            id("application_environment_id")?,
        ),
        occurred_at,
        correlation_id: token(correlation, 64).then(|| correlation.to_string()),
    })
}

async fn existing_event<C: ConnectionTrait>(
    db: &C,
    producer: &str,
    source_event_id: &str,
) -> Result<Option<String>, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT event_id FROM devplat_webhook_events WHERE producer = $1 AND source_event_id = $2 \
              ORDER BY (origin = 'NATIVE') DESC LIMIT 1",
            vec![producer.into(), source_event_id.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => Some(r.try_get_by_index::<String>(0)?),
        None => None,
    })
}

impl WebhookService {
    /// Ingests the envelope of the authenticated `producer`.
    pub async fn ingest_envelope(
        &self,
        producer: &str,
        runtime_environment: &str,
        envelope: &Value,
    ) -> Result<Ingested, ApiError> {
        let facts = validate_envelope(envelope, producer, runtime_environment)?;
        let event = match catalog::project(producer, envelope) {
            Err(refusal) => {
                tracing::info!(producer, refusal = ?refusal, "platform event refused");
                return Err(invalid());
            }
            Ok(None) => {
                return Ok(Ingested {
                    event_id: None,
                    duplicate: false,
                    deliveries: 0,
                })
            }
            Ok(Some(event)) => event,
        };
        self.ingest_public_event(producer, &facts, &event).await
    }

    /// Stores one public event (deduplicated) and fans it out, in one transaction.
    pub async fn ingest_public_event(
        &self,
        producer: &str,
        facts: &EnvelopeFacts,
        event: &catalog::PublicEvent,
    ) -> Result<Ingested, ApiError> {
        let now = self.now();
        let duplicate = |event_id: String| Ingested {
            event_id: Some(event_id),
            duplicate: true,
            deliveries: 0,
        };
        let txn = self.db.begin().await?;
        if let Some(found) = existing_event(&txn, producer, &facts.source_event_id).await? {
            return Ok(duplicate(found));
        }
        let event_id = format!("whe_{}", Uuid::new_v4().simple());
        let mut body = serde_json::json!({
            "id": event_id,
            "type": event.event_type,
            "apiVersion": "v1",
            "occurredAt": ts(facts.occurred_at),
            "data": Value::Object(event.data.clone()),
        });
        if let Some(c) = &facts.correlation_id {
            body["correlationId"] = Value::String(c.clone());
        }
        let inserted = txn
            .execute(stmt(
                "INSERT INTO devplat_webhook_events (event_id, producer, source_event_id, event_type, \
                   tenant_id, application_id, environment_id, occurred_at, payload, received_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                 ON CONFLICT (producer, source_event_id) WHERE origin = 'NATIVE' DO NOTHING",
                vec![
                    event_id.clone().into(),
                    producer.into(),
                    facts.source_event_id.clone().into(),
                    event.event_type.into(),
                    facts.scope.0.into(),
                    facts.scope.1.into(),
                    facts.scope.2.into(),
                    facts.occurred_at.into(),
                    body.into(),
                    now.into(),
                ],
            ))
            .await?
            .rows_affected();
        if inserted == 0 {
            // A concurrent publication of the same source event won.
            let found = existing_event(&txn, producer, &facts.source_event_id)
                .await?
                .ok_or(ApiError(ErrorCode::ServiceUnavailable))?;
            return Ok(duplicate(found));
        }
        let shadow = self.settings.mode == DeliveryMode::Shadow;
        let deliveries = txn
            .execute(stmt(
                "INSERT INTO devplat_webhook_deliveries (id, subscription_id, event_id, tenant_id, \
                   application_id, environment_id, status, attempt_count, next_attempt_at, \
                   created_at, updated_at, completed_at) \
                 SELECT gen_random_uuid(), s.id, $1, s.tenant_id, s.application_id, s.environment_id, \
                        CASE WHEN $6 THEN 'SHADOW' ELSE 'PENDING' END, 0, $7, $7, $7, \
                        CASE WHEN $6 THEN $7 END \
                   FROM devplat_webhook_subscriptions s \
                  WHERE s.tenant_id = $2 AND s.application_id = $3 AND s.environment_id = $4 \
                    AND s.status = 'ACTIVE' AND $5 = ANY (s.event_types) \
                 ON CONFLICT (subscription_id, event_id) WHERE replay_of IS NULL DO NOTHING",
                vec![
                    event_id.clone().into(),
                    facts.scope.0.into(),
                    facts.scope.1.into(),
                    facts.scope.2.into(),
                    event.event_type.into(),
                    shadow.into(),
                    now.into(),
                ],
            ))
            .await?
            .rows_affected();
        txn.commit().await?;
        Ok(Ingested {
            event_id: Some(event_id),
            duplicate: false,
            deliveries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope() -> Value {
        json!({
            "event_id": "evt_6d5c4b3a291807f6e5d4c3b2a1908f7e",
            "event_type": "calling.call-state-changed",
            "schema_version": 1,
            "occurred_at": "2026-10-07T09:00:00Z",
            "producer_service": "cfmeet.calling",
            "runtime_environment": "test",
            "correlation_id": "corr_0a1b2c3d4e5f60718293a4b5c6d7e8f9",
            "ownership": {
                "tenant_id": "3f0f4c1e-6a7b-4c2d-9e8f-0a1b2c3d4e5f",
                "application_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
                "application_environment_id": "9b2e3c4d-5f60-4718-8a9b-0c1d2e3f4a5b"
            },
            "payload": { "event_type": "CALL_REQUESTED" }
        })
    }

    #[test]
    fn a_valid_envelope_binds_producer_environment_and_scope() {
        let f = validate_envelope(&envelope(), "cfmeet.calling", "test").unwrap();
        assert_eq!(f.source_event_id, "evt_6d5c4b3a291807f6e5d4c3b2a1908f7e");
        assert_eq!(
            f.correlation_id.as_deref(),
            Some("corr_0a1b2c3d4e5f60718293a4b5c6d7e8f9")
        );
        // Spoofed producer, other runtime environment, unknown keys, missing scope.
        assert!(validate_envelope(&envelope(), "cfmeet.messaging", "test").is_err());
        assert!(validate_envelope(&envelope(), "cfmeet.calling", "production").is_err());
        for (k, v) in [
            ("extra", json!(1)),
            ("event_id", json!("not-an-event")),
            ("event_type", json!("Calling.X")),
            ("schema_version", json!(0)),
            ("occurred_at", json!("yesterday")),
            ("payload", json!([])),
        ] {
            let mut e = envelope();
            e[k] = v;
            assert!(
                validate_envelope(&e, "cfmeet.calling", "test").is_err(),
                "{k}"
            );
        }
        let mut e = envelope();
        e["ownership"]
            .as_object_mut()
            .unwrap()
            .remove("application_environment_id");
        assert!(validate_envelope(&e, "cfmeet.calling", "test").is_err());
    }
}
