//! The Calling domain service: the only writer of call-domain state.
//!
//! Every mutation runs in one PostgreSQL transaction that
//!
//! 1. locks the Call row (`SELECT … FOR UPDATE`, confined to the caller's tenant /
//!    application / environment — a foreign Call is reported exactly like a missing one);
//! 2. validates the operation against the persisted state (and, for transitions, the
//!    expected version, the graph and the action's evidence);
//! 3. writes the authoritative mutation;
//! 4. appends the immutable [`CallEvent`](super::events::CallEvent)s describing it (gap-free
//!    per-call sequence);
//! 5. appends one publication-ready outbox row per event;
//! 6. commits — or rolls all of it back.
//!
//! The database independently refuses a new Call version without its state event and a
//! call event without its outbox row, so no partial combination can commit. The service
//! is independent of HTTP: handlers call it, and it never accepts database rows or client
//! JSON as input.

mod lifecycle;
mod participation;
mod queries;
mod records;
mod routing;

use cf_service_auth::RuntimeEnvironment;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    DbBackend, EntityTrait, QueryFilter, QuerySelect, Statement,
};

pub use lifecycle::{CreatedCall, NewCall, TransitionOutcome};
pub use queries::{CallDetail, CallPage, ListCallsQuery, ParticipantDetail, SessionDetail};

use super::{
    attributes::{SafeAttributes, SafeValue},
    command::{CallCommandContext, CallScope},
    errors::{CallError, CallResult},
    events::{CallEvent, CallEventType, CALL_EVENT_SCHEMA_VERSION},
    ids::{CallEventId, CallId, OutboxEventId},
    kinds::OutboxStatus,
    state::CallState,
};
use crate::{
    domain::SharedClock,
    models::{call_events, calling_outbox_events, calls},
    platform::limits::LimitDefaults,
};

/// Tunables of the call domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallServiceConfig {
    /// Minimum time a Call must have been ENDING before the reconciliation-timeout
    /// disposition may end it without completion evidence.
    pub ending_reconciliation_timeout: Duration,
}

impl Default for CallServiceConfig {
    fn default() -> Self {
        Self {
            ending_reconciliation_timeout: Duration::minutes(5),
        }
    }
}

/// Call-domain service (see the module documentation).
#[derive(Clone)]
pub struct CallService {
    db: DatabaseConnection,
    clock: SharedClock,
    limit_defaults: LimitDefaults,
    runtime_environment: RuntimeEnvironment,
    config: CallServiceConfig,
}

/// An event to append, before it has an id and a sequence.
pub(crate) struct EventDraft {
    pub event_type: CallEventType,
    pub from_state: Option<CallState>,
    pub to_state: Option<CallState>,
    pub data: SafeAttributes,
}

impl EventDraft {
    pub(crate) fn fact(event_type: CallEventType, data: SafeAttributes) -> Self {
        Self {
            event_type,
            from_state: None,
            to_state: None,
            data,
        }
    }
}

/// Builds event data from known-safe entries.
pub(crate) fn data(entries: Vec<(&str, SafeValue)>) -> CallResult<SafeAttributes> {
    let mut attrs = SafeAttributes::new();
    for (k, v) in entries {
        attrs.insert(k, v)?;
    }
    Ok(attrs)
}

pub(crate) fn text(value: impl ToString) -> SafeValue {
    SafeValue::Text(value.to_string())
}

/// `NOT IN (…)` list of the terminal states, as SQL (literal, so partial indexes apply).
pub(crate) const TERMINAL_STATES_SQL: &str =
    "('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')";

