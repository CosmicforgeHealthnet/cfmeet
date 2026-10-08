//! Command idempotency (`privacy_relay_commands`).
//!
//! Every mutation carries an Idempotency-Key. Inside the command's transaction the key is
//! serialized with a transaction-scoped advisory lock on (scope, operation, key), so a
//! concurrent duplicate waits for the first execution and then replays its recorded
//! response: one key, exactly one domain effect. A different request under the same key
//! (other canonical fingerprint) is 409 IDEMPOTENCY_CONFLICT. A command that fails rolls
//! back with its record, so a retry re-executes (and fails the same way).

use chrono::{DateTime, Utc};
use sea_orm::DatabaseTransaction;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db,
    error::{ErrorCode, RelayError, RelayResult},
    models::CommandRow,
    scope::CommandContext,
    vocabulary::CommandOperation,
};

/// Validated Idempotency-Key: 1-255 printable ASCII characters without spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: &str) -> RelayResult<Self> {
        if !value.is_empty()
            && value.len() <= 255
            && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            Ok(Self(value.to_string()))
        } else {
            Err(RelayError::Validation(
                "idempotencyKey must be 1-255 printable ASCII characters without spaces.",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A command's idempotency identity.
#[derive(Debug, Clone)]
pub struct CommandKey {
    pub operation: CommandOperation,
    pub key: IdempotencyKey,
    pub fingerprint: String,
}

impl CommandKey {
    /// Fingerprint = SHA-256(operation, target resource, canonical typed request).
    pub fn new<T: Serialize>(
        operation: CommandOperation,
        key: IdempotencyKey,
        target: Option<Uuid>,
        request: &T,
    ) -> RelayResult<Self> {
        let canonical = serde_json::to_value(request)
            .map_err(|_| RelayError::Refused(ErrorCode::InternalError))?;
        let mut hasher = Sha256::new();
        hasher.update(operation.as_str().as_bytes());
        hasher.update([0u8]);
        hasher.update(target.map(|t| t.to_string()).unwrap_or_default().as_bytes());
        hasher.update([0u8]);
        hasher.update(canonical.to_string().as_bytes());
        Ok(Self {
            operation,
            key,
            fingerprint: hex::encode(hasher.finalize()),
        })
    }
}

/// Recorded response of an earlier execution.
#[derive(Debug, Clone)]
pub struct PriorResponse {
    pub status: u16,
    pub body: Value,
}

/// Locks the key for the rest of `txn` and returns the earlier execution, if any.
pub async fn begin(
    txn: &DatabaseTransaction,
    cmd: &CommandContext,
    key: &CommandKey,
) -> RelayResult<Option<PriorResponse>> {
    let scope = cmd.scope();
    db::advisory_xact_lock(
        txn,
        &format!(
            "cfmeet.privacy-relay.command:{}:{}:{}:{}:{}",
            scope.tenant_id(),
            scope.application_id(),
            scope.environment_id(),
            key.operation,
            key.key.as_str()
        ),
    )
    .await?;
    let existing: Option<CommandRow> = db::one(
        txn,
        "SELECT id, request_fingerprint, state, response_status, response_snapshot, result_resource_id \
         FROM privacy_relay_commands \
         WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
           AND operation = $4 AND idempotency_key = $5",
        vec![
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            key.operation.as_str().into(),
            key.key.as_str().into(),
        ],
    )
    .await?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    if existing.request_fingerprint != key.fingerprint {
        return Err(RelayError::Refused(ErrorCode::IdempotencyConflict));
    }
    match (existing.state.as_str(), existing.response_snapshot) {
        ("COMPLETED", Some(body)) => Ok(Some(PriorResponse {
            status: existing
                .response_status
                .and_then(|s| u16::try_from(s).ok())
                .unwrap_or(200),
            body,
        })),
        // Commands complete inside their own transaction; an incomplete committed record
        // cannot exist. Fail closed.
        _ => Err(RelayError::Refused(ErrorCode::InternalError)),
    }
}

/// Records the completed command in `txn` (the transaction of its domain effect).
#[allow(clippy::too_many_arguments)]
pub async fn complete(
    txn: &DatabaseTransaction,
    cmd: &CommandContext,
    key: &CommandKey,
    resource_id: Option<Uuid>,
    result_version: Option<i64>,
    status: u16,
    body: &Value,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    let scope = cmd.scope();
    db::exec(
        txn,
        "INSERT INTO privacy_relay_commands (id, tenant_id, application_id, environment_id, resource_id, \
           operation, idempotency_key, request_fingerprint, state, response_status, response_snapshot, \
           result_version, result_resource_id, actor_service_account_id, actor_service, request_id, \
           correlation_id, created_at, completed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'COMPLETED', $9, $10, $11, $5, $12, $13, $14, $15, $16, $16)",
        vec![
            Uuid::new_v4().into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            resource_id.into(),
            key.operation.as_str().into(),
            key.key.as_str().into(),
            key.fingerprint.clone().into(),
            (status as i16).into(),
            body.clone().into(),
            result_version.into(),
            cmd.service_account_id().into(),
            cmd.actor_service().as_str().into(),
            cmd.request_id().as_str().into(),
            cmd.correlation_id().as_str().into(),
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
    fn keys_and_fingerprints() {
        assert!(IdempotencyKey::parse("ctx-1").is_ok());
        assert!(IdempotencyKey::parse("").is_err());
        assert!(IdempotencyKey::parse("has space").is_err());
        assert!(IdempotencyKey::parse(&"k".repeat(256)).is_err());
        let k = IdempotencyKey::parse("k").unwrap();
        let a = CommandKey::new(
            CommandOperation::CreateContext,
            k.clone(),
            None,
            &serde_json::json!({"a": 1}),
        )
        .unwrap();
        let b = CommandKey::new(
            CommandOperation::CreateContext,
            k.clone(),
            None,
            &serde_json::json!({"a": 2}),
        )
        .unwrap();
        let c = CommandKey::new(
            CommandOperation::UpdateContext,
            k,
            None,
            &serde_json::json!({"a": 1}),
        )
        .unwrap();
        assert_ne!(a.fingerprint, b.fingerprint);
        assert_ne!(a.fingerprint, c.fingerprint);
    }
}
