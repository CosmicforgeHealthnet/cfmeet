//! Generic command idempotency (Part C §57).
//!
//! A command with an Idempotency-Key runs inside a transaction that first takes a
//! transaction-scoped advisory lock on (scope, operation, key): concurrent replays of the
//! same key serialize, the first commits its effect together with its `call_commands`
//! record, every later one reads the record. The unique constraint
//! `uq_call_commands_idempotency` is the database backstop. A different request under the
//! same key (different canonical fingerprint) is refused with IDEMPOTENCY_CONFLICT.

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseTransaction,
    EntityTrait, QueryFilter,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{execute, CallService};
use crate::{
    calls::{
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::CallCommandId,
        kinds::{CommandOperation, CommandStatus},
    },
    models::call_commands,
};

/// A validated Idempotency-Key: 1–255 printable ASCII characters, no spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: &str) -> CallResult<Self> {
        if value.is_empty()
            || value.len() > 255
            || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(CallError::Validation(
                "Idempotency-Key must be 1-255 printable ASCII characters without spaces.",
            ));
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// SHA-256 (hex) of a command's canonical request: operation, target and body.
pub fn fingerprint(operation: CommandOperation, target: Option<Uuid>, canonical: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(operation.as_str().as_bytes());
    hasher.update([0u8]);
    hasher.update(target.map(|t| t.to_string()).unwrap_or_default().as_bytes());
    hasher.update([0u8]);
    hasher.update(canonical.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

/// A command's idempotency identity.
#[derive(Debug, Clone)]
pub struct CommandKey {
    pub operation: CommandOperation,
    pub key: IdempotencyKey,
    pub fingerprint: String,
}

/// What a previous execution of the same key recorded.
#[derive(Debug, Clone)]
pub enum PriorCommand {
    /// Completed: replay this response.
    Completed {
        status: u16,
        body: Value,
        call_id: Option<Uuid>,
    },
    /// Recorded but not completed yet (Call creation still routing).
    InProgress { call_id: Option<Uuid> },
}

impl CallService {
    /// Serializes executions of one (scope, operation, key) for the rest of `txn` and
    /// returns what an earlier execution recorded (`None`: first execution).
    pub(crate) async fn begin_command_in(
        &self,
        txn: &DatabaseTransaction,
        scope: CallScope,
        key: &CommandKey,
    ) -> CallResult<Option<PriorCommand>> {
        execute(
            txn,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            vec![format!(
                "cfmeet.calling.command:{}:{}:{}:{}:{}",
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                key.operation,
                key.key.as_str()
            )
            .into()],
        )
        .await?;
        self.prior_command(txn, scope, key).await
    }

    /// The prior execution of `key` in `scope`, without locking.
    pub(crate) async fn prior_command<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: CallScope,
        key: &CommandKey,
    ) -> CallResult<Option<PriorCommand>> {
        let Some(existing) = call_commands::Entity::find()
            .filter(call_commands::Column::TenantId.eq(scope.tenant_id().as_uuid()))
            .filter(call_commands::Column::ApplicationId.eq(scope.application_id().as_uuid()))
            .filter(call_commands::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
            .filter(call_commands::Column::Operation.eq(key.operation))
            .filter(call_commands::Column::IdempotencyKey.eq(key.key.as_str()))
            .one(db)
            .await?
        else {
            return Ok(None);
        };
        if existing.request_fingerprint != key.fingerprint {
            return Err(CallError::IdempotencyConflict);
        }
        Ok(Some(match (existing.status, existing.response_snapshot) {
            (CommandStatus::Completed, Some(body)) => PriorCommand::Completed {
                status: existing
                    .response_status
                    .and_then(|s| u16::try_from(s).ok())
                    .unwrap_or(200),
                body,
                call_id: existing.call_id,
            },
            _ => PriorCommand::InProgress {
                call_id: existing.call_id,
            },
        }))
    }

    /// Records the command in `txn`. `completed` carries the response every replay gets.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_command_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        key: &CommandKey,
        call_id: Option<Uuid>,
        completed: Option<(u16, &Value)>,
        result_version: Option<i64>,
        result_resource: Option<Uuid>,
    ) -> CallResult<call_commands::Model> {
        let at = self.now();
        let scope = cmd.scope();
        let model = call_commands::ActiveModel {
            id: Set(CallCommandId::generate().as_uuid()),
            tenant_id: Set(scope.tenant_id().as_uuid()),
            application_id: Set(scope.application_id().as_uuid()),
            environment_id: Set(scope.environment_id().as_uuid()),
            call_id: Set(call_id),
            operation: Set(key.operation),
            idempotency_key: Set(key.key.as_str().to_string()),
            request_fingerprint: Set(key.fingerprint.clone()),
            status: Set(if completed.is_some() {
                CommandStatus::Completed
            } else {
                CommandStatus::InProgress
            }),
            result_call_version: Set(result_version),
            result_resource_id: Set(result_resource),
            response_status: Set(completed.map(|(s, _)| s as i16)),
            response_snapshot: Set(completed.map(|(_, b)| b.clone())),
            actor_kind: Set(cmd.actor().kind()),
            actor_service_account_id: Set(cmd.actor().service_account_id().as_uuid()),
            actor_credential_id: Set(cmd.actor().credential_id().map(|c| c.as_uuid())),
            actor_service: Set(cmd.actor().service().map(|s| s.as_str().to_string())),
            actor_endpoint_id: Set(cmd.actor().endpoint_id().map(|e| e.as_uuid())),
            request_id: Set(cmd.request_id().as_str().to_string()),
            correlation_id: Set(cmd.correlation_id().as_str().to_string()),
            created_at: Set(at),
            completed_at: Set(completed.map(|_| at)),
        };
        Ok(model.insert(txn).await?)
    }

    /// Completes an IN_PROGRESS command with its response.
    pub(crate) async fn complete_command_in(
        &self,
        txn: &DatabaseTransaction,
        scope: CallScope,
        key: &CommandKey,
        status: u16,
        body: &Value,
        result_version: Option<i64>,
    ) -> CallResult<()> {
        let Some(existing) = call_commands::Entity::find()
            .filter(call_commands::Column::TenantId.eq(scope.tenant_id().as_uuid()))
            .filter(call_commands::Column::ApplicationId.eq(scope.application_id().as_uuid()))
            .filter(call_commands::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
            .filter(call_commands::Column::Operation.eq(key.operation))
            .filter(call_commands::Column::IdempotencyKey.eq(key.key.as_str()))
            .one(txn)
            .await?
        else {
            return Err(CallError::NotFound);
        };
        if existing.status == CommandStatus::Completed {
            return Ok(());
        }
        let at = self.now().max(existing.created_at);
        let mut active: call_commands::ActiveModel = existing.into();
        active.status = Set(CommandStatus::Completed);
        active.response_status = Set(Some(status as i16));
        active.response_snapshot = Set(Some(body.clone()));
        active.result_call_version = Set(result_version);
        active.completed_at = Set(Some(at));
        active.update(txn).await?;
        Ok(())
    }
}

impl CallService {
    /// Completes the IN_PROGRESS `operation` record of `call_id` (Call creation finishes
    /// after the media resource is ensured, possibly on another instance).
    pub(crate) async fn complete_call_command_in(
        &self,
        txn: &DatabaseTransaction,
        call_id: Uuid,
        operation: CommandOperation,
        status: u16,
        body: &Value,
        result_version: Option<i64>,
    ) -> CallResult<()> {
        let Some(existing) = call_commands::Entity::find()
            .filter(call_commands::Column::CallId.eq(call_id))
            .filter(call_commands::Column::Operation.eq(operation))
            .filter(call_commands::Column::Status.eq(CommandStatus::InProgress))
            .one(txn)
            .await?
        else {
            return Ok(());
        };
        let at = self.now().max(existing.created_at);
        let mut active: call_commands::ActiveModel = existing.into();
        active.status = Set(CommandStatus::Completed);
        active.response_status = Set(Some(status as i16));
        active.response_snapshot = Set(Some(body.clone()));
        active.result_call_version = Set(result_version);
        active.completed_at = Set(Some(at));
        active.update(txn).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_bounded_printable_ascii() {
        assert!(IdempotencyKey::parse("a").is_ok());
        assert!(IdempotencyKey::parse(&"x".repeat(255)).is_ok());
        for bad in ["", " ", "a b", "ключ", &"x".repeat(256)] {
            assert!(IdempotencyKey::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn fingerprints_distinguish_operation_target_and_body() {
        let body = json!({ "expectedVersion": 3 });
        let a = fingerprint(CommandOperation::Hold, Some(Uuid::nil()), &body);
        assert_eq!(
            a,
            fingerprint(CommandOperation::Hold, Some(Uuid::nil()), &body)
        );
        assert_ne!(
            a,
            fingerprint(CommandOperation::Resume, Some(Uuid::nil()), &body)
        );
        assert_ne!(
            a,
            fingerprint(CommandOperation::Hold, Some(Uuid::from_u128(1)), &body)
        );
        assert_ne!(
            a,
            fingerprint(
                CommandOperation::Hold,
                Some(Uuid::nil()),
                &json!({ "expectedVersion": 4 })
            )
        );
        assert_eq!(a.len(), 64);
    }
}
