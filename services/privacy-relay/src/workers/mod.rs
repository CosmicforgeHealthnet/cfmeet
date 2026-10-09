//! Background workers: final expiry, enforcement retry and relay-number release /
//! cooldown / drain.
//!
//! Workers MATERIALIZE what read-time evaluation already enforces (an expired context
//! authorizes nothing even if no worker ever ran). They are multi-instance safe: contexts
//! are claimed with `FOR UPDATE SKIP LOCKED`, every transition re-checks the locked row,
//! and the lifecycle trigger makes a second transition of a terminal row impossible — so
//! concurrent workers produce exactly one EXPIRED transition, one audit event and one
//! enforcement job per context.
//!
//! Final expiry policy (fail-safe): when a context's or a session's final authority ends,
//! active protected calls under it are terminated (an enforcement job is always created).

use std::time::Duration;

use sea_orm::TransactionTrait;
use serde_json::json;
use uuid::Uuid;

use crate::{
    audit::{self, AuditDraft},
    context::aggregate::ContextAggregate,
    db,
    enforcement::{self, NewJob},
    error::{ErrorCode, RelayError, RelayResult},
    models::{ContextRow, SessionRow, CONTEXT_COLUMNS, SESSION_COLUMNS},
    scope::{parse_environment_kind, CommandContext, RelayScope},
    service::RelayService,
    vocabulary::{
        AuditEventType, CommunicationContextStatus, EnforcementReason, PrivacyRelaySessionStatus,
    },
};

/// What one sweep did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    pub contexts_expired: usize,
    pub sessions_expired: usize,
    pub enforcement_applied: usize,
    pub relay_assignments_expired: usize,
    pub relay_cooldowns_completed: usize,
    pub relay_drains_completed: usize,
    pub events_published: usize,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct SessionCandidate {
    id: Uuid,
    context_id: Uuid,
}

pub(crate) fn system_cmd(row: &ContextRow) -> RelayResult<CommandContext> {
    let kind = parse_environment_kind(&row.environment_kind)
        .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
    Ok(CommandContext::system(
        RelayScope::from_row(row.tenant_id, row.application_id, row.environment_id),
        kind,
        row.created_by_service_account_id,
    ))
}

