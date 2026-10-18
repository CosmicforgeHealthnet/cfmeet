//! Calling's projection of the shared identity authority (ADR 0032).
//!
//! Every Calling table keeps referencing tenants, applications, environments, service
//! accounts, credentials and principals through composite foreign keys, so Calling keeps
//! those rows — with the SAME ids — as a projection of Platform Access. Calling OWNS the
//! projection, not the identity: rows are written only here, from Platform Access
//! snapshots, never from requests or administration.
//!
//! * [`IdentityProjection::apply_chain`] applies the authority chain of every
//!   authenticated request (so a credential issued a moment ago can act at once).
//! * [`IdentityProjection::sync`] applies the identity change feed after the persisted
//!   cursor, page by page, each page in one transaction with its cursor. Consumers may
//!   see a change twice (a crash between apply and commit replays the page): applying a
//!   snapshot is idempotent and ordered by `revision`, so a stale snapshot never
//!   regresses a row.
//! * [`IdentityProjection::catch_up`] runs the sync on demand (a principal or credential
//!   that is not projected yet); concurrent callers share one run.
//!
//! Calling-owned consequences of identity changes are applied here too: a rotation
//! successor inherits its predecessor's credential limits (Part G §70), and a tombstoned
//! principal's participations lose their display names (Part H §80.42) unless a
//! retention hold covers the principal.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, Statement,
    TransactionTrait,
};
use serde::de::DeserializeOwned;
use uuid::Uuid;

use super::{
    access::{
        ApplicationScopeSnapshot, ApplicationSnapshot, AuthorityChain, AuthorityError,
        CredentialScopeSnapshot, CredentialSnapshot, EnvironmentSnapshot, IdentityAuthority,
        IdentityChange, PrincipalSnapshot, ServiceAccountSnapshot, TenantSnapshot,
    },
    error::summarize_db_error,
};
use crate::domain::SharedClock;

/// Largest feed page requested.
pub const PAGE: u64 = 200;
/// Pages applied by one [`IdentityProjection::sync`] call at most.
pub const MAX_PAGES_PER_SYNC: usize = 50;

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("{0}")]
    Authority(#[from] AuthorityError),
    #[error("identity projection database failure ({0})")]
    Database(String),
    #[error("identity change {sequence} is not a valid {kind} snapshot")]
    InvalidSnapshot { sequence: i64, kind: String },
}

impl From<DbErr> for ProjectionError {
    fn from(e: DbErr) -> Self {
        ProjectionError::Database(summarize_db_error(&e))
    }
}

pub type ProjectionResult<T> = Result<T, ProjectionError>;

/// What a sync did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    pub pages: usize,
    pub changes: usize,
    pub cursor: i64,
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn is_foreign_key_violation(e: &ProjectionError) -> bool {
    matches!(e, ProjectionError::Database(s) if s.contains("sqlstate 23503"))
}

pub struct IdentityProjection {
    db: DatabaseConnection,
    authority: Arc<dyn IdentityAuthority>,
    clock: SharedClock,
    /// Serializes syncs inside this instance (instances serialize on the cursor row).
    sync_lock: tokio::sync::Mutex<()>,
    /// Revisions this instance already applied (skips redundant upserts per request).
    applied: StdMutex<HashMap<Uuid, i64>>,
}

const APPLIED_CACHE_LIMIT: usize = 10_000;

impl IdentityProjection {
    pub fn new(
        db: DatabaseConnection,
        authority: Arc<dyn IdentityAuthority>,
        clock: SharedClock,
    ) -> Self {
        Self {
            db,
            authority,
            clock,
            sync_lock: tokio::sync::Mutex::new(()),
            applied: StdMutex::new(HashMap::new()),
        }
    }

    pub fn authority(&self) -> &Arc<dyn IdentityAuthority> {
        &self.authority
    }

    fn fresh(&self, id: Uuid, revision: i64) -> bool {
        self.applied
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .is_none_or(|seen| *seen < revision)
    }

    fn remember(&self, ids: &[(Uuid, i64)]) {
        let mut applied = self.applied.lock().unwrap_or_else(|p| p.into_inner());
        if applied.len() > APPLIED_CACHE_LIMIT {
            applied.clear();
        }
        for (id, revision) in ids {
            applied.insert(*id, *revision);
        }
    }