impl CallService {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        limit_defaults: LimitDefaults,
        runtime_environment: RuntimeEnvironment,
        config: CallServiceConfig,
    ) -> Self {
        Self {
            db,
            clock,
            limit_defaults,
            runtime_environment,
            config,
        }
    }

    pub fn config(&self) -> &CallServiceConfig {
        &self.config
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Locks the Call row for the rest of the transaction. A Call outside `scope` is
    /// indistinguishable from a missing one.
    pub(crate) async fn lock_call(
        &self,
        txn: &DatabaseTransaction,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<calls::Model> {
        calls::Entity::find_by_id(call_id.as_uuid())
            .filter(calls::Column::TenantId.eq(scope.tenant_id().as_uuid()))
            .filter(calls::Column::ApplicationId.eq(scope.application_id().as_uuid()))
            .filter(calls::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or(CallError::NotFound)
    }

    /// Locks the Call row and refuses terminal Calls.
    pub(crate) async fn lock_open_call(
        &self,
        txn: &DatabaseTransaction,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<calls::Model> {
        let call = self.lock_call(txn, scope, call_id).await?;
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        Ok(call)
    }

    /// Appends one immutable call event and its outbox row. `call` is the Call row as it
    /// is persisted after the mutation the event describes.
    pub(crate) async fn append_event(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        draft: EventDraft,
        at: DateTime<Utc>,
    ) -> CallResult<CallEvent> {
        let last = scalar_i64(
            txn,
            "SELECT COALESCE(MAX(sequence), 0) FROM call_events WHERE call_id = $1",
            vec![call.id.into()],
        )
        .await?;
        let event = CallEvent {
            event_id: CallEventId::generate(),
            call_id: CallId::from_uuid(call.id),
            scope: cmd.scope(),
            sequence: last + 1,
            event_type: draft.event_type,
            schema_version: CALL_EVENT_SCHEMA_VERSION,
            from_state: draft.from_state,
            to_state: draft.to_state,
            call_version: call.version,
            actor_kind: cmd.actor().kind(),
            request_id: cmd.request_id().as_str().to_string(),
            correlation_id: cmd.correlation_id().as_str().to_string(),
            data: draft.data.to_json(),
            occurred_at: at,
        };
        let created_at = self.now();
        call_events::Entity::insert(call_events::ActiveModel {
            event_id: Set(event.event_id.as_uuid()),
            call_id: Set(call.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            sequence: Set(event.sequence),
            event_type: Set(event.event_type),
            schema_version: Set(event.schema_version),
            from_state: Set(event.from_state),
            to_state: Set(event.to_state),
            call_version: Set(event.call_version),
            actor_kind: Set(event.actor_kind),
            actor_service_account_id: Set(cmd.actor().service_account_id().as_uuid()),
            actor_credential_id: Set(cmd.actor().credential_id().map(|c| c.as_uuid())),
            actor_service: Set(cmd.actor().service().map(|s| s.as_str().to_string())),
            request_id: Set(event.request_id.clone()),
            correlation_id: Set(event.correlation_id.clone()),
            payload: Set(event.data.clone()),
            occurred_at: Set(event.occurred_at),
            created_at: Set(created_at),
        })
        .exec_without_returning(txn)
        .await?;
        calling_outbox_events::Entity::insert(calling_outbox_events::ActiveModel {
            id: Set(OutboxEventId::generate().as_uuid()),
            event_id: Set(event.event_id.envelope_id()),
            source_event_id: Set(event.event_id.as_uuid()),
            aggregate_type: Set("CALL".to_string()),
            aggregate_id: Set(call.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            event_type: Set(event.event_type.envelope_type()),
            schema_version: Set(event.schema_version),
            envelope: Set(event.envelope(self.runtime_environment)),
            status: Set(OutboxStatus::Pending),
            attempt_count: Set(0),
            next_attempt_at: Set(created_at),
            published_at: Set(None),
            created_at: Set(created_at),
            updated_at: Set(created_at),
        })
        .exec_without_returning(txn)
        .await?;
        Ok(event)
    }
}

/// One BIGINT scalar from a query.
pub(crate) async fn scalar_i64<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> CallResult<i64> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?
        .ok_or(CallError::Database {
            summary: "scalar query returned no row".to_string(),
            connectivity: false,
        })?;
    Ok(row.try_get_by_index::<i64>(0)?)
}

/// Runs a statement and returns the affected row count.
pub(crate) async fn execute<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> CallResult<u64> {
    Ok(db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_sql_list_matches_the_state_machine() {
        let expected = format!(
            "({})",
            CallState::TERMINAL
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        assert_eq!(TERMINAL_STATES_SQL, expected);
    }
}