impl RelayService {
    /// Expires contexts whose final authority has ended (ACTIVE / SUSPENDED / COMPLETED ->
    /// EXPIRED; REVOKED stays REVOKED), cascading to their sessions.
    pub async fn expire_contexts(&self, limit: u64) -> RelayResult<usize> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let rows: Vec<ContextRow> = db::all(
            &txn,
            &format!(
                "SELECT {CONTEXT_COLUMNS} FROM communication_contexts \
                 WHERE status IN ('ACTIVE', 'SUSPENDED', 'COMPLETED') AND final_expires_at <= $1 \
                 ORDER BY final_expires_at, id LIMIT $2 FOR UPDATE SKIP LOCKED"
            ),
            vec![now.into(), (limit as i64).into()],
        )
        .await?;
        let mut jobs = Vec::new();
        let count = rows.len();
        for row in rows {
            let cmd = system_cmd(&row)?;
            let previous = row.status()?;
            let version = row.version + 1;
            db::exec(
                &txn,
                "UPDATE communication_contexts SET status = 'EXPIRED', version = $2, expired_at = $3, updated_at = $3 \
                 WHERE id = $1 AND status IN ('ACTIVE', 'SUSPENDED', 'COMPLETED')",
                vec![row.id.into(), version.into(), now.into()],
            )
            .await?;
            let ctx = ContextAggregate::load(
                &txn,
                cmd.scope(),
                row.id,
                crate::context::repository::Lock::None,
            )
            .await?;
            self.cascade_sessions_in(&txn, &cmd, &ctx, PrivacyRelaySessionStatus::Expired, now)
                .await?;
            audit::append(
                &txn,
                &cmd,
                self.runtime,
                AuditDraft {
                    event_type: AuditEventType::ContextExpired,
                    context_id: row.id,
                    context_version: version,
                    context_status: CommunicationContextStatus::Expired.as_str(),
                    session: None,
                    session_status: None,
                    enforcement_job_id: None,
                    data: audit::data(vec![
                        ("previous_status", json!(previous.as_str())),
                        ("previous_version", json!(row.version)),
                    ]),
                },
                now,
            )
            .await?;
            jobs.push(
                enforcement::request(
                    &txn,
                    &cmd,
                    self.runtime,
                    NewJob {
                        context_id: row.id,
                        context_version: version,
                        context_status: CommunicationContextStatus::Expired.as_str(),
                        session: None,
                        reason: EnforcementReason::ContextExpired,
                        affected_principals: &[],
                        terminate_active_calls: true,
                    },
                    now,
                )
                .await?,
            );
            tracing::info!(context_id = %row.id, context_version = version, "communication context expired");
        }
        txn.commit().await?;
        self.deliver_all(&jobs).await;
        Ok(count)
    }

    /// Expires sessions whose own final authority has ended.
    pub async fn expire_sessions(&self, limit: u64) -> RelayResult<usize> {
        let now = self.now();
        let candidates: Vec<SessionCandidate> = db::all(
            &self.db,
            "SELECT id, context_id FROM privacy_relay_sessions \
             WHERE status IN ('ACTIVE', 'SUSPENDED', 'COMPLETED') AND final_expires_at <= $1 \
             ORDER BY final_expires_at, id LIMIT $2",
            vec![now.into(), (limit as i64).into()],
        )
        .await?;
        let mut expired = 0;
        for candidate in candidates {
            let txn = self.db.begin().await?;
            // Context first (lock order); skip if another instance holds it.
            let ctx_row: Option<ContextRow> = db::one(
                &txn,
                &format!("SELECT {CONTEXT_COLUMNS} FROM communication_contexts WHERE id = $1 FOR UPDATE SKIP LOCKED"),
                vec![candidate.context_id.into()],
            )
            .await?;
            let Some(ctx_row) = ctx_row else {
                txn.rollback().await?;
                continue;
            };
            let session: Option<SessionRow> = db::one(
                &txn,
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM privacy_relay_sessions WHERE id = $1 \
                       AND status IN ('ACTIVE', 'SUSPENDED', 'COMPLETED') AND final_expires_at <= $2 FOR UPDATE"
                ),
                vec![candidate.id.into(), now.into()],
            )
            .await?;
            let Some(session) = session else {
                txn.rollback().await?;
                continue;
            };
            let cmd = system_cmd(&ctx_row)?;
            let version = session.version + 1;
            db::exec(
                &txn,
                "UPDATE privacy_relay_sessions SET status = 'EXPIRED', version = $2, expired_at = $3, updated_at = $3 \
                 WHERE id = $1",
                vec![session.id.into(), version.into(), now.into()],
            )
            .await?;
            let context_status = ctx_row.status()?.as_str();
            audit::append(
                &txn,
                &cmd,
                self.runtime,
                AuditDraft {
                    event_type: AuditEventType::RelaySessionExpired,
                    context_id: ctx_row.id,
                    context_version: ctx_row.version,
                    context_status,
                    session: Some((session.id, version)),
                    session_status: Some(PrivacyRelaySessionStatus::Expired.as_str()),
                    enforcement_job_id: None,
                    data: audit::data(vec![("previous_status", json!(session.status))]),
                },
                now,
            )
            .await?;
            self.end_relay_assignment_in(
                &txn,
                &crate::relay_numbers::assignment::AssignmentAudit {
                    cmd: &cmd,
                    context_id: ctx_row.id,
                    context_version: ctx_row.version,
                    context_status,
                    session_id: session.id,
                    session_version: version,
                },
                crate::vocabulary::RelayAssignmentEndReason::SessionExpired,
                now,
            )
            .await?;
            let job = enforcement::request(
                &txn,
                &cmd,
                self.runtime,
                NewJob {
                    context_id: ctx_row.id,
                    context_version: ctx_row.version,
                    context_status,
                    session: Some((session.id, version)),
                    reason: EnforcementReason::SessionExpired,
                    affected_principals: &[],
                    terminate_active_calls: true,
                },
                now,
            )
            .await?;
            txn.commit().await?;
            expired += 1;
            tracing::info!(relay_session_id = %session.id, session_version = version, "privacy relay session expired");
            self.deliver_all(&[job]).await;
        }
        Ok(expired)
    }

    /// One worker pass: final expiry, then due enforcement retries.
    pub async fn run_sweep(&self) -> RelayResult<SweepReport> {
        Ok(SweepReport {
            contexts_expired: self.expire_contexts(100).await?,
            sessions_expired: self.expire_sessions(100).await?,
            enforcement_applied: self.run_enforcement_sweep(50).await?,
            // Relay numbers: restrictive and maintenance work, never gated by a flag.
            relay_assignments_expired: self.expire_relay_assignments(100).await?,
            relay_cooldowns_completed: self.complete_relay_cooldowns(100).await?,
            relay_drains_completed: self.complete_relay_drains(100).await?,
            // Durable event publication (never gated by a flag).
            events_published: self.publish_outbox(200).await?,
        })
    }

    /// Runs [`run_sweep`](Self::run_sweep) forever (restrictive work: never gated by the
    /// feature kill switch).
    pub async fn run_workers(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(error) = self.run_sweep().await {
                tracing::warn!(%error, "privacy relay worker sweep failed; will retry");
            }
        }
    }
}

/// Contact Vault maintenance on every instance: key-rotation re-encryption in bounded
/// `SKIP LOCKED` batches (a no-op when every contact is under the active key).
pub async fn run_contact_workers(vault: crate::contact_vault::ContactVault, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if let Err(error) = vault.run_sweep().await {
            tracing::warn!(%error, "contact vault worker sweep failed; will retry");
        }
    }
}