    /// Forgets cached revisions (a database restore / rollback drill).
    pub fn forget(&self) {
        self.applied
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// Applies the authority chain of an authenticated credential. A missing parent
    /// (e.g. a rotation successor not projected yet) triggers one catch-up and a retry.
    pub async fn apply_chain(&self, chain: &AuthorityChain) -> ProjectionResult<()> {
        let ids = [
            (chain.tenant.id, chain.tenant.revision),
            (chain.application.id, chain.application.revision),
            (chain.environment.id, chain.environment.revision),
            (chain.service_account.id, chain.service_account.revision),
            (chain.credential.id, chain.credential.revision),
        ];
        if ids.iter().all(|(id, rev)| !self.fresh(*id, *rev)) {
            return Ok(());
        }
        match self.apply_chain_once(chain).await {
            Err(e) if is_foreign_key_violation(&e) => {
                self.catch_up().await?;
                self.apply_chain_once(chain).await?;
            }
            other => other?,
        }
        self.remember(&ids);
        Ok(())
    }

    async fn apply_chain_once(&self, chain: &AuthorityChain) -> ProjectionResult<()> {
        let txn = self.db.begin().await?;
        upsert_tenant(&txn, &chain.tenant).await?;
        upsert_application(&txn, &chain.application).await?;
        upsert_environment(&txn, &chain.environment).await?;
        upsert_service_account(&txn, &chain.service_account).await?;
        upsert_credential(&txn, &chain.credential).await?;
        txn.commit().await?;
        Ok(())
    }

    /// Applies one principal snapshot (from a registration or tombstone answer).
    pub async fn apply_principal(&self, principal: &PrincipalSnapshot) -> ProjectionResult<()> {
        let once = || async {
            let txn = self.db.begin().await?;
            upsert_principal(&txn, principal, self.clock.now()).await?;
            txn.commit().await?;
            Ok::<_, ProjectionError>(())
        };
        match once().await {
            Err(e) if is_foreign_key_violation(&e) => {
                self.catch_up().await?;
                once().await
            }
            other => other,
        }
    }

    /// Applies the change feed after the persisted cursor until caught up (bounded).
    pub async fn sync(&self) -> ProjectionResult<SyncReport> {
        let _guard = self.sync_lock.lock().await;
        let mut report = SyncReport::default();
        for _ in 0..MAX_PAGES_PER_SYNC {
            let cursor = self.cursor().await?;
            let page = self.authority.changes(cursor, PAGE).await?;
            report.cursor = cursor;
            if page.changes.is_empty() {
                self.touch().await?;
                break;
            }
            let txn = self.db.begin().await?;
            // Lock the cursor row: concurrent instances apply a page once each, in order.
            let locked: i64 = txn
                .query_one(stmt(
                    "SELECT last_sequence FROM platform_identity_projection_state WHERE id = 1 FOR UPDATE",
                    vec![],
                ))
                .await?
                .map(|r| r.try_get_by_index::<i64>(0))
                .transpose()?
                .unwrap_or(0);
            if locked != cursor {
                // Another instance advanced the cursor meanwhile: re-read the feed.
                txn.rollback().await?;
                continue;
            }
            let now = self.clock.now();
            for change in &page.changes {
                apply_change(&txn, change, now).await?;
            }
            txn.execute(stmt(
                "UPDATE platform_identity_projection_state SET last_sequence = $1, last_synced_at = $2 WHERE id = 1",
                vec![page.next_after.into(), now.into()],
            ))
            .await?;
            txn.commit().await?;
            report.pages += 1;
            report.changes += page.changes.len();
            report.cursor = page.next_after;
            crate::metrics::inc("calling_identity_projection_pages_total", &[]);
            if !page.has_more {
                break;
            }
        }
        if report.changes > 0 {
            // Cached revisions may be older than what the feed just applied; they only
            // ever skip redundant writes, so clearing is always safe.
            self.forget();
            tracing::debug!(
                changes = report.changes,
                cursor = report.cursor,
                "identity projection synced"
            );
        }
        Ok(report)
    }

    /// On-demand sync (an identity referenced by a request is not projected yet).
    pub async fn catch_up(&self) -> ProjectionResult<SyncReport> {
        self.sync().await
    }

    pub async fn cursor(&self) -> ProjectionResult<i64> {
        Ok(self
            .db
            .query_one(stmt(
                "SELECT last_sequence FROM platform_identity_projection_state WHERE id = 1",
                vec![],
            ))
            .await?
            .map(|r| r.try_get_by_index::<i64>(0))
            .transpose()?
            .unwrap_or(0))
    }

    async fn touch(&self) -> ProjectionResult<()> {
        self.db
            .execute(stmt(
                "UPDATE platform_identity_projection_state SET last_synced_at = $1 WHERE id = 1",
                vec![self.clock.now().into()],
            ))
            .await?;
        Ok(())
    }

    /// Runs [`sync`](Self::sync) forever on `interval` (every instance; the cursor row
    /// lock makes that safe).
    pub async fn run(self: Arc<Self>, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = self.sync().await {
                crate::metrics::inc("calling_identity_projection_failures_total", &[]);
                tracing::warn!(error = %e, "identity projection sync failed");
            }
        }
    }
}

