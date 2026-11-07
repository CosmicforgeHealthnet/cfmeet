//! `Idempotency-Key` for v2 meeting-space and conference creation (SEC-014).
//!
//! A key is scoped to the authenticated caller and the operation (conference creation
//! also to the space), kept 24 hours, and stored only as a SHA-256 digest together
//! with the digest of the canonical request body:
//!
//! * same key + same body → the original resource is returned again (`201`, header
//!   `Idempotent-Replayed: true`), nothing is created twice;
//! * same key + different body → `409 IDEMPOTENCY_CONFLICT`;
//! * concurrent first requests → exactly one creates (unique key), the others replay.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{actor::ActorContext, meeting_core::validation},
};

pub const RETENTION_HOURS: i64 = 24;

#[derive(Debug, Clone)]
pub struct IdempotencyRequest {
    actor_ref: String,
    operation: String,
    key_digest: String,
    request_digest: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Deterministic JSON (object keys sorted at every level) for request digests.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        Value::String(k.clone()),
                        canonical_json(&map[k.as_str()])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => other.to_string(),
    }
}

/// SHA-256 of the canonical form of a JSON request body.
pub fn body_digest(body: &Value) -> String {
    sha256_hex(canonical_json(body).as_bytes())
}

/// Validate a client `Idempotency-Key` header value (1-128 visible ASCII characters).
pub fn validate_key(raw: &str) -> ApiResult<()> {
    if raw.is_empty() || raw.len() > 128 || !raw.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(validation(
            "Idempotency-Key must be 1-128 visible ASCII characters",
        ));
    }
    Ok(())
}

impl IdempotencyRequest {
    /// `None` when the caller cannot own a key (anonymous / guest callers never create).
    pub fn new(
        ctx: &ActorContext,
        operation: impl Into<String>,
        key: &str,
        request_digest: String,
    ) -> ApiResult<Option<Self>> {
        validate_key(key)?;
        let actor_ref = if let Some(p) = ctx.platform() {
            format!(
                "environment:{}:service_account:{}",
                p.environment_id, p.service_account_id
            )
        } else if let Some(r) = ctx.registered() {
            format!("user:{}", r.user_id)
        } else {
            return Ok(None);
        };
        Ok(Some(Self {
            actor_ref,
            operation: operation.into(),
            key_digest: sha256_hex(key.as_bytes()),
            request_digest,
        }))
    }
}

pub fn conflict() -> ApiError {
    ApiError::domain(
        ErrorCode::IdempotencyConflict,
        "This Idempotency-Key was already used with a different request",
    )
}

/// A concurrent request with the same key has not finished yet.
pub fn in_progress() -> ApiError {
    ApiError::domain(
        ErrorCode::IdempotencyConflict,
        "A request with this Idempotency-Key is still in progress; retry shortly",
    )
}

/// The resource created by an earlier request with this key, if any (live records only).
/// A different request body under the same key is `IDEMPOTENCY_CONFLICT`.
pub async fn prior<C: ConnectionTrait>(
    conn: &C,
    req: &IdempotencyRequest,
) -> ApiResult<Option<Uuid>> {
    let row = conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT request_digest, resource_id FROM meeting_idempotency_keys \
              WHERE actor_ref = $1 AND operation = $2 AND key_digest = $3 AND expires_at > now()",
            [
                req.actor_ref.clone().into(),
                req.operation.clone().into(),
                req.key_digest.clone().into(),
            ],
        ))
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let digest: String = row.try_get("", "request_digest")?;
    if digest != req.request_digest {
        return Err(conflict());
    }
    Ok(Some(row.try_get("", "resource_id")?))
}

/// Record the key in the creating transaction. `false` = another request holds the key
/// (the caller rolls back and replays). An expired record is taken over.
pub async fn record<C: ConnectionTrait>(
    conn: &C,
    req: &IdempotencyRequest,
    resource_type: &str,
    resource_id: Uuid,
) -> ApiResult<bool> {
    let now: DateTime<Utc> = Utc::now();
    let inserted = conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO meeting_idempotency_keys \
               (id, actor_ref, operation, key_digest, request_digest, resource_type, resource_id, created_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (actor_ref, operation, key_digest) DO UPDATE \
               SET request_digest = EXCLUDED.request_digest, resource_type = EXCLUDED.resource_type, \
                   resource_id = EXCLUDED.resource_id, created_at = EXCLUDED.created_at, \
                   expires_at = EXCLUDED.expires_at \
             WHERE meeting_idempotency_keys.expires_at <= EXCLUDED.created_at",
            [
                Uuid::new_v4().into(),
                req.actor_ref.clone().into(),
                req.operation.clone().into(),
                req.key_digest.clone().into(),
                req.request_digest.clone().into(),
                resource_type.into(),
                resource_id.into(),
                now.into(),
                (now + Duration::hours(RETENTION_HOURS)).into(),
            ],
        ))
        .await?
        .rows_affected();
    Ok(inserted == 1)
}

/// Delete expired records (scheduler housekeeping, bounded).
pub async fn purge_expired<C: ConnectionTrait>(conn: &C, now: DateTime<Utc>) -> ApiResult<u64> {
    Ok(conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM meeting_idempotency_keys WHERE id IN \
               (SELECT id FROM meeting_idempotency_keys WHERE expires_at <= $1 LIMIT 500)",
            [now.into()],
        ))
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_ignores_key_order_and_whitespace() {
        let a: Value =
            serde_json::from_str(r#"{"b": 1, "a": {"y": [1, {"d": 2, "c": 3}], "x": "s"}}"#)
                .unwrap();
        let b = json!({ "a": { "x": "s", "y": [1, { "c": 3, "d": 2 }] }, "b": 1 });
        assert_eq!(body_digest(&a), body_digest(&b));
        assert_eq!(
            canonical_json(&b),
            r#"{"a":{"x":"s","y":[1,{"c":3,"d":2}]},"b":1}"#
        );
        assert_ne!(body_digest(&a), body_digest(&json!({ "b": 2 })));
    }

    #[test]
    fn keys_are_validated() {
        assert!(validate_key("abc-123_XYZ").is_ok());
        assert!(validate_key("").is_err());
        assert!(validate_key("has space").is_err());
        assert!(validate_key(&"k".repeat(129)).is_err());
    }
}
