//! Idempotency keys with scoped uniqueness.
//!
//! A key is unique per `(scope, operation, key)`: the same key in another tenant scope,
//! service or operation is a different key. Use it INSIDE the transaction of the
//! operation it protects:
//!
//! 1. [`begin`] inserts the key IN_PROGRESS (a concurrent duplicate blocks on the unique
//!    index until the first transaction ends);
//! 2. the operation runs in the same transaction;
//! 3. [`complete`] stores the outcome; commit.
//!
//! If the operation fails the transaction rolls back and the key never existed, so a
//! retry runs again. A committed key replays the stored outcome for the same request
//! fingerprint and refuses a different request with the same key.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Who a key belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyScope {
    /// A customer application environment (`tenant:application:environment`).
    Customer {
        tenant_id: Uuid,
        application_id: Uuid,
        environment_id: Uuid,
    },
    /// An internal service (by its service identity).
    Service(String),
    /// A human operator.
    Operator(String),
}

impl IdempotencyScope {
    fn kind(&self) -> &'static str {
        match self {
            IdempotencyScope::Customer { .. } => "CUSTOMER",
            IdempotencyScope::Service(_) => "SERVICE",
            IdempotencyScope::Operator(_) => "OPERATOR",
        }
    }

    fn key(&self) -> String {
        match self {
            IdempotencyScope::Customer {
                tenant_id,
                application_id,
                environment_id,
            } => format!(
                "{}:{}:{}",
                tenant_id.simple(),
                application_id.simple(),
                environment_id.simple()
            ),
            IdempotencyScope::Service(s) | IdempotencyScope::Operator(s) => s.clone(),
        }
    }
}

/// `true` for a well-formed client key (`[A-Za-z0-9._:-]{8,128}`).
pub fn valid_key(key: &str) -> bool {
    (8..=128).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// SHA-256 fingerprint of the request that a key protects (method, route, canonical body).
pub fn fingerprint(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Begin {
    /// First use: run the operation, then [`complete`] this record.
    New(Uuid),
    /// Already completed for the same request: return the stored outcome.
    Replay {
        status: u16,
        body: Option<Value>,
        resource_id: Option<Uuid>,
    },
    /// The key was used for a different request.
    Mismatch,
    /// The key is held by an operation that has not completed (only possible across
    /// separate transactions, e.g. a stuck caller).
    InProgress,
}

pub async fn begin<C: ConnectionTrait>(
    db: &C,
    scope: &IdempotencyScope,
    operation: &str,
    key: &str,
    request_hash: &str,
    now: DateTime<Utc>,
    ttl: chrono::Duration,
) -> Result<Begin, DbErr> {
    if !valid_key(key) {
        return Err(DbErr::Custom("invalid idempotency key".into()));
    }
    // An expired key is forgotten first, so it can be reused.
    db.execute(stmt(
        "DELETE FROM numbering_idempotency_keys \
         WHERE scope_kind = $1 AND scope_key = $2 AND operation = $3 AND idempotency_key = $4 AND expires_at <= $5",
        vec![scope.kind().into(), scope.key().into(), operation.into(), key.into(), now.into()],
    ))
    .await?;
    let id = Uuid::new_v4();
    let inserted = db
        .query_one(stmt(
            "INSERT INTO numbering_idempotency_keys (id, scope_kind, scope_key, operation, idempotency_key, \
               request_hash, status, created_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 'IN_PROGRESS', $7, $8) \
             ON CONFLICT (scope_kind, scope_key, operation, idempotency_key) DO NOTHING RETURNING id",
            vec![
                id.into(),
                scope.kind().into(),
                scope.key().into(),
                operation.into(),
                key.into(),
                request_hash.into(),
                now.into(),
                (now + ttl).into(),
            ],
        ))
        .await?;
    if inserted.is_some() {
        return Ok(Begin::New(id));
    }
    let row = db
        .query_one(stmt(
            "SELECT request_hash, status, response_status, response_body, resource_id \
               FROM numbering_idempotency_keys \
              WHERE scope_kind = $1 AND scope_key = $2 AND operation = $3 AND idempotency_key = $4",
            vec![
                scope.kind().into(),
                scope.key().into(),
                operation.into(),
                key.into(),
            ],
        ))
        .await?
        .ok_or_else(|| DbErr::RecordNotFound("idempotency key".into()))?;
    let hash: String = row.try_get_by_index(0)?;
    if hash != request_hash {
        return Ok(Begin::Mismatch);
    }
    let status: String = row.try_get_by_index(1)?;
    if status != "COMPLETED" {
        return Ok(Begin::InProgress);
    }
    let response_status: i16 = row.try_get_by_index(2)?;
    Ok(Begin::Replay {
        status: response_status as u16,
        body: row.try_get_by_index(3)?,
        resource_id: row.try_get_by_index(4)?,
    })
}

/// Stores the outcome of the operation `record` protects (same transaction).
pub async fn complete<C: ConnectionTrait>(
    db: &C,
    record: Uuid,
    status: u16,
    body: Option<Value>,
    resource_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    let updated = db
        .execute(stmt(
            "UPDATE numbering_idempotency_keys SET status = 'COMPLETED', response_status = $2, response_body = $3, \
               resource_id = $4, completed_at = $5 WHERE id = $1 AND status = 'IN_PROGRESS'",
            vec![record.into(), (status as i16).into(), body.into(), resource_id.into(), now.into()],
        ))
        .await?
        .rows_affected();
    if updated != 1 {
        return Err(DbErr::RecordNotUpdated);
    }
    Ok(())
}

/// Forgets expired keys (bounded batch). Returns how many were removed.
pub async fn purge_expired<C: ConnectionTrait>(
    db: &C,
    now: DateTime<Utc>,
    limit: u64,
) -> Result<u64, DbErr> {
    Ok(db
        .execute(stmt(
            "DELETE FROM numbering_idempotency_keys WHERE id IN ( \
               SELECT id FROM numbering_idempotency_keys WHERE expires_at <= $1 LIMIT $2 FOR UPDATE SKIP LOCKED)",
            vec![now.into(), (limit as i64).into()],
        ))
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_bounded_and_fingerprints_are_unambiguous() {
        assert!(valid_key("order-2026:0001"));
        assert!(!valid_key("short"));
        assert!(!valid_key("has space in it"));
        assert!(!valid_key(&"k".repeat(129)));
        // Length-prefixed parts: ("ab","c") and ("a","bc") differ.
        assert_ne!(fingerprint(&[b"ab", b"c"]), fingerprint(&[b"a", b"bc"]));
        assert_eq!(fingerprint(&[b"x"]).len(), 64);
    }

    #[test]
    fn scopes_never_collide_across_kinds() {
        let t = Uuid::new_v4();
        let customer = IdempotencyScope::Customer {
            tenant_id: t,
            application_id: t,
            environment_id: t,
        };
        assert_eq!(customer.kind(), "CUSTOMER");
        assert_eq!(
            IdempotencyScope::Service("cfmeet.calling".into()).kind(),
            "SERVICE"
        );
        assert!(customer.key().len() <= 200);
    }
}