fn snapshot<T: DeserializeOwned>(change: &IdentityChange) -> ProjectionResult<T> {
    serde_json::from_value(change.snapshot.clone()).map_err(|_| ProjectionError::InvalidSnapshot {
        sequence: change.sequence,
        kind: change.entity_kind.clone(),
    })
}

/// Applies one feed change inside `txn`.
pub async fn apply_change(
    txn: &DatabaseTransaction,
    change: &IdentityChange,
    now: DateTime<Utc>,
) -> ProjectionResult<()> {
    let delete = change.operation == "DELETE";
    match change.entity_kind.as_str() {
        "TENANT" if !delete => upsert_tenant(txn, &snapshot(change)?).await,
        "APPLICATION" if !delete => upsert_application(txn, &snapshot(change)?).await,
        "ENVIRONMENT" if !delete => upsert_environment(txn, &snapshot(change)?).await,
        "SERVICE_ACCOUNT" if !delete => upsert_service_account(txn, &snapshot(change)?).await,
        "CREDENTIAL" if !delete => upsert_credential(txn, &snapshot(change)?).await,
        "PRINCIPAL" if !delete => upsert_principal(txn, &snapshot(change)?, now)
            .await
            .map(|_| ()),
        "APPLICATION_SCOPE" => {
            let s: ApplicationScopeSnapshot = snapshot(change)?;
            if delete {
                // Cascades to the credential scopes of the application (foreign key).
                txn.execute(stmt(
                    "DELETE FROM platform_application_scopes WHERE application_id = $1 AND scope = $2",
                    vec![s.application_id.into(), s.scope.into()],
                ))
                .await?;
            } else {
                txn.execute(stmt(
                    "INSERT INTO platform_application_scopes (tenant_id, application_id, scope, granted_at) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT (application_id, scope) DO NOTHING",
                    vec![s.tenant_id.into(), s.application_id.into(), s.scope.into(), s.granted_at.into()],
                ))
                .await?;
            }
            Ok(())
        }
        "CREDENTIAL_SCOPE" => {
            let s: CredentialScopeSnapshot = snapshot(change)?;
            if delete {
                txn.execute(stmt(
                    "DELETE FROM platform_api_credential_scopes WHERE credential_id = $1 AND scope = $2",
                    vec![s.credential_id.into(), s.scope.into()],
                ))
                .await?;
            } else {
                txn.execute(stmt(
                    "INSERT INTO platform_api_credential_scopes (credential_id, tenant_id, application_id, scope, granted_at) \
                     VALUES ($1, $2, $3, $4, $5) ON CONFLICT (credential_id, scope) DO NOTHING",
                    vec![
                        s.credential_id.into(),
                        s.tenant_id.into(),
                        s.application_id.into(),
                        s.scope.into(),
                        s.granted_at.into(),
                    ],
                ))
                .await?;
            }
            Ok(())
        }
        other => {
            // Platform Access never deletes identity entities; an unknown kind is a newer
            // contract this Calling does not project yet.
            tracing::warn!(kind = other, operation = %change.operation, sequence = change.sequence, "identity change not projected");
            Ok(())
        }
    }
}

