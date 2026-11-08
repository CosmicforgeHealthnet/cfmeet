//! Organization offboarding and restoration (final closure, J-10 / J-14).
//!
//! `organization_member_access` holds each member's DESIRED organization access, set by
//! the Organizations event ingress ([`super::organization_events`]) and fenced by the
//! membership version. This worker applies it to the member's principals in the
//! organization's own technical scope (the environments recorded in
//! `organization_scope_environments` when the Meeting Service created those principals):
//!
//! * ACTIVE → principal ACTIVE (re-invited / reactivated member);
//! * SUSPENDED or REMOVED → principal SUSPENDED (Platform Access). Messaging and Calling
//!   resolve the acting principal on every request and revalidate realtime sessions, so a
//!   suspended principal can no longer read, send, call or keep a subscription in the
//!   organization scope. SUSPENDED (not DEACTIVATED) keeps re-admission possible.
//!
//! The member's personal-scope principal is a different principal in a different
//! environment and is never touched: personal conversations keep working.
//!
//! Rows are claimed with `FOR UPDATE SKIP LOCKED` (multi-instance safe); a failure backs
//! off exponentially and is retried until it succeeds (a permanent refusal is retried at
//! the maximum interval and reported, so an operator sees it — never silently dropped).

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait,
};
use uuid::Uuid;

use super::organizations_authority::ResolvedOrganizationScope;

/// One organization environment the Meeting Service created member principals in.
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct ScopeEnvironment {
    pub organization_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
}

/// A failure class of the principal-status port (never a payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortError {
    pub class: String,
    pub permanent: bool,
}

/// Sets a member's principal status in one organization environment.
#[async_trait]
pub trait PrincipalStatusPort: Send + Sync {
    async fn set_status(
        &self,
        scope: &ScopeEnvironment,
        user_id: Uuid,
        status: &str,
    ) -> Result<(), PortError>;
}

#[async_trait]
impl PrincipalStatusPort for super::organization_calling::OrganizationCalling {
    async fn set_status(
        &self,
        scope: &ScopeEnvironment,
        user_id: Uuid,
        status: &str,
    ) -> Result<(), PortError> {
        self.set_principal_status(scope, user_id, status).await
    }
}

/// Records the organization environment of `scope` (best effort; the table belongs to the
/// organization-integration history, which may not be applied yet).
pub async fn record_scope_environment<C: ConnectionTrait>(
    db: &C,
    scope: &ResolvedOrganizationScope,
    service_account_id: Uuid,
) {
    let result = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO organization_scope_environments (environment_id, organization_id, tenant_id, application_id, service_account_id) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (environment_id) DO UPDATE SET organization_id = EXCLUDED.organization_id, tenant_id = EXCLUDED.tenant_id, \
               application_id = EXCLUDED.application_id, service_account_id = EXCLUDED.service_account_id, recorded_at = now()",
            [
                scope.environment_id.into(),
                scope.organization_id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                service_account_id.into(),
            ],
        ))
        .await;
    if let Err(e) = result {
        tracing::debug!(organization_id = %scope.organization_id, "organization scope not recorded: {e}");
    }
}

/// The principal status that realizes a desired organization access.
pub fn principal_status(desired: &str) -> &'static str {
    match desired {
        "ACTIVE" => "ACTIVE",
        _ => "SUSPENDED",
    }
}

/// Backoff after `attempts` failed attempts (2^n seconds, capped at 15 minutes).
pub fn backoff_seconds(attempts: i32) -> i64 {
    2_i64.pow(attempts.clamp(0, 30) as u32).min(900)
}

