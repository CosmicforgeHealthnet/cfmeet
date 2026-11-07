//! `Idempotency-Key` replay (contract §2): the same key with the same body returns the
//! stored response; a different body returns `IDEMPOTENCY_CONFLICT`. Only successful
//! responses are stored, inside the change's own transaction, so a failed attempt can be
//! retried. Concurrent requests with one key serialize on a transaction advisory lock.
//! Records are swept after 24 hours by the worker.

use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db,
    error::{ApiError, ErrorCode},
};

pub const HEADER: &str = "idempotency-key";

/// An idempotent request: its scope (operation, caller, actor), key and body digest.
#[derive(Debug, Clone)]
pub struct Idempotency {
    pub scope: String,
    pub key: String,
    pub digest: String,
}

/// A stored response.
#[derive(Debug, Clone, FromQueryResult)]
pub struct Stored {
    pub request_digest: String,
    pub response_status: i16,
    pub response_body: Value,
    pub resource_id: Option<Uuid>,
}

/// SHA-256 of the canonical JSON body (object keys sorted by `serde_json`).
pub fn digest(body: &Value) -> String {
    hex::encode(Sha256::digest(body.to_string().as_bytes()))
}

/// The request's `Idempotency-Key`, if any (`[\x21-\x7e]{1,128}`).
pub fn from_headers(
    headers: &HeaderMap,
    scope: String,
    body: &Value,
) -> Result<Option<Idempotency>, ApiError> {
    let Some(value) = headers.get(HEADER) else {
        return Ok(None);
    };
    let key = value
        .to_str()
        .ok()
        .filter(|k| {
            !k.is_empty() && k.len() <= 128 && k.bytes().all(|b| (0x21..=0x7e).contains(&b))
        })
        .ok_or_else(|| {
            ApiError::validation("Idempotency-Key must be 1-128 visible ASCII characters")
        })?;
    Ok(Some(Idempotency {
        scope: scope.chars().take(200).collect(),
        key: key.to_string(),
        digest: digest(body),
    }))
}

/// Locks the key for this transaction and returns the stored response, if any.
pub async fn begin<C: ConnectionTrait>(
    c: &C,
    idem: &Idempotency,
) -> Result<Option<Stored>, ApiError> {
    db::exec(
        c,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("org-idem:{}:{}", idem.scope, idem.key).into()],
    )
    .await?;
    let stored: Option<Stored> = db::one(
        c,
        "SELECT request_digest, response_status, response_body, resource_id FROM organization_idempotency_records WHERE scope_key = $1 AND idempotency_key = $2 AND created_at > now() - interval '24 hours'",
        vec![idem.scope.clone().into(), idem.key.clone().into()],
    )
    .await?;
    match stored {
        Some(s) if s.request_digest.trim() != idem.digest => {
            Err(ApiError::new(ErrorCode::IdempotencyConflict))
        }
        other => Ok(other),
    }
}

/// Stores the successful response of this transaction.
pub async fn finish<C: ConnectionTrait>(
    c: &C,
    idem: &Idempotency,
    status: u16,
    body: &Value,
    resource_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    db::exec(
        c,
        "INSERT INTO organization_idempotency_records (scope_key, idempotency_key, request_digest, response_status, response_body, resource_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (scope_key, idempotency_key) DO UPDATE SET request_digest = EXCLUDED.request_digest, response_status = EXCLUDED.response_status, response_body = EXCLUDED.response_body, resource_id = EXCLUDED.resource_id, created_at = EXCLUDED.created_at",
        vec![
            idem.scope.clone().into(),
            idem.key.clone().into(),
            idem.digest.clone().into(),
            (status as i16).into(),
            body.clone().into(),
            resource_id.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_independent_of_key_order() {
        let a: Value = serde_json::from_str(r#"{"name":"A","slug":"a-b"}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"slug":"a-b","name":"A"}"#).unwrap();
        assert_eq!(digest(&a), digest(&b));
    }

    #[test]
    fn keys_are_bounded_visible_ascii() {
        let body = serde_json::json!({});
        let mut h = HeaderMap::new();
        h.insert(HEADER, "key-1".parse().unwrap());
        assert!(from_headers(&h, "s".into(), &body).unwrap().is_some());
        h.insert(HEADER, "has space".parse().unwrap());
        assert!(from_headers(&h, "s".into(), &body).is_err());
        assert!(from_headers(&HeaderMap::new(), "s".into(), &body)
            .unwrap()
            .is_none());
    }
}
