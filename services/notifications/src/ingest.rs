//! I1 `ingestNotificationEvents` (Document 4 §3.3, §9.5): the responsibility boundary.
//!
//! For each envelope of the batch (≤ 100):
//! 1. strict parse (`deny_unknown_fields`) and the full contract validation of
//!    `cf-notification-client` with the authenticated issuer (`producer == issuer`, closed
//!    per-producer type allowlist, forbidden data keys, sealed allowance, legacy key formats,
//!    CRITICAL rules, size);
//! 2. `route.protocol` must be supported by this replica (N and N−1), else the whole call is
//!    `503 PROTOCOL_UNSUPPORTED` so the producer retries another replica;
//! 3. dedupe on `(producer, eventId)` plus the request digest (`DUPLICATE` has `ACCEPTED`
//!    semantics; a different body under the same id is `REJECTED DIGEST_MISMATCH`), and
//!    `route.decisionId` unique per producer;
//! 4. a category whose service-side channels are all `OFF` is **HELD** (stored, never sent,
//!    counted for the alert), never dropped; shadow copies (`shadow: true`) are recorded
//!    and never sent whatever the mode.
//!
//! `ACCEPTED` is returned only after the batch transaction commits. A rejected envelope is
//! recorded without its body (it may carry forbidden data), only its identity and reason.

use cf_notification_client::{validate, Envelope, EnvelopeError, Lane};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{catalogue, db, metrics::Metrics, routing};

/// Routing protocol versions this release accepts (N and N−1; v1 is the first).
pub const SUPPORTED_PROTOCOLS: &[i16] = &[1];
/// STANDARD backpressure thresholds (Document 4 §10.3).
pub const BACKPRESSURE_PENDING: i64 = 10_000;
pub const BACKPRESSURE_AGE_SECS: i64 = 300;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngestRequest {
    pub events: Vec<Value>,
    #[serde(default)]
    pub dry_run: bool,
    /// Contract amendment (N3): the batch holds producer SHADOW copies of OLD decisions.
    /// They are recorded and evaluated for the comparison and never sent, whatever the
    /// service mode (so a mode disagreement can never turn a shadow copy into a delivery).
    #[serde(default)]
    pub shadow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestResult {
    pub event_id: String,
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("events must hold 1 to 100 envelopes")]
    BatchSize,
    #[error("unsupported routing protocol")]
    ProtocolUnsupported,
    #[error("standard lane backpressure")]
    Backpressure,
    #[error(transparent)]
    Db(#[from] DbErr),
}

fn reason_of(e: &EnvelopeError) -> &'static str {
    match e {
        EnvelopeError::SchemaVersion => "SCHEMA_VERSION",
        EnvelopeError::UnknownEventType => "TYPE_UNKNOWN",
        EnvelopeError::ProducerNotAllowed => "TYPE_NOT_ALLOWED",
        EnvelopeError::ProducerMismatch => "PRODUCER_MISMATCH",
        EnvelopeError::PriorityMismatch => "PRIORITY_MISMATCH",
        EnvelopeError::Invalid(_) => "INVALID_FIELD",
        EnvelopeError::DataKey(_) => "FORBIDDEN_DATA_KEY",
        EnvelopeError::MissingData(_) => "MISSING_DATA",
        EnvelopeError::DataValue(_) => "INVALID_DATA",
        EnvelopeError::SealedNotAllowed => "SEALED_NOT_ALLOWED",
        EnvelopeError::SealedRequired(_) => "SEALED_REQUIRED",
        EnvelopeError::IdempotencyKeyRequired => "IDEMPOTENCY_KEY_REQUIRED",
        EnvelopeError::IdempotencyKeyFormat => "IDEMPOTENCY_KEY_FORMAT",
        EnvelopeError::TooLarge(_) => "TOO_LARGE",
        EnvelopeError::Seal(_) => "SEALED_INVALID",
    }
}

/// SHA-256 of the canonical envelope (struct field order; `data` is a sorted map).
pub fn digest(env: &Envelope) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(env).expect("envelope serializes"),
    ))
}

fn event_id_of(v: &Value) -> String {
    v.get("eventId")
        .and_then(Value::as_str)
        .map(|s| s.chars().take(64).collect())
        .unwrap_or_default()
}

enum Prepared {
    Rejected {
        event_id: String,
        reason: String,
        identity: Option<(String, Uuid, i32, i16, String)>,
    },
    Valid {
        env: Box<Envelope>,
        lane: Lane,
        digest: String,
        /// Addressed to partner end users (ADR 0063 N-P4).
        partner: bool,
    },
}

