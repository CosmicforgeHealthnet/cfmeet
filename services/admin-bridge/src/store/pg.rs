//! PostgreSQL [`Store`] over the bridge's own database (`cfmeet_admin_bridge`, migration
//! crate `admin-bridge-migration`). Raw parameterized SQL only; driver errors become
//! [`StoreError::Unavailable`] and no value is ever logged.

use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, SubsecRound, Utc};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr, QueryResult,
    Statement, TransactionTrait, Value as DbValue,
};
use serde_json::Value;

use super::*;

pub use admin_bridge_migration::AUDIT_LOCK_KEY;

fn unavailable(e: DbErr) -> StoreError {
    StoreError::Unavailable(Box::new(e))
}

fn stmt(sql: &str, values: Vec<DbValue>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn get<T: sea_orm::TryGetable>(row: &QueryResult, col: &str) -> StoreResult<T> {
    row.try_get::<T>("", col)
        .map_err(|_| StoreError::Invalid("unexpected column type"))
}

#[derive(Clone)]
pub struct PgStore {
    db: DatabaseConnection,
}

impl PgStore {
    /// Connects with a bounded pool (1–200 connections). Statement logging is off: no
    /// parameter value ever reaches a log.
    pub async fn connect(url: &str, max_connections: u32) -> StoreResult<Self> {
        let mut opts = ConnectOptions::new(url.to_owned());
        opts.max_connections(max_connections.clamp(1, 200))
            .min_connections(1)
            .connect_timeout(StdDuration::from_secs(5))
            .acquire_timeout(StdDuration::from_secs(5))
            .sqlx_logging(false);
        Database::connect(opts)
            .await
            .map(Self::from_connection)
            .map_err(unavailable)
    }

    pub fn from_connection(db: DatabaseConnection) -> Self {
        Self { db }
    }

    pub fn connection(&self) -> &DatabaseConnection {
        &self.db
    }
}

fn principal_from_row(row: &QueryResult) -> StoreResult<Principal> {
    let kind: String = get(row, "kind")?;
    Ok(Principal {
        principal: get(row, "principal")?,
        kind: PrincipalKind::parse(&kind).ok_or(StoreError::Invalid("principal kind"))?,
        key_id: get(row, "key_id")?,
        material: get(row, "material")?,
        issuer: get(row, "issuer")?,
        audience: get(row, "audience")?,
        not_before: get(row, "not_before")?,
        expires_at: get(row, "expires_at")?,
        revoked_at: get(row, "revoked_at")?,
    })
}

fn json_text(v: &Value) -> String {
    serde_json::to_string(v).expect("serializable")
}

#[async_trait]
impl Store for PgStore {
    async fn ping(&self) -> StoreResult<()> {
        self.db
            .execute(stmt("SELECT 1", vec![]))
            .await
            .map(|_| ())
            .map_err(unavailable)
    }

    async fn principal(&self, kind: PrincipalKind, key_id: &str) -> StoreResult<Option<Principal>> {
        let row = self
            .db
            .query_one(stmt(
                "SELECT principal, kind, key_id, material, issuer, audience, not_before, expires_at, revoked_at \
                 FROM admin_remote_principals WHERE kind = $1 AND key_id = $2",
                vec![kind.as_str().into(), key_id.into()],
            ))
            .await
            .map_err(unavailable)?;
        row.as_ref().map(principal_from_row).transpose()
    }

    async fn is_revoked(
        &self,
        sub: &str,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let row = self
            .db
            .query_one(stmt(
                "SELECT EXISTS (SELECT 1 FROM admin_revocations WHERE revoked_at <= $3 \
                 AND ((sub = $1 AND session_id IS NULL) OR session_id = $2)) AS revoked",
                vec![sub.into(), session_id.into(), now.into()],
            ))
            .await
            .map_err(unavailable)?
            .ok_or(StoreError::Invalid("no row"))?;
        get(&row, "revoked")
    }

    async fn claim_once(&self, claims: &[Claim]) -> StoreResult<ClaimOutcome> {
        let txn = self.db.begin().await.map_err(unavailable)?;
        for c in claims {
            let res = txn
                .execute(stmt(
                    "INSERT INTO admin_security_claims (kind, value, keep_until) VALUES ($1, $2, $3) \
                     ON CONFLICT (kind, value) DO NOTHING",
                    vec![c.kind.as_str().into(), c.value.clone().into(), c.keep_until.into()],
                ))
                .await
                .map_err(unavailable)?;
            if res.rows_affected() == 0 {
                txn.rollback().await.map_err(unavailable)?;
                return Ok(ClaimOutcome::Replay(c.kind));
            }
        }
        txn.commit().await.map_err(unavailable)?;
        Ok(ClaimOutcome::Recorded)
    }

    async fn rate_allow(&self, key: &RateKey, limit: u32) -> StoreResult<bool> {
        let row = self
            .db
            .query_one(stmt(
                "INSERT INTO admin_rate_limits (bucket, window_start, count) VALUES ($1, $2, 1) \
                 ON CONFLICT (bucket, window_start) DO UPDATE SET count = admin_rate_limits.count + 1 \
                 RETURNING count",
                vec![key.bucket.clone().into(), key.window_start.into()],
            ))
            .await
            .map_err(unavailable)?
            .ok_or(StoreError::Invalid("no row"))?;
        let count: i32 = get(&row, "count")?;
        Ok(i64::from(count) <= i64::from(limit))
    }

    async fn policy_revision(&self, revision: i64) -> StoreResult<Option<PolicyRow>> {
        let row = self
            .db
            .query_one(stmt(
                "SELECT revision, document, signature, key_id FROM admin_policy_revisions WHERE revision = $1",
                vec![revision.into()],
            ))
            .await
            .map_err(unavailable)?;
        row.map(|r| {
            Ok(PolicyRow {
                revision: get(&r, "revision")?,
                document: get(&r, "document")?,
                signature: get(&r, "signature")?,
                key_id: get(&r, "key_id")?,
            })
        })
        .transpose()
    }

    async fn append_audit(&self, draft: &AuditDraft) -> StoreResult<AuditReceipt> {
        // Timestamps are stored with microsecond precision; hash exactly what is stored.
        let mut draft = draft.clone();
        draft.occurred_at = draft.occurred_at.trunc_subsecs(6);
        let txn = self.db.begin().await.map_err(unavailable)?;
        txn.execute(stmt(
            "SELECT pg_advisory_xact_lock($1)",
            vec![AUDIT_LOCK_KEY.into()],
        ))
        .await
        .map_err(unavailable)?;
        let last = txn
            .query_one(stmt(
                "SELECT entry_hash FROM admin_audit_entries ORDER BY sequence DESC LIMIT 1",
                vec![],
            ))
            .await
            .map_err(unavailable)?;
        let previous: String = match last {
            Some(row) => get(&row, "entry_hash")?,
            None => GENESIS_HASH.to_string(),
        };
        let entry_hash = chain_hash(&previous, &draft);
        let row = txn
            .query_one(stmt(
                "INSERT INTO admin_audit_entries (entry_hash, previous_hash, correlation_id, occurred_at, actor, \
                 session_id, capability, method, path, purpose, scope, decision, http_status, request_digest, \
                 approval_id, approver, detail) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::json, $12, $13, $14, $15, $16, $17::json) \
                 RETURNING sequence",
                vec![
                    entry_hash.clone().into(),
                    previous.into(),
                    draft.correlation_id.into(),
                    draft.occurred_at.into(),
                    draft.actor.clone().into(),
                    draft.session_id.clone().into(),
                    draft.capability.clone().into(),
                    draft.method.clone().into(),
                    draft.path.clone().into(),
                    draft.purpose.clone().into(),
                    json_text(&draft.scope).into(),
                    draft.decision.clone().into(),
                    i32::from(draft.http_status).into(),
                    draft.request_digest.clone().into(),
                    draft.approval_id.clone().into(),
                    draft.approver.clone().into(),
                    json_text(&draft.detail).into(),
                ],
            ))
            .await
            .map_err(unavailable)?
            .ok_or(StoreError::Invalid("no row"))?;
        let sequence: i64 = get(&row, "sequence")?;
        txn.commit().await.map_err(unavailable)?;
        Ok(AuditReceipt {
            sequence,
            entry_hash,
        })
    }

    async fn dataset_days(
        &self,
        dataset_id: &str,
        start: NaiveDate,
        end: NaiveDate,
        limit: u32,
    ) -> StoreResult<Vec<DatasetDay>> {
        let rows = self
            .db
            .query_all(stmt(
                "SELECT dataset_id, day, metrics, to_jsonb(sources_reported) AS sources, updated_at \
                 FROM admin_dataset_days WHERE dataset_id = $1 AND day BETWEEN $2 AND $3 \
                 ORDER BY day ASC LIMIT $4",
                vec![dataset_id.into(), start.into(), end.into(), i64::from(limit).into()],
            ))
            .await
            .map_err(unavailable)?;
        rows.iter()
            .map(|r| {
                let metrics = match get::<Value>(r, "metrics")? {
                    Value::Object(m) => m,
                    _ => return Err(StoreError::Invalid("metrics is not an object")),
                };
                let sources: Value = get(r, "sources")?;
                let sources_reported = sources
                    .as_array()
                    .ok_or(StoreError::Invalid("sources"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or(StoreError::Invalid("sources"))
                    })
                    .collect::<StoreResult<Vec<_>>>()?;
                Ok(DatasetDay {
                    dataset_id: get(r, "dataset_id")?,
                    day: get(r, "day")?,
                    metrics,
                    sources_reported,
                    updated_at: get(r, "updated_at")?,
                })
            })
            .collect()
    }

    async fn projections(&self) -> StoreResult<Vec<ProjectionRow>> {
        let rows = self
            .db
            .query_all(stmt(
                "SELECT name, last_event_at, open_gaps, quarantined, updated_at FROM admin_projections ORDER BY name",
                vec![],
            ))
            .await
            .map_err(unavailable)?;
        rows.iter()
            .map(|r| {
                Ok(ProjectionRow {
                    name: get(r, "name")?,
                    last_event_at: get(r, "last_event_at")?,
                    open_gaps: get(r, "open_gaps")?,
                    quarantined: get(r, "quarantined")?,
                    updated_at: get(r, "updated_at")?,
                })
            })
            .collect()
    }

    async fn cleanup(&self, now: DateTime<Utc>) -> StoreResult<CleanupStats> {
        let claims = self
            .db
            .execute(stmt(
                "DELETE FROM admin_security_claims WHERE keep_until <= $1",
                vec![now.into()],
            ))
            .await
            .map_err(unavailable)?
            .rows_affected();
        let rate_buckets = self
            .db
            .execute(stmt(
                "DELETE FROM admin_rate_limits WHERE window_start <= $1",
                vec![(now - Duration::hours(1)).into()],
            ))
            .await
            .map_err(unavailable)?
            .rows_affected();
        Ok(CleanupStats {
            claims,
            rate_buckets,
        })
    }
}

/// One stored audit row, as the chain verifier reads it back.
pub struct StoredAuditEntry {
    pub sequence: i64,
    pub entry_hash: String,
    pub previous_hash: String,
    pub draft: AuditDraft,
}

impl PgStore {
    /// Audit rows with `sequence > after`, ascending, at most `limit` (chain verification).
    pub async fn audit_page(&self, after: i64, limit: u32) -> StoreResult<Vec<StoredAuditEntry>> {
        let rows = self
            .db
            .query_all(stmt(
                "SELECT sequence, entry_hash, previous_hash, correlation_id, occurred_at, actor, session_id, \
                 capability, method, path, purpose, scope::text AS scope, decision, http_status, request_digest, \
                 approval_id, approver, detail::text AS detail \
                 FROM admin_audit_entries WHERE sequence > $1 ORDER BY sequence ASC LIMIT $2",
                vec![after.into(), i64::from(limit).into()],
            ))
            .await
            .map_err(unavailable)?;
        rows.iter()
            .map(|r| {
                let parse = |col: &str| -> StoreResult<Value> {
                    serde_json::from_str(&get::<String>(r, col)?)
                        .map_err(|_| StoreError::Invalid("audit json"))
                };
                let status: i32 = get(r, "http_status")?;
                Ok(StoredAuditEntry {
                    sequence: get(r, "sequence")?,
                    entry_hash: get(r, "entry_hash")?,
                    previous_hash: get(r, "previous_hash")?,
                    draft: AuditDraft {
                        correlation_id: get(r, "correlation_id")?,
                        occurred_at: get(r, "occurred_at")?,
                        actor: get(r, "actor")?,
                        session_id: get(r, "session_id")?,
                        capability: get(r, "capability")?,
                        method: get(r, "method")?,
                        path: get(r, "path")?,
                        purpose: get(r, "purpose")?,
                        scope: parse("scope")?,
                        decision: get(r, "decision")?,
                        http_status: u16::try_from(status)
                            .map_err(|_| StoreError::Invalid("status"))?,
                        request_digest: get(r, "request_digest")?,
                        approval_id: get(r, "approval_id")?,
                        approver: get(r, "approver")?,
                        detail: parse("detail")?,
                    },
                })
            })
            .collect()
    }
}

/// Recomputes the chain over every stored row. `Ok(n)`: n rows verified; `Err(seq)`: the
/// first sequence whose hash or link is wrong.
pub async fn verify_audit_chain(store: &PgStore) -> StoreResult<Result<u64, i64>> {
    let mut previous = GENESIS_HASH.to_string();
    let mut after = 0;
    let mut verified = 0u64;
    loop {
        let page = store.audit_page(after, 500).await?;
        if page.is_empty() {
            return Ok(Ok(verified));
        }
        for e in page {
            if e.previous_hash != previous || chain_hash(&previous, &e.draft) != e.entry_hash {
                return Ok(Err(e.sequence));
            }
            previous = e.entry_hash;
            after = e.sequence;
            verified += 1;
        }
    }
}