async fn upsert_tenant<C: ConnectionTrait>(db: &C, t: &TenantSnapshot) -> ProjectionResult<()> {
    db.execute(stmt(
        "INSERT INTO platform_tenants (id, slug, display_name, classification, status, billing_currency, \
           created_at, updated_at, pa_revision) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name, status = EXCLUDED.status, \
           updated_at = EXCLUDED.updated_at, pa_revision = EXCLUDED.pa_revision \
         WHERE platform_tenants.pa_revision < EXCLUDED.pa_revision",
        vec![
            t.id.into(),
            t.slug.clone().into(),
            t.display_name.clone().into(),
            t.classification.as_str().into(),
            t.status.as_str().into(),
            t.billing_currency.clone().into(),
            t.created_at.into(),
            t.updated_at.into(),
            t.revision.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn upsert_application<C: ConnectionTrait>(
    db: &C,
    a: &ApplicationSnapshot,
) -> ProjectionResult<()> {
    db.execute(stmt(
        "INSERT INTO platform_applications (id, tenant_id, slug, display_name, status, created_at, \
           updated_at, pa_revision) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name, status = EXCLUDED.status, \
           updated_at = EXCLUDED.updated_at, pa_revision = EXCLUDED.pa_revision \
         WHERE platform_applications.pa_revision < EXCLUDED.pa_revision",
        vec![
            a.id.into(),
            a.tenant_id.into(),
            a.slug.clone().into(),
            a.display_name.clone().into(),
            a.status.as_str().into(),
            a.created_at.into(),
            a.updated_at.into(),
            a.revision.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn upsert_environment<C: ConnectionTrait>(
    db: &C,
    e: &EnvironmentSnapshot,
) -> ProjectionResult<()> {
    // Defence in depth: never project live telecom outside PRODUCTION (the CHECK refuses
    // it too).
    let live = e.live_telecom_allowed && e.kind.may_allow_live_telecom();
    db.execute(stmt(
        "INSERT INTO platform_environments (id, tenant_id, application_id, kind, status, live_telecom_allowed, \
           created_at, updated_at, pa_revision) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, \
           live_telecom_allowed = EXCLUDED.live_telecom_allowed, updated_at = EXCLUDED.updated_at, \
           pa_revision = EXCLUDED.pa_revision \
         WHERE platform_environments.pa_revision < EXCLUDED.pa_revision",
        vec![
            e.id.into(),
            e.tenant_id.into(),
            e.application_id.into(),
            e.kind.as_str().into(),
            e.status.as_str().into(),
            live.into(),
            e.created_at.into(),
            e.updated_at.into(),
            e.revision.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn upsert_service_account<C: ConnectionTrait>(
    db: &C,
    s: &ServiceAccountSnapshot,
) -> ProjectionResult<()> {
    db.execute(stmt(
        "INSERT INTO platform_service_accounts (id, tenant_id, application_id, environment_id, name, \
           description, status, revoked_at, created_at, updated_at, pa_revision) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (id) DO UPDATE SET description = EXCLUDED.description, status = EXCLUDED.status, \
           revoked_at = EXCLUDED.revoked_at, updated_at = EXCLUDED.updated_at, \
           pa_revision = EXCLUDED.pa_revision \
         WHERE platform_service_accounts.pa_revision < EXCLUDED.pa_revision",
        vec![
            s.id.into(),
            s.tenant_id.into(),
            s.application_id.into(),
            s.environment_id.into(),
            s.name.clone().into(),
            s.description.clone().into(),
            s.status.as_str().into(),
            s.revoked_at.into(),
            s.created_at.into(),
            s.updated_at.into(),
            s.revision.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn upsert_credential<C: ConnectionTrait>(
    db: &C,
    c: &CredentialSnapshot,
) -> ProjectionResult<()> {
    let inserted = db
        .query_one(stmt(
            "INSERT INTO platform_api_credentials (id, public_id, tenant_id, application_id, environment_id, \
               service_account_id, label, last_four, status, not_before, expires_at, revoked_at, \
               rotated_from_credential_id, superseded_by_credential_id, superseded_at, \
               supersession_valid_until, created_at, updated_at, pa_revision) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) \
             ON CONFLICT (id) DO UPDATE SET label = EXCLUDED.label, status = EXCLUDED.status, \
               revoked_at = EXCLUDED.revoked_at, \
               superseded_by_credential_id = EXCLUDED.superseded_by_credential_id, \
               superseded_at = EXCLUDED.superseded_at, \
               supersession_valid_until = EXCLUDED.supersession_valid_until, \
               updated_at = EXCLUDED.updated_at, pa_revision = EXCLUDED.pa_revision \
             WHERE platform_api_credentials.pa_revision < EXCLUDED.pa_revision \
             RETURNING (xmax = 0) AS inserted",
            vec![
                c.id.into(),
                c.public_id.clone().into(),
                c.tenant_id.into(),
                c.application_id.into(),
                c.environment_id.into(),
                c.service_account_id.into(),
                c.label.clone().into(),
                c.last_four.clone().into(),
                c.status.as_str().into(),
                c.not_before.into(),
                c.expires_at.into(),
                c.revoked_at.into(),
                c.rotated_from_credential_id.into(),
                c.superseded_by_credential_id.into(),
                c.superseded_at.into(),
                c.supersession_valid_until.into(),
                c.created_at.into(),
                c.updated_at.into(),
                c.revision.into(),
            ],
        ))
        .await?
        .map(|r| r.try_get::<bool>("", "inserted"))
        .transpose()?
        .unwrap_or(false);
    if let (true, Some(predecessor)) = (inserted, c.rotated_from_credential_id) {
        // Part G §70: a rotation successor keeps its predecessor's credential limits.
        db.execute(stmt(
            "INSERT INTO platform_credential_limits (credential_id, tenant_id, application_id, \
               requests_per_minute, max_concurrent_communications, telecom_spend_daily_limit_minor, \
               telecom_spend_monthly_limit_minor, created_at, updated_at) \
             SELECT $1, l.tenant_id, l.application_id, l.requests_per_minute, l.max_concurrent_communications, \
                    l.telecom_spend_daily_limit_minor, l.telecom_spend_monthly_limit_minor, $3, $3 \
             FROM platform_credential_limits l WHERE l.credential_id = $2 \
             ON CONFLICT (credential_id) DO NOTHING",
            vec![c.id.into(), predecessor.into(), c.created_at.into()],
        ))
        .await?;
    }
    Ok(())
}

/// Applies a principal snapshot inside `db` (a transaction). Returns how many
/// participations a tombstone minimized (0 otherwise).
pub(crate) async fn upsert_principal<C: ConnectionTrait>(
    db: &C,
    p: &PrincipalSnapshot,
    now: DateTime<Utc>,
) -> ProjectionResult<u64> {
    db.execute(stmt(
        "INSERT INTO platform_principals (id, tenant_id, application_id, environment_id, kind, \
           external_reference, linked_service_account_id, status, tombstoned_at, created_at, \
           updated_at, pa_revision) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, \
           external_reference = EXCLUDED.external_reference, tombstoned_at = EXCLUDED.tombstoned_at, \
           updated_at = EXCLUDED.updated_at, pa_revision = EXCLUDED.pa_revision \
         WHERE platform_principals.pa_revision < EXCLUDED.pa_revision",
        vec![
            p.id.into(),
            p.tenant_id.into(),
            p.application_id.into(),
            p.environment_id.into(),
            p.kind.as_str().into(),
            p.external_reference.clone().into(),
            p.linked_service_account_id.into(),
            p.status.as_str().into(),
            p.tombstoned_at.into(),
            p.created_at.into(),
            p.updated_at.into(),
            p.revision.into(),
        ],
    ))
    .await?;
    if p.tombstoned_at.is_some() {
        return minimize_participations(db, p, now).await;
    }
    Ok(0)
}

/// Part H §80.42: a tombstoned principal's participations lose their display names
/// (idempotent) — unless a Calling retention hold covers the principal.
async fn minimize_participations<C: ConnectionTrait>(
    db: &C,
    p: &PrincipalSnapshot,
    now: DateTime<Utc>,
) -> ProjectionResult<u64> {
    use crate::retention::{holds, DataClass};
    let owner = holds::HoldOwner {
        tenant_id: p.tenant_id,
        application_id: p.application_id,
        environment_id: p.environment_id,
    };
    let held = holds::is_held(db, DataClass::Principal, Some(p.id), Some(owner))
        .await
        .map_err(|e| ProjectionError::Database(e.to_string()))?;
    if held {
        tracing::warn!(principal_id = %p.id, "principal tombstoned while a retention hold covers it; participations kept");
        return Ok(0);
    }
    db.execute_unprepared("SET LOCAL calling.retention_worker = 'on'")
        .await?;
    let minimized = db
        .execute(stmt(
            "UPDATE call_participants SET display_name = NULL, display_minimized_at = $2 \
             WHERE principal_id = $1 AND display_minimized_at IS NULL",
            vec![p.id.into(), now.into()],
        ))
        .await?
        .rows_affected();
    db.execute_unprepared("SET LOCAL calling.retention_worker = 'off'")
        .await?;
    Ok(minimized)
}