/// Whether STANDARD ingest is under backpressure.
pub async fn backpressure<C: ConnectionTrait>(conn: &C) -> Result<bool, DbErr> {
    let row = conn
        .query_one(db::stmt(
            "SELECT count(*)::bigint AS n, COALESCE(EXTRACT(EPOCH FROM now() - min(received_at)), 0)::bigint AS age
               FROM notif_event_inbox WHERE state = 'PENDING' AND lane <> 'CRITICAL'",
            vec![],
        ))
        .await?;
    let (n, age) = row
        .map(|r| {
            (
                r.try_get::<i64>("", "n").unwrap_or(0),
                r.try_get::<i64>("", "age").unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));
    Ok(n > BACKPRESSURE_PENDING || age > BACKPRESSURE_AGE_SECS)
}

pub async fn ingest(
    db_conn: &DatabaseConnection,
    metrics: &Metrics,
    issuer: &str,
    request: IngestRequest,
) -> Result<Vec<IngestResult>, IngestError> {
    ingest_with(db_conn, metrics, issuer, request, false).await
}

/// [`ingest`] with partner push (ADR 0063 N-P4) on or off. Off, an envelope addressed to
/// `platform-principal:<env>:<principal>` is refused exactly as before the recipient kind
/// existed (`INVALID_FIELD`). On, such an event is never HELD by the first-party routing
/// modes (no legacy partner path exists to cut over from).
pub async fn ingest_with(
    db_conn: &DatabaseConnection,
    metrics: &Metrics,
    issuer: &str,
    request: IngestRequest,
    partner_push: bool,
) -> Result<Vec<IngestResult>, IngestError> {
    if request.events.is_empty() || request.events.len() > 100 {
        return Err(IngestError::BatchSize);
    }
    let mut prepared = Vec::with_capacity(request.events.len());
    for raw in &request.events {
        let event_id = event_id_of(raw);
        let identity = || -> Option<(String, Uuid, i32, i16, String)> {
            let route = raw.get("route")?;
            Some((
                raw.get("eventType")?.as_str()?.chars().take(96).collect(),
                Uuid::parse_str(route.get("decisionId")?.as_str()?).ok()?,
                route.get("routingVersion")?.as_i64()? as i32,
                route.get("protocol")?.as_i64()? as i16,
                event_id.clone(),
            ))
        };
        let env: Envelope = match serde_json::from_value(raw.clone()) {
            Ok(e) => e,
            Err(_) => {
                prepared.push(Prepared::Rejected {
                    event_id: event_id.clone(),
                    reason: "INVALID_ENVELOPE".into(),
                    identity: identity(),
                });
                continue;
            }
        };
        if !SUPPORTED_PROTOCOLS.contains(&env.route.protocol) {
            return Err(IngestError::ProtocolUnsupported);
        }
        let partner = env.recipients.iter().any(|r| {
            cf_notification_client::parse_platform_principal(&r.reference).is_some()
        });
        match validate(&env, Some(issuer)) {
            Ok(_) if partner && !partner_push => {
                let ident = identity();
                prepared.push(Prepared::Rejected {
                    event_id,
                    reason: reason_of(&EnvelopeError::Invalid("recipients")).into(),
                    identity: ident,
                })
            }
            Ok(spec) => {
                let d = digest(&env);
                prepared.push(Prepared::Valid {
                    lane: spec.lane(),
                    env: Box::new(env),
                    digest: d,
                    partner,
                });
            }
            Err(e) => {
                let ident = identity();
                prepared.push(Prepared::Rejected {
                    event_id,
                    reason: reason_of(&e).into(),
                    identity: ident,
                })
            }
        }
    }

    if request.dry_run {
        let mut out = Vec::new();
        for p in &prepared {
            let id = match p {
                Prepared::Valid { env, .. } => env.event_id.clone(),
                Prepared::Rejected { event_id, .. } => event_id.clone(),
            };
            let exists = db_conn
                .query_one(db::stmt(
                    "SELECT 1 AS x FROM notif_event_inbox WHERE producer = $1 AND event_id = $2 AND state <> 'REJECTED'",
                    vec![issuer.into(), id.clone().into()],
                ))
                .await?
                .is_some();
            out.push(IngestResult {
                event_id: id,
                outcome: if exists { "DUPLICATE" } else { "UNKNOWN" },
                reason: None,
            });
        }
        return Ok(out);
    }

    let non_critical = prepared
        .iter()
        .any(|p| matches!(p, Prepared::Valid { lane, .. } if *lane != Lane::Critical));
    if non_critical && backpressure(db_conn).await? {
        metrics.inc("notif_ingest_backpressure_total", "");
        return Err(IngestError::Backpressure);
    }

    let txn = db_conn.begin().await?;
    let mut results = Vec::with_capacity(prepared.len());
    for p in prepared {
        match p {
            Prepared::Rejected {
                event_id,
                reason,
                identity,
            } => {
                if let Some((event_type, decision, version, protocol, id)) =
                    identity.filter(|i| !i.4.is_empty())
                {
                    // Identity only: the body may carry forbidden data and is not kept.
                    db::exec(
                        &txn,
                        "INSERT INTO notif_event_inbox (producer, event_id, request_digest, event_type, event_version,
                             route_category, lane, state, reject_reason, envelope, route_decision_id, routing_version,
                             routing_protocol, processed_at)
                         VALUES ($1, $2, repeat('0', 64), $3, 0, $4, 'STANDARD', 'REJECTED', $5, '{}'::jsonb, $6, $7, $8, now())
                         ON CONFLICT DO NOTHING",
                        vec![
                            issuer.into(),
                            id.into(),
                            event_type.clone().into(),
                            catalogue::route_category(&event_type).into(),
                            reason.clone().into(),
                            decision.into(),
                            version.into(),
                            protocol.into(),
                        ],
                    )
                    .await?;
                }
                metrics.inc(
                    "notif_ingest_total",
                    format!("outcome=\"REJECTED\",reason=\"{reason}\""),
                );
                results.push(IngestResult {
                    event_id,
                    outcome: "REJECTED",
                    reason: Some(reason),
                });
            }
            Prepared::Valid {
                env,
                lane,
                digest,
                partner,
            } => {
                let category = catalogue::route_category(&env.event_type);
                let state = if request.shadow
                    || partner
                    || env.event_type == cf_notification_client::CONTROL_EVENT
                {
                        "PENDING"
                    } else {
                        let modes = routing::category(&txn, category).await?;
                        if routing::highest(&modes) == routing::Mode::Off {
                            "HELD"
                        } else {
                            "PENDING"
                        }
                    };
                let envelope = serde_json::to_value(&*env).expect("envelope serializes");
                let inserted = db::exec(
                    &txn,
                    "INSERT INTO notif_event_inbox (producer, event_id, request_digest, event_type, event_version,
                         route_category, lane, state, shadow, envelope, route_decision_id, routing_version, routing_protocol)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                     ON CONFLICT DO NOTHING",
                    vec![
                        issuer.into(),
                        env.event_id.clone().into(),
                        digest.clone().into(),
                        env.event_type.clone().into(),
                        (env.event_version as i16).into(),
                        category.into(),
                        lane.as_str().into(),
                        state.into(),
                        request.shadow.into(),
                        envelope.into(),
                        env.route.decision_id.into(),
                        env.route.routing_version.into(),
                        env.route.protocol.into(),
                    ],
                )
                .await?;
                let (outcome, reason) = if inserted == 1 {
                    if state == "HELD" {
                        ("HELD", Some("CATEGORY_NOT_ACTIVE".to_string()))
                    } else {
                        ("ACCEPTED", None)
                    }
                } else {
                    let existing = txn
                        .query_one(db::stmt(
                            "SELECT request_digest, state FROM notif_event_inbox WHERE producer = $1 AND event_id = $2",
                            vec![issuer.into(), env.event_id.clone().into()],
                        ))
                        .await?;
                    match existing {
                        Some(r) if r.try_get::<String>("", "request_digest")? == digest => {
                            ("DUPLICATE", None)
                        }
                        Some(_) => ("REJECTED", Some("DIGEST_MISMATCH".to_string())),
                        // The event id is new but its decision id was already used.
                        None => ("REJECTED", Some("DECISION_REUSED".to_string())),
                    }
                };
                metrics.inc(
                    "notif_ingest_total",
                    format!("outcome=\"{outcome}\",lane=\"{}\"", lane.as_str()),
                );
                results.push(IngestResult {
                    event_id: env.event_id.clone(),
                    outcome,
                    reason,
                });
            }
        }
    }
    txn.commit().await?;
    Ok(results)
}

