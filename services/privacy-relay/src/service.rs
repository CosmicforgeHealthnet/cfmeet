//! The Privacy Relay domain service: the single authority for Communication Contexts,
//! Privacy Relay Sessions, communication authorization decisions and active-call
//! enforcement jobs.
//!
//! Command shape (every mutation): one PostgreSQL transaction holding the context row
//! lock writes the authoritative state, the immutable audit event, its outbox envelope,
//! the enforcement job when one is required, and the idempotency record — all or
//! nothing. Required active-call enforcement is then attempted synchronously AFTER the
//! commit; if Calling cannot confirm it, the job stays durable for retry and the response
//! says PENDING (never a false APPLIED).

use std::sync::Arc;

use cf_service_auth::RuntimeEnvironment;
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    clock::SharedClock,
    config::FeatureFlag,
    db,
    enforcement::CallingEnforcer,
    error::{ErrorCode, RelayError, RelayResult},
    vocabulary::EnforcementState,
};

#[derive(Clone)]
pub struct RelayService {
    pub(crate) db: DatabaseConnection,
    pub(crate) clock: SharedClock,
    pub(crate) feature: FeatureFlag,
    pub(crate) runtime: RuntimeEnvironment,
    pub(crate) enforcer: Arc<dyn CallingEnforcer>,
    pub(crate) worker_id: String,
}

/// Result of a mutation: HTTP status, body and whether it replays an earlier execution.
#[derive(Debug, Clone)]
pub struct CommandOutcome {
    pub status: u16,
    pub body: Value,
    pub replayed: bool,
}

impl RelayService {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        feature: FeatureFlag,
        runtime: RuntimeEnvironment,
        enforcer: Arc<dyn CallingEnforcer>,
    ) -> Self {
        Self {
            db,
            clock,
            feature,
            runtime,
            enforcer,
            worker_id: format!("privacy-relay-{}", Uuid::new_v4().simple()),
        }
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub fn feature(&self) -> &FeatureFlag {
        &self.feature
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.runtime
    }

    pub(crate) fn require_enabled(&self) -> RelayResult<()> {
        if self.feature.enabled() {
            Ok(())
        } else {
            Err(RelayError::Refused(ErrorCode::PrivacyRelayDisabled))
        }
    }

    /// A replayed response with the CURRENT state of its enforcement job (the recorded
    /// response captured PENDING at commit time).
    pub(crate) async fn replay(&self, status: u16, mut body: Value) -> RelayResult<CommandOutcome> {
        self.refresh_enforcement(&mut body).await?;
        Ok(CommandOutcome {
            status,
            body,
            replayed: true,
        })
    }

    /// Sets `body.enforcement.status` from the jobs named by
    /// `body.enforcement.enforcementEventIds` (APPLIED only when every job is applied).
    pub(crate) async fn refresh_enforcement(&self, body: &mut Value) -> RelayResult<()> {
        let ids: Vec<Uuid> = body
            .pointer("/enforcement/enforcementEventIds")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                    .collect()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            return Ok(());
        }
        let jobs: Vec<JobStatus> = db::all(
            &self.db,
            "SELECT status FROM privacy_relay_enforcement_jobs WHERE enforcement_event_id = ANY($1)",
            vec![ids.clone().into()],
        )
        .await?;
        let applied = jobs.len() == ids.len() && jobs.iter().all(|j| j.status == "APPLIED");
        body["enforcement"]["status"] = json!(if applied {
            EnforcementState::Applied.as_str()
        } else {
            EnforcementState::Pending.as_str()
        });
        Ok(())
    }

    /// Synchronous-first delivery of the jobs a committed command created; returns the
    /// combined state (APPLIED only if Calling confirmed every one).
    pub(crate) async fn deliver_all(&self, event_ids: &[Uuid]) -> EnforcementState {
        let mut all_applied = true;
        for id in event_ids {
            match self.deliver_enforcement(*id).await {
                Ok(EnforcementState::Applied) => {}
                Ok(_) => all_applied = false,
                Err(error) => {
                    tracing::warn!(%error, enforcement_event_id = %id, "enforcement delivery deferred to retry");
                    all_applied = false;
                }
            }
        }
        if event_ids.is_empty() {
            EnforcementState::NotRequired
        } else if all_applied {
            EnforcementState::Applied
        } else {
            EnforcementState::Pending
        }
    }
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct JobStatus {
    status: String,
}

/// The `enforcement` member of a mutation response.
pub fn enforcement_body(event_ids: &[Uuid], state: EnforcementState) -> Value {
    let state = if event_ids.is_empty() {
        EnforcementState::NotRequired
    } else {
        state
    };
    json!({
        "status": state.as_str(),
        "enforcementEventIds": event_ids,
    })
}
