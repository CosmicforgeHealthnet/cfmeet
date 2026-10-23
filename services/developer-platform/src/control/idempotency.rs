//! `Idempotency-Key` replay of control-plane mutations (Section 15.1).
//!
//! The key is scoped to the authenticated actor and the operation (`scope`), and bound to
//! a digest of the request: replaying the same request returns the original response;
//! reusing the key for a different request is IDEMPOTENCY_KEY_REUSED. A placeholder row
//! claims the key BEFORE the operation runs, so two concurrent requests with one key
//! never both execute (the second gets VERSION_CONFLICT and may retry). Only successful
//! responses are stored; a failed operation releases its claim.
//!
//! Responses containing a one-time secret (invitation tokens) are never stored: their
//! replay returns the original resource WITHOUT the secret.

use std::future::Future;

use axum::http::StatusCode;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::stmt;
use crate::http::error::{ApiError, ErrorCode};

/// Validated `Idempotency-Key` value (visible ASCII, 1–128).
pub fn valid_key(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

pub fn digest(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p);
    }
    hex::encode(h.finalize())
}

const IN_PROGRESS: i16 = 0;

/// Runs `op` at most once per `(scope, key)`. `redact` removes one-time secrets from the
/// stored copy of the response.
pub async fn run<F, Fut>(
    db: &DatabaseConnection,
    scope: &str,
    key: Option<&str>,
    request_digest: &str,
    redact: fn(&mut Value),
    op: F,
) -> Result<(StatusCode, Value), ApiError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(StatusCode, Value), ApiError>>,
{
    let Some(key) = key else {
        return op().await;
    };
    if !valid_key(key) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let scope: String = scope.chars().take(200).collect();
    let claimed = db
        .execute(stmt(
            "INSERT INTO devplat_idempotency_records \
                 (scope_key, idempotency_key, request_digest, response_status, response_body, created_at) \
             VALUES ($1, $2, $3, $4, '{}'::jsonb, $5) ON CONFLICT DO NOTHING",
            vec![
                scope.clone().into(),
                key.into(),
                request_digest.into(),
                IN_PROGRESS.into(),
                Utc::now().into(),
            ],
        ))
        .await?
        .rows_affected()
        == 1;
    if !claimed {
        let row = db
            .query_one(stmt(
                "SELECT request_digest, response_status, response_body FROM devplat_idempotency_records \
                 WHERE scope_key = $1 AND idempotency_key = $2",
                vec![scope.clone().into(), key.into()],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::VersionConflict))?;
        let stored_digest: String = row.try_get("", "request_digest")?;
        if stored_digest != request_digest {
            return Err(ApiError(ErrorCode::IdempotencyKeyReused));
        }
        let status: i16 = row.try_get("", "response_status")?;
        if status == IN_PROGRESS {
            return Err(ApiError(ErrorCode::VersionConflict));
        }
        let body: Value = row.try_get("", "response_body")?;
        return Ok((
            StatusCode::from_u16(status as u16).unwrap_or(StatusCode::OK),
            body,
        ));
    }
    match op().await {
        Ok((status, body)) => {
            let mut stored = body.clone();
            redact(&mut stored);
            db.execute(stmt(
                "UPDATE devplat_idempotency_records SET response_status = $3, response_body = $4 \
                 WHERE scope_key = $1 AND idempotency_key = $2",
                vec![
                    scope.into(),
                    key.into(),
                    (status.as_u16() as i16).into(),
                    stored.into(),
                ],
            ))
            .await?;
            Ok((status, body))
        }
        Err(e) => {
            release(db, &scope, key).await;
            Err(e)
        }
    }
}

async fn release<C: ConnectionTrait>(db: &C, scope: &str, key: &str) {
    let _ = db
        .execute(stmt(
            "DELETE FROM devplat_idempotency_records \
             WHERE scope_key = $1 AND idempotency_key = $2 AND response_status = 0",
            vec![scope.into(), key.into()],
        ))
        .await;
}

/// No redaction.
pub fn keep(_: &mut Value) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_digests_are_bounded_and_stable() {
        assert!(valid_key("abc-123"));
        assert!(!valid_key(""));
        assert!(!valid_key("has space"));
        assert!(!valid_key(&"x".repeat(129)));
        assert_eq!(digest(&[b"a", b"bc"]), digest(&[b"a", b"bc"]));
        assert_ne!(digest(&[b"ab", b"c"]), digest(&[b"a", b"bc"]));
    }
}