/// I8 route audit: per category and hour, the accepted decision ids' count and SHA-256
/// over their sorted list (certify compares it with the producer's own digest, §9.10).
pub async fn route_audit<C: ConnectionTrait>(
    conn: &C,
    producer: Option<&str>,
    hours: i64,
) -> Result<Value, DbErr> {
    #[derive(sea_orm::FromQueryResult)]
    struct Row {
        producer: String,
        route_category: String,
        hour: chrono::DateTime<chrono::Utc>,
        ids: Vec<Uuid>,
    }
    let rows: Vec<Row> = db::all(
        conn,
        "SELECT producer, route_category, date_trunc('hour', received_at) AS hour,
                array_agg(route_decision_id ORDER BY route_decision_id) AS ids
           FROM notif_event_inbox
          WHERE state <> 'REJECTED' AND NOT shadow AND received_at > now() - make_interval(hours => $1::int)
            AND ($2::text IS NULL OR producer = $2)
          GROUP BY 1, 2, 3 ORDER BY 1, 2, 3",
        vec![hours.into(), producer.map(str::to_string).into()],
    )
    .await?;
    Ok(Value::Array(
        rows.into_iter()
            .map(|r| {
                let mut h = Sha256::new();
                for id in &r.ids {
                    h.update(id.to_string().as_bytes());
                    h.update(b"\n");
                }
                json!({
                    "producer": r.producer,
                    "category": r.route_category,
                    "hour": r.hour,
                    "count": r.ids.len(),
                    "digest": hex::encode(h.finalize()),
                })
            })
            .collect(),
    ))
}
