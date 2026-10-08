//! `contact_access_audit`: immutable evidence of every sensitive Contact Vault operation.
//!
//! One row per attempt — successful, denied or failed — with who (actor kind, service,
//! service account, credential id), which ContactReference, which operation and purpose,
//! the execution references it was bound to (context, session, decision, call, call leg,
//! grant), the outcome and a stable reason code, and the request / correlation ids. The
//! table rejects UPDATE and DELETE. A contact value never enters it.

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

use super::{ContactActor, ContactOperation};
use crate::{db, error::RelayResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Denied,
    Failed,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Succeeded => "SUCCEEDED",
            Outcome::Denied => "DENIED",
            Outcome::Failed => "FAILED",
        }
    }
}

/// Execution references an operation is bound to (all optional, all opaque ids).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecutionRefs {
    pub context: Option<Uuid>,
    pub relay_session: Option<Uuid>,
    pub decision: Option<Uuid>,
    pub call: Option<Uuid>,
    pub call_leg: Option<Uuid>,
    pub grant: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct AccessEntry {
    pub contact_reference: Option<Uuid>,
    pub operation: ContactOperation,
    /// Upper-snake purpose, e.g. `CONTACT_MANAGEMENT`, `PUBLIC_NETWORK_CALL`.
    pub purpose: &'static str,
    pub refs: ExecutionRefs,
    pub outcome: Outcome,
    /// Stable reason code (an error code), never free text.
    pub reason: Option<&'static str>,
}

impl AccessEntry {
    pub fn new(operation: ContactOperation, contact_reference: Option<Uuid>) -> Self {
        Self {
            contact_reference,
            operation,
            purpose: "CONTACT_MANAGEMENT",
            refs: ExecutionRefs::default(),
            outcome: Outcome::Succeeded,
            reason: None,
        }
    }

    pub fn purpose(mut self, purpose: &'static str) -> Self {
        self.purpose = purpose;
        self
    }

    pub fn refs(mut self, refs: ExecutionRefs) -> Self {
        self.refs = refs;
        self
    }

    /// A successful operation with an informational code (e.g. DEDUPLICATED).
    pub fn noted(mut self, note: &'static str) -> Self {
        self.reason = Some(note);
        self
    }

    pub fn denied(mut self, reason: &'static str) -> Self {
        self.outcome = Outcome::Denied;
        self.reason = Some(reason);
        self
    }

    pub fn failed(mut self, reason: &'static str) -> Self {
        self.outcome = Outcome::Failed;
        self.reason = Some(reason);
        self
    }
}

/// Appends one audit row (inside the operation's transaction for successes; on its own
/// for denials and failures, whose transaction rolled back).
pub async fn record<C: ConnectionTrait>(
    conn: &C,
    actor: &ContactActor,
    entry: &AccessEntry,
    at: DateTime<Utc>,
) -> RelayResult<Uuid> {
    let id = Uuid::new_v4();
    let scope = actor.scope;
    db::exec(
        conn,
        "INSERT INTO contact_access_audit (id, tenant_id, application_id, environment_id, contact_reference, \
           actor_kind, actor_service, service_account_id, credential_ref, operation, purpose, context_ref, \
           relay_session_ref, decision_ref, call_ref, call_leg_ref, grant_ref, outcome, reason_code, \
           request_id, correlation_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22)",
        vec![
            id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            entry.contact_reference.into(),
            actor.kind.as_str().into(),
            actor.service.as_str().into(),
            actor.service_account_id.into(),
            actor.credential_ref.clone().into(),
            entry.operation.as_str().into(),
            entry.purpose.into(),
            entry.refs.context.into(),
            entry.refs.relay_session.into(),
            entry.refs.decision.into(),
            entry.refs.call.into(),
            entry.refs.call_leg.into(),
            entry.refs.grant.into(),
            entry.outcome.as_str().into(),
            entry.reason.into(),
            actor.request_id.as_str().into(),
            actor.correlation_id.as_str().into(),
            at.into(),
        ],
    )
    .await?;
    Ok(id)
}

/// Records a refused / failed attempt best-effort (never masks the original error).
pub async fn record_refusal<C: ConnectionTrait>(
    conn: &C,
    actor: &ContactActor,
    entry: AccessEntry,
    error: &crate::error::RelayError,
    at: DateTime<Utc>,
) {
    let code = error.code();
    let entry = match error {
        crate::error::RelayError::Database { .. } => entry.failed("INTERNAL_ERROR"),
        _ => entry.denied(code.as_str()),
    };
    if let Err(e) = record(conn, actor, &entry, at).await {
        tracing::error!(error = %e, operation = entry.operation.as_str(), "contact access audit could not be written");
    }
}