#[derive(Debug, Clone, FromQueryResult)]
struct Due {
    organization_id: Uuid,
    user_id: Uuid,
    desired_state: String,
    membership_version: i64,
    attempts: i32,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccessReport {
    pub applied: u64,
    pub failed: u64,
    pub principals_updated: u64,
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// One pass: applies up to `limit` due rows.
pub async fn run_once(
    db: &DatabaseConnection,
    port: &dyn PrincipalStatusPort,
    limit: i64,
) -> Result<AccessReport, DbErr> {
    let mut report = AccessReport::default();
    // Claim: push the claimed rows' next attempt out so a concurrent worker skips them
    // while this pass works (the row lock is held only for the claim).
    let txn = db.begin().await?;
    let due = Due::find_by_statement(stmt(
        "WITH due AS (SELECT organization_id, user_id FROM organization_member_access \
             WHERE (applied_state IS DISTINCT FROM desired_state OR applied_version IS DISTINCT FROM membership_version) \
               AND next_attempt_at <= now() \
             ORDER BY next_attempt_at LIMIT $1 FOR UPDATE SKIP LOCKED) \
         UPDATE organization_member_access a SET next_attempt_at = now() + interval '2 minutes' \
         FROM due WHERE a.organization_id = due.organization_id AND a.user_id = due.user_id \
         RETURNING a.organization_id, a.user_id, a.desired_state, a.membership_version, a.attempts",
        vec![limit.into()],
    ))
    .all(&txn)
    .await?;
    txn.commit().await?;

    for row in due {
        let scopes = ScopeEnvironment::find_by_statement(stmt(
            "SELECT e.organization_id, e.tenant_id, e.application_id, e.environment_id, e.service_account_id \
             FROM organization_scope_environments e \
             JOIN communication_principal_links l ON l.environment_id = e.environment_id \
               AND l.subject_kind = 'USER' AND l.subject_id = $2 \
             WHERE e.organization_id = $1 ORDER BY e.environment_id",
            vec![row.organization_id.into(), row.user_id.into()],
        ))
        .all(db)
        .await?;
        let status = principal_status(&row.desired_state);
        let mut failure: Option<PortError> = None;
        for scope in &scopes {
            match port.set_status(scope, row.user_id, status).await {
                Ok(()) => report.principals_updated += 1,
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        match failure {
            None => {
                // Settle only the version this pass applied (a newer event may have arrived).
                db.execute(stmt(
                    "UPDATE organization_member_access SET applied_state = $3, applied_version = $4, applied_at = now(), \
                       attempts = 0, last_error_class = NULL, next_attempt_at = now() \
                     WHERE organization_id = $1 AND user_id = $2 AND membership_version = $4",
                    vec![
                        row.organization_id.into(),
                        row.user_id.into(),
                        row.desired_state.clone().into(),
                        row.membership_version.into(),
                    ],
                ))
                .await?;
                report.applied += 1;
                tracing::info!(
                    organization_id = %row.organization_id,
                    user_id = %row.user_id,
                    desired = %row.desired_state,
                    principals = scopes.len(),
                    "organization access applied"
                );
            }
            Some(e) => {
                let attempts = row.attempts + 1;
                let delay = if e.permanent {
                    backoff_seconds(30)
                } else {
                    backoff_seconds(attempts)
                };
                db.execute(stmt(
                    "UPDATE organization_member_access SET attempts = $3, last_error_class = $4, \
                       next_attempt_at = now() + make_interval(secs => $5) \
                     WHERE organization_id = $1 AND user_id = $2",
                    vec![
                        row.organization_id.into(),
                        row.user_id.into(),
                        attempts.into(),
                        e.class.chars().take(64).collect::<String>().into(),
                        (delay as f64).into(),
                    ],
                ))
                .await?;
                report.failed += 1;
                tracing::warn!(
                    organization_id = %row.organization_id,
                    user_id = %row.user_id,
                    desired = %row.desired_state,
                    attempts,
                    error_class = %e.class,
                    permanent = e.permanent,
                    "organization access not applied; retrying"
                );
            }
        }
    }
    Ok(report)
}

/// Pending organization access changes (monitoring): count and the oldest change.
#[derive(Debug, Clone, FromQueryResult, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessBacklog {
    pub pending: i64,
    pub failing: i64,
    pub oldest_pending_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn backlog<C: ConnectionTrait>(db: &C) -> Result<AccessBacklog, DbErr> {
    Ok(AccessBacklog::find_by_statement(stmt(
        "SELECT count(*)::bigint AS pending, count(*) FILTER (WHERE attempts > 0)::bigint AS failing, min(updated_at) AS oldest_pending_at \
         FROM organization_member_access \
         WHERE applied_state IS DISTINCT FROM desired_state OR applied_version IS DISTINCT FROM membership_version",
        vec![],
    ))
    .one(db)
    .await?
    .unwrap_or(AccessBacklog {
        pending: 0,
        failing: 0,
        oldest_pending_at: None,
    }))
}

/// Runs [`run_once`] every `interval` (forever; the process owns the task).
pub fn spawn(db: DatabaseConnection, port: Arc<dyn PrincipalStatusPort>, interval: Duration) {
    tokio::spawn(async move {
        loop {
            match run_once(&db, port.as_ref(), 100).await {
                Ok(r) if r.failed > 0 => {
                    tracing::warn!(applied = r.applied, failed = r.failed, "organization access pass")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("organization access pass failed: {e}"),
            }
            tokio::time::sleep(interval).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_and_suspension_suspend_and_activation_restores() {
        assert_eq!(principal_status("ACTIVE"), "ACTIVE");
        assert_eq!(principal_status("SUSPENDED"), "SUSPENDED");
        assert_eq!(principal_status("REMOVED"), "SUSPENDED");
    }

    #[test]
    fn backoff_grows_and_is_capped_at_fifteen_minutes() {
        assert_eq!(backoff_seconds(1), 2);
        assert_eq!(backoff_seconds(5), 32);
        assert_eq!(backoff_seconds(20), 900);
        assert_eq!(backoff_seconds(-3), 1);
    }
}
