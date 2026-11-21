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

pub use admin_bridge_migration::{AUDIT_LOCK_KEY, EVENTS_LOCK_KEY};

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

const OPERATION_COLUMNS: &str = "principal_sub, idempotency_key, operation_id, capability, owner, purpose, \
     scope, intent_sha256, target, expected_version, approval_id, correlation_id, status, source_status, \
     source_version, error_code, created_at, sent_at, settled_at";

fn operation_row(r: &QueryResult) -> StoreResult<Operation> {
    let status: String = get(r, "status")?;
    Ok(Operation {
        principal_sub: get(r, "principal_sub")?,
        idempotency_key: get(r, "idempotency_key")?,
        operation_id: get(r, "operation_id")?,
        capability: get(r, "capability")?,
        owner: get(r, "owner")?,
        purpose: get(r, "purpose")?,
        scope: get(r, "scope")?,
        intent_sha256: get(r, "intent_sha256")?,
        target: get(r, "target")?,
        expected_version: get(r, "expected_version")?,
        approval_id: get(r, "approval_id")?,
        correlation_id: get(r, "correlation_id")?,
        status: OpStatus::parse(&status).ok_or(StoreError::Invalid("operation status"))?,
        source_status: get(r, "source_status")?,
        source_version: get(r, "source_version")?,
        error_code: get(r, "error_code")?,
        created_at: get(r, "created_at")?,
        sent_at: get(r, "sent_at")?,
        settled_at: get(r, "settled_at")?,
    })
}

const ACTIVATION_COLUMNS: &str =
    "request_id, operation, release_sha, confirm, requested_by, approver, \
     approval_id, state, created_at, dispatched_at, run_id";

fn activation_row(r: &QueryResult) -> StoreResult<ActivationRequest> {
    Ok(ActivationRequest {
        request_id: get(r, "request_id")?,
        operation: get(r, "operation")?,
        release_sha: get(r, "release_sha")?,
        confirm: get(r, "confirm")?,
        requested_by: get(r, "requested_by")?,
        approver: get(r, "approver")?,
        approval_id: get(r, "approval_id")?,
        state: get(r, "state")?,
        created_at: get(r, "created_at")?,
        dispatched_at: get(r, "dispatched_at")?,
        run_id: get(r, "run_id")?,
    })
}

fn op_values(op: &Operation, created_at: DateTime<Utc>) -> Vec<DbValue> {
    vec![
        op.principal_sub.clone().into(),
        op.idempotency_key.into(),
        op.operation_id.into(),
        op.capability.clone().into(),
        op.owner.clone().into(),
        op.purpose.clone().into(),
        json_text(&op.scope).into(),
        op.intent_sha256.clone().into(),
        op.target.clone().into(),
        op.expected_version.clone().into(),
        op.approval_id.clone().into(),
        op.correlation_id.into(),
        created_at.into(),
    ]
}

const INSERT_PENDING_OPERATION: &str = "INSERT INTO admin_operations (principal_sub, idempotency_key, operation_id, \
     capability, owner, purpose, scope, intent_sha256, target, expected_version, approval_id, correlation_id, status, \
     created_at) VALUES ($1, $2, $3, $4, $5, $6, $7::json, $8, $9, $10, $11, $12, 'PENDING', $13) ON CONFLICT DO NOTHING";

/// The statuses an operation may move to `status` from (the ledger trigger's transitions).
fn settle_from(status: OpStatus) -> &'static [&'static str] {
    match status {
        OpStatus::Sent => &["PENDING"],
        OpStatus::UnknownOutcome => &["SENT"],
        OpStatus::Succeeded => &["SENT", "UNKNOWN_OUTCOME"],
        OpStatus::Failed => &["PENDING", "SENT", "UNKNOWN_OUTCOME"],
        OpStatus::Pending => &[],
    }
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

    async fn merge_dataset_day(
        &self,
        dataset_id: &str,
        day: NaiveDate,
        metrics: &serde_json::Map<String, Value>,
        sources: &[String],
        at: DateTime<Utc>,
    ) -> StoreResult<()> {
        let sources = Value::from(sources.to_vec());
        self.db
            .execute(stmt(
                "INSERT INTO admin_dataset_days (dataset_id, day, metrics, sources_reported, updated_at) \
                 VALUES ($1, $2, $3::jsonb, ARRAY(SELECT DISTINCT s FROM jsonb_array_elements_text($4::jsonb) AS s ORDER BY s), $5) \
                 ON CONFLICT (dataset_id, day) DO UPDATE SET \
                   metrics = admin_dataset_days.metrics || EXCLUDED.metrics, \
                   sources_reported = ARRAY(SELECT DISTINCT s FROM \
                     unnest(admin_dataset_days.sources_reported || EXCLUDED.sources_reported) AS s ORDER BY s), \
                   updated_at = GREATEST(admin_dataset_days.updated_at, EXCLUDED.updated_at)",
                vec![
                    dataset_id.into(),
                    day.into(),
                    json_text(&Value::Object(metrics.clone())).into(),
                    json_text(&sources).into(),
                    at.into(),
                ],
            ))
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn event_states(
        &self,
        event_type: &str,
        end: DateTime<Utc>,
    ) -> StoreResult<Vec<EventStateCount>> {
        let rows = self
            .db
            .query_all(stmt(
                "SELECT producer, state, count(*) AS subjects FROM ( \
                   SELECT DISTINCT ON (producer, subject_kind, subject_id) producer, state \
                   FROM admin_hq_outbox WHERE event_type = $1 AND occurred_at < $2 \
                   ORDER BY producer, subject_kind, subject_id, version DESC) latest \
                 GROUP BY producer, state ORDER BY producer, state",
                vec![event_type.into(), end.into()],
            ))
            .await
            .map_err(unavailable)?;
        rows.iter()
            .map(|r| {
                Ok(EventStateCount {
                    producer: get(r, "producer")?,
                    state: get(r, "state")?,
                    subjects: get(r, "subjects")?,
                })
            })
            .collect()
    }

    async fn quarantined_between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> StoreResult<i64> {
        let row = self
            .db
            .query_one(stmt(
                "SELECT count(*) AS n FROM admin_event_inbox \
                 WHERE disposition = 'QUARANTINED' AND received_at >= $1 AND received_at < $2",
                vec![start.into(), end.into()],
            ))
            .await
            .map_err(unavailable)?
            .ok_or(StoreError::Invalid("count"))?;
        get(&row, "n")
    }

    async fn audit_window(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        after: Option<i64>,
        limit: u32,
    ) -> StoreResult<Vec<(AuditDraft, AuditReceipt)>> {
        let rows = self
            .db
            .query_all(stmt(
                "SELECT sequence, entry_hash, correlation_id, occurred_at, actor, session_id, capability, method, path,                  purpose, scope::text AS scope, decision, http_status, request_digest, approval_id, approver,                  detail::text AS detail FROM admin_audit_entries                  WHERE occurred_at >= $1 AND occurred_at < $2 AND ($3::bigint IS NULL OR sequence > $3)                  ORDER BY sequence ASC LIMIT $4",
                vec![start.into(), end.into(), after.into(), i64::from(limit).into()],
            ))
            .await
            .map_err(unavailable)?;
        rows.iter()
            .map(|r| {
                let scope: String = get(r, "scope")?;
                let detail: String = get(r, "detail")?;
                let status: i32 = get(r, "http_status")?;
                Ok((
                    AuditDraft {
                        correlation_id: get(r, "correlation_id")?,
                        occurred_at: get(r, "occurred_at")?,
                        actor: get(r, "actor")?,
                        session_id: get(r, "session_id")?,
                        capability: get(r, "capability")?,
                        method: get(r, "method")?,
                        path: get(r, "path")?,
                        purpose: get(r, "purpose")?,
                        scope: serde_json::from_str(&scope)
                            .map_err(|_| StoreError::Invalid("scope"))?,
                        decision: get(r, "decision")?,
                        http_status: u16::try_from(status)
                            .map_err(|_| StoreError::Invalid("status"))?,
                        request_digest: get(r, "request_digest")?,
                        approval_id: get(r, "approval_id")?,
                        approver: get(r, "approver")?,
                        detail: serde_json::from_str(&detail)
                            .map_err(|_| StoreError::Invalid("detail"))?,
                    },
                    AuditReceipt {
                        sequence: get(r, "sequence")?,
                        entry_hash: get(r, "entry_hash")?,
                    },
                ))
            })
            .collect()
    }

    async fn begin_operation(&self, op: &Operation) -> StoreResult<BeginOutcome> {
        let inserted = self
            .db
            .execute(stmt(
                INSERT_PENDING_OPERATION,
                op_values(op, op.created_at.trunc_subsecs(6)),
            ))
            .await
            .map_err(unavailable)?
            .rows_affected();
        if inserted == 1 {
            return Ok(BeginOutcome::Started);
        }
        match self
            .operation(&op.principal_sub, op.idempotency_key)
            .await?
        {
            Some(existing) => Ok(BeginOutcome::Existing(Box::new(existing))),
            None => Ok(BeginOutcome::VersionTaken),
        }
    }

    async fn mark_sent(&self, sub: &str, key: uuid::Uuid, at: DateTime<Utc>) -> StoreResult<bool> {
        let n = self
            .db
            .execute(stmt(
                "UPDATE admin_operations SET status = 'SENT', sent_at = $3 \
                 WHERE principal_sub = $1 AND idempotency_key = $2 AND status = 'PENDING'",
                vec![sub.into(), key.into(), at.trunc_subsecs(6).into()],
            ))
            .await
            .map_err(unavailable)?
            .rows_affected();
        Ok(n == 1)
    }

    async fn settle_operation(
        &self,
        sub: &str,
        key: uuid::Uuid,
        st: &Settlement,
    ) -> StoreResult<bool> {
        let from = settle_from(st.status);
        if from.is_empty() {
            return Ok(false);
        }
        let settled_at = st.status.settled().then(|| st.at.trunc_subsecs(6));
        let n = self
            .db
            .execute(stmt(
                "UPDATE admin_operations SET status = $3, source_status = $4, source_version = $5, \
                 error_code = $6, settled_at = $7 \
                 WHERE principal_sub = $1 AND idempotency_key = $2 AND status = ANY($8)",
                vec![
                    sub.into(),
                    key.into(),
                    st.status.as_str().into(),
                    st.source_status.clone().into(),
                    st.source_version.clone().into(),
                    st.error_code.clone().into(),
                    settled_at.into(),
                    from.iter().map(|s| s.to_string()).collect::<Vec<String>>().into(),
                ],
            ))
            .await
            .map_err(unavailable)?
            .rows_affected();
        Ok(n == 1)
    }

    async fn operation(&self, sub: &str, key: uuid::Uuid) -> StoreResult<Option<Operation>> {
        let row = self
            .db
            .query_one(stmt(
                &format!("SELECT {OPERATION_COLUMNS} FROM admin_operations WHERE principal_sub = $1 AND idempotency_key = $2"),
                vec![sub.into(), key.into()],
            ))
            .await
            .map_err(unavailable)?;
        row.as_ref().map(operation_row).transpose()
    }

    async fn open_operations(
        &self,
        before: DateTime<Utc>,
        limit: u32,
    ) -> StoreResult<Vec<Operation>> {
        let rows = self
            .db
            .query_all(stmt(
                &format!(
                    "SELECT {OPERATION_COLUMNS} FROM admin_operations \
                     WHERE status IN ('PENDING', 'SENT', 'UNKNOWN_OUTCOME') AND created_at < $1 \
                     ORDER BY created_at ASC LIMIT $2"
                ),
                vec![before.into(), i64::from(limit).into()],
            ))
            .await
            .map_err(unavailable)?;
        rows.iter().map(operation_row).collect()
    }

    async fn local_command(
        &self,
        op: &Operation,
        change: &LocalChange,
    ) -> StoreResult<LocalOutcome> {
        let now = op.created_at.trunc_subsecs(6);
        let txn = self.db.begin().await.map_err(unavailable)?;
        let inserted = txn
            .execute(stmt(INSERT_PENDING_OPERATION, op_values(op, now)))
            .await
            .map_err(unavailable)?
            .rows_affected();
        if inserted != 1 {
            txn.rollback().await.map_err(unavailable)?;
            return match self
                .operation(&op.principal_sub, op.idempotency_key)
                .await?
            {
                Some(existing) => Ok(LocalOutcome::Existing(Box::new(existing))),
                None => Ok(LocalOutcome::VersionTaken),
            };
        }
        let actor = format!("hq:{}", op.principal_sub);
        let applied: Result<(String, i64), &'static str> = match change {
            LocalChange::PrivacyRegister { kind, subject_ref } => {
                let n = txn
                    .execute(stmt(
                        "INSERT INTO admin_privacy_requests (request_id, kind, subject_ref, state, created_by, \
                         created_at, updated_at, version) VALUES ($1, $2, $3, 'OPEN', $4, $5, $5, 1) \
                         ON CONFLICT DO NOTHING",
                        vec![op.operation_id.into(), kind.clone().into(), (*subject_ref).into(), actor.clone().into(), now.into()],
                    ))
                    .await
                    .map_err(unavailable)?
                    .rows_affected();
                if n == 1 {
                    for service in PRIVACY_SERVICES {
                        txn.execute(stmt(
                            "INSERT INTO admin_privacy_request_services (request_id, service, outcome) VALUES ($1, $2, 'PENDING')",
                            vec![op.operation_id.into(), service.into()],
                        ))
                        .await
                        .map_err(unavailable)?;
                    }
                    Ok(("OPEN".into(), 1))
                } else {
                    Err("VERSION_CONFLICT")
                }
            }
            LocalChange::PrivacyRecord {
                request_id,
                service,
                outcome,
                expected_version,
            } => {
                let row = txn
                    .query_one(stmt(
                        "SELECT state, version FROM admin_privacy_requests WHERE request_id = $1 FOR UPDATE",
                        vec![(*request_id).into()],
                    ))
                    .await
                    .map_err(unavailable)?;
                match row {
                    None => Err("RESOURCE_NOT_FOUND"),
                    Some(r) => {
                        let (state, version): (String, i64) =
                            (get(&r, "state")?, get(&r, "version")?);
                        if state != "OPEN" || version != *expected_version {
                            Err("VERSION_CONFLICT")
                        } else {
                            let n = txn
                                .execute(stmt(
                                    "UPDATE admin_privacy_request_services SET outcome = $3, recorded_by = $4, recorded_at = $5 \
                                     WHERE request_id = $1 AND service = $2 AND outcome = 'PENDING'",
                                    vec![(*request_id).into(), service.clone().into(), outcome.clone().into(), actor.clone().into(), now.into()],
                                ))
                                .await
                                .map_err(unavailable)?
                                .rows_affected();
                            if n != 1 {
                                Err("VERSION_CONFLICT")
                            } else {
                                let r = txn
                                    .query_one(stmt(
                                        "UPDATE admin_privacy_requests SET version = version + 1, updated_at = $2, \
                                           state = CASE WHEN EXISTS (SELECT 1 FROM admin_privacy_request_services \
                                             WHERE request_id = $1 AND outcome = 'PENDING') THEN 'OPEN' ELSE 'COMPLETE' END, \
                                           completed_at = CASE WHEN EXISTS (SELECT 1 FROM admin_privacy_request_services \
                                             WHERE request_id = $1 AND outcome = 'PENDING') THEN NULL ELSE $2 END \
                                         WHERE request_id = $1 RETURNING state, version",
                                        vec![(*request_id).into(), now.into()],
                                    ))
                                    .await
                                    .map_err(unavailable)?
                                    .ok_or(StoreError::Invalid("no row"))?;
                                Ok((get(&r, "state")?, get(&r, "version")?))
                            }
                        }
                    }
                }
            }
        };
        let outcome = match &applied {
            Ok((status, version)) => {
                txn.execute(stmt(
                    "UPDATE admin_operations SET status = 'SENT', sent_at = $3 WHERE principal_sub = $1 AND idempotency_key = $2",
                    vec![op.principal_sub.clone().into(), op.idempotency_key.into(), now.into()],
                ))
                .await
                .map_err(unavailable)?;
                txn.execute(stmt(
                    "UPDATE admin_operations SET status = 'SUCCEEDED', source_status = $3, source_version = $4, settled_at = $5 \
                     WHERE principal_sub = $1 AND idempotency_key = $2",
                    vec![
                        op.principal_sub.clone().into(),
                        op.idempotency_key.into(),
                        status.clone().into(),
                        version.to_string().into(),
                        now.into(),
                    ],
                ))
                .await
                .map_err(unavailable)?;
                LocalOutcome::Applied {
                    status: status.clone(),
                    version: *version,
                }
            }
            Err(code) => {
                txn.execute(stmt(
                    "UPDATE admin_operations SET status = 'FAILED', source_status = 'REFUSED', error_code = $3, settled_at = $4 \
                     WHERE principal_sub = $1 AND idempotency_key = $2",
                    vec![op.principal_sub.clone().into(), op.idempotency_key.into(), (*code).into(), now.into()],
                ))
                .await
                .map_err(unavailable)?;
                LocalOutcome::Refused(code)
            }
        };
        txn.commit().await.map_err(unavailable)?;
        Ok(outcome)
    }

    async fn privacy_requests(
        &self,
        after: Option<(DateTime<Utc>, uuid::Uuid)>,
        limit: u32,
    ) -> StoreResult<Vec<PrivacyRequest>> {
        let (t, id) = match after {
            Some((t, id)) => (Some(t), Some(id)),
            None => (None, None),
        };
        let rows = self
            .db
            .query_all(stmt(
                "SELECT request_id FROM admin_privacy_requests \
                 WHERE ($1::timestamptz IS NULL OR (created_at, request_id) > ($1::timestamptz, $2::uuid)) \
                 ORDER BY created_at, request_id LIMIT $3",
                vec![t.into(), id.into(), i64::from(limit).into()],
            ))
            .await
            .map_err(unavailable)?;
        let mut out = Vec::new();
        for r in rows {
            let id: uuid::Uuid = get(&r, "request_id")?;
            if let Some(p) = self.privacy_request(id).await? {
                out.push(p);
            }
        }
        Ok(out)
    }

    async fn privacy_request(&self, id: uuid::Uuid) -> StoreResult<Option<PrivacyRequest>> {
        let Some(r) = self
            .db
            .query_one(stmt(
                "SELECT request_id, kind, subject_ref, state, created_by, created_at, updated_at, completed_at, version \
                 FROM admin_privacy_requests WHERE request_id = $1",
                vec![id.into()],
            ))
            .await
            .map_err(unavailable)?
        else {
            return Ok(None);
        };
        let services = self
            .db
            .query_all(stmt(
                "SELECT service, outcome, recorded_by, recorded_at FROM admin_privacy_request_services \
                 WHERE request_id = $1 ORDER BY service",
                vec![id.into()],
            ))
            .await
            .map_err(unavailable)?
            .iter()
            .map(|s| {
                Ok(PrivacyService {
                    service: get(s, "service")?,
                    outcome: get(s, "outcome")?,
                    recorded_by: get(s, "recorded_by")?,
                    recorded_at: get(s, "recorded_at")?,
                })
            })
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(Some(PrivacyRequest {
            request_id: get(&r, "request_id")?,
            kind: get(&r, "kind")?,
            subject_ref: get(&r, "subject_ref")?,
            state: get(&r, "state")?,
            created_by: get(&r, "created_by")?,
            created_at: get(&r, "created_at")?,
            updated_at: get(&r, "updated_at")?,
            completed_at: get(&r, "completed_at")?,
            version: get(&r, "version")?,
            services,
        }))
    }

    async fn ingest_event(
        &self,
        event: &OwnerEvent,
        accepted: bool,
        received_at: DateTime<Utc>,
    ) -> StoreResult<IngestOutcome> {
        let at = received_at.trunc_subsecs(6);
        let txn = self.db.begin().await.map_err(unavailable)?;
        // One append at a time: feed sequences commit in increasing order.
        txn.execute(stmt(
            "SELECT pg_advisory_xact_lock($1)",
            vec![EVENTS_LOCK_KEY.into()],
        ))
        .await
        .map_err(unavailable)?;
        let seen = txn
            .query_one(stmt(
                "SELECT 1 AS seen FROM admin_event_inbox WHERE producer = $1 AND event_id = $2",
                vec![event.producer.clone().into(), event.event_id.into()],
            ))
            .await
            .map_err(unavailable)?;
        if seen.is_some() {
            txn.rollback().await.map_err(unavailable)?;
            return Ok(IngestOutcome::Duplicate);
        }
        let mut outcome = if !accepted {
            IngestOutcome::Quarantined
        } else {
            let row = txn
                .query_one(stmt(
                    "SELECT COALESCE(max(version), 0)::bigint AS last FROM admin_hq_outbox \
                     WHERE producer = $1 AND subject_kind = $2 AND subject_id = $3",
                    vec![
                        event.producer.clone().into(),
                        event.subject_kind.clone().into(),
                        event.subject_id.clone().into(),
                    ],
                ))
                .await
                .map_err(unavailable)?
                .ok_or(StoreError::Invalid("no row"))?;
            let last: i64 = get(&row, "last")?;
            if last >= event.version {
                IngestOutcome::Stale
            } else {
                IngestOutcome::Appended(0)
            }
        };
        txn.execute(stmt(
            "INSERT INTO admin_event_inbox (producer, event_id, event_type, disposition, received_at) \
             VALUES ($1, $2, $3, $4, $5)",
            vec![
                event.producer.clone().into(),
                event.event_id.into(),
                event.event_type.clone().into(),
                outcome.as_str().into(),
                at.into(),
            ],
        ))
        .await
        .map_err(unavailable)?;
        if let IngestOutcome::Appended(_) = outcome {
            let row = txn
                .query_one(stmt(
                    "INSERT INTO admin_hq_outbox (producer, event_id, event_type, subject_kind, subject_id, state, \
                     version, occurred_at, appended_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING sequence",
                    vec![
                        event.producer.clone().into(),
                        event.event_id.into(),
                        event.event_type.clone().into(),
                        event.subject_kind.clone().into(),
                        event.subject_id.clone().into(),
                        event.state.clone().into(),
                        event.version.into(),
                        event.occurred_at.trunc_subsecs(6).into(),
                        at.into(),
                    ],
                ))
                .await
                .map_err(unavailable)?
                .ok_or(StoreError::Invalid("no row"))?;
            outcome = IngestOutcome::Appended(get(&row, "sequence")?);
        }
        let projection = match outcome {
            IngestOutcome::Quarantined => {
                "INSERT INTO admin_projections (name, last_event_at, open_gaps, quarantined, updated_at) \
                 VALUES ($1, NULL, 0, 1, $2) ON CONFLICT (name) DO UPDATE \
                 SET quarantined = admin_projections.quarantined + 1, updated_at = $2"
            }
            _ => {
                "INSERT INTO admin_projections (name, last_event_at, open_gaps, quarantined, updated_at) \
                 VALUES ($1, $2, 0, 0, $2) ON CONFLICT (name) DO UPDATE \
                 SET last_event_at = GREATEST(COALESCE(admin_projections.last_event_at, $2), $2), updated_at = $2"
            }
        };
        txn.execute(stmt(
            projection,
            vec![crate::routes::EVENT_INBOX_PROJECTION.into(), at.into()],
        ))
        .await
        .map_err(unavailable)?;
        txn.commit().await.map_err(unavailable)?;
        Ok(outcome)
    }

    async fn event_feed(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        after: Option<i64>,
        limit: u32,
    ) -> StoreResult<Vec<FeedEvent>> {
        let rows = self
            .db
            .query_all(stmt(
                "SELECT sequence, producer, event_id, event_type, subject_kind, subject_id, state, version, \
                 occurred_at, appended_at FROM admin_hq_outbox \
                 WHERE appended_at >= $1 AND appended_at < $2 AND ($3::bigint IS NULL OR sequence > $3) \
                 ORDER BY sequence ASC LIMIT $4",
                vec![start.into(), end.into(), after.into(), i64::from(limit).into()],
            ))
            .await
            .map_err(unavailable)?;
        rows.iter()
            .map(|r| {
                Ok(FeedEvent {
                    sequence: get(r, "sequence")?,
                    event: OwnerEvent {
                        producer: get(r, "producer")?,
                        event_id: get(r, "event_id")?,
                        event_type: get(r, "event_type")?,
                        subject_kind: get(r, "subject_kind")?,
                        subject_id: get(r, "subject_id")?,
                        state: get(r, "state")?,
                        version: get(r, "version")?,
                        occurred_at: get(r, "occurred_at")?,
                    },
                    appended_at: get(r, "appended_at")?,
                })
            })
            .collect()
    }

    async fn acknowledge_events(
        &self,
        consumer: &str,
        sequence: i64,
        at: DateTime<Utc>,
    ) -> StoreResult<AckOutcome> {
        // The head only grows: a sequence at or below it now stays valid.
        let head = self
            .db
            .query_one(stmt(
                "SELECT COALESCE(max(sequence), 0)::bigint AS head FROM admin_hq_outbox",
                vec![],
            ))
            .await
            .map_err(unavailable)?
            .ok_or(StoreError::Invalid("no row"))?;
        let head: i64 = get(&head, "head")?;
        if sequence > head {
            return Ok(AckOutcome::BeyondHead);
        }
        let row = self
            .db
            .query_one(stmt(
                "INSERT INTO admin_hq_consumer_cursors (consumer, acknowledged_sequence, updated_at) \
                 VALUES ($1, $2, $3) ON CONFLICT (consumer) DO UPDATE SET \
                   acknowledged_sequence = GREATEST(admin_hq_consumer_cursors.acknowledged_sequence, EXCLUDED.acknowledged_sequence), \
                   updated_at = CASE WHEN EXCLUDED.acknowledged_sequence > admin_hq_consumer_cursors.acknowledged_sequence \
                     THEN EXCLUDED.updated_at ELSE admin_hq_consumer_cursors.updated_at END \
                 RETURNING acknowledged_sequence",
                vec![consumer.into(), sequence.into(), at.trunc_subsecs(6).into()],
            ))
            .await
            .map_err(unavailable)?
            .ok_or(StoreError::Invalid("no row"))?;
        Ok(AckOutcome::Acknowledged(get(
            &row,
            "acknowledged_sequence",
        )?))
    }

    async fn begin_activation(
        &self,
        op: &Operation,
        request: &ActivationRequest,
    ) -> StoreResult<BeginOutcome> {
        let txn = self.db.begin().await.map_err(unavailable)?;
        let inserted = txn
            .execute(stmt(
                INSERT_PENDING_OPERATION,
                op_values(op, op.created_at.trunc_subsecs(6)),
            ))
            .await
            .map_err(unavailable)?
            .rows_affected();
        if inserted != 1 {
            txn.rollback().await.map_err(unavailable)?;
            return match self
                .operation(&op.principal_sub, op.idempotency_key)
                .await?
            {
                Some(existing) => Ok(BeginOutcome::Existing(Box::new(existing))),
                None => Ok(BeginOutcome::VersionTaken),
            };
        }
        let recorded = txn
            .execute(stmt(
                "INSERT INTO admin_activation_requests (request_id, operation, release_sha, confirm, requested_by, \
                 approver, approval_id, state, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, 'REQUESTED', $8) \
                 ON CONFLICT DO NOTHING",
                vec![
                    request.request_id.into(),
                    request.operation.clone().into(),
                    request.release_sha.clone().into(),
                    request.confirm.clone().into(),
                    request.requested_by.clone().into(),
                    request.approver.clone().into(),
                    request.approval_id.clone().into(),
                    request.created_at.trunc_subsecs(6).into(),
                ],
            ))
            .await
            .map_err(unavailable)?
            .rows_affected();
        if recorded != 1 {
            txn.rollback().await.map_err(unavailable)?;
            return Ok(BeginOutcome::VersionTaken);
        }
        txn.commit().await.map_err(unavailable)?;
        Ok(BeginOutcome::Started)
    }

    async fn settle_activation(
        &self,
        sub: &str,
        key: uuid::Uuid,
        st: &Settlement,
        update: &ActivationUpdate,
    ) -> StoreResult<bool> {
        let from = settle_from(st.status);
        if from.is_empty() {
            return Ok(false);
        }
        let settled_at = st.status.settled().then(|| st.at.trunc_subsecs(6));
        let txn = self.db.begin().await.map_err(unavailable)?;
        let row = txn
            .query_one(stmt(
                "UPDATE admin_operations SET status = $3, source_status = $4, source_version = $5, \
                 error_code = $6, settled_at = $7 \
                 WHERE principal_sub = $1 AND idempotency_key = $2 AND status = ANY($8) RETURNING operation_id",
                vec![
                    sub.into(),
                    key.into(),
                    st.status.as_str().into(),
                    st.source_status.clone().into(),
                    st.source_version.clone().into(),
                    st.error_code.clone().into(),
                    settled_at.into(),
                    from.iter().map(|s| s.to_string()).collect::<Vec<String>>().into(),
                ],
            ))
            .await
            .map_err(unavailable)?;
        let Some(row) = row else {
            txn.rollback().await.map_err(unavailable)?;
            return Ok(false);
        };
        let id: uuid::Uuid = get(&row, "operation_id")?;
        txn.execute(stmt(
            "UPDATE admin_activation_requests SET state = COALESCE($2, state), \
             dispatched_at = COALESCE(dispatched_at, $3), run_id = COALESCE(run_id, $4) \
             WHERE request_id = $1 AND state = 'REQUESTED'",
            vec![
                id.into(),
                update.state.map(str::to_string).into(),
                update.dispatched_at.map(|t| t.trunc_subsecs(6)).into(),
                update.run_id.into(),
            ],
        ))
        .await
        .map_err(unavailable)?;
        txn.commit().await.map_err(unavailable)?;
        Ok(true)
    }

    async fn link_activation_run(&self, request_id: uuid::Uuid, run_id: i64) -> StoreResult<bool> {
        let n = self
            .db
            .execute(stmt(
                "UPDATE admin_activation_requests SET run_id = $2 \
                 WHERE request_id = $1 AND state = 'DISPATCHED' AND run_id IS NULL \
                 AND NOT EXISTS (SELECT 1 FROM admin_activation_requests WHERE run_id = $2)",
                vec![request_id.into(), run_id.into()],
            ))
            .await
            .map_err(unavailable)?
            .rows_affected();
        Ok(n == 1)
    }

    async fn activation_requests(
        &self,
        before: Option<(DateTime<Utc>, uuid::Uuid)>,
        limit: u32,
    ) -> StoreResult<Vec<ActivationRequest>> {
        let (t, id) = match before {
            Some((t, id)) => (Some(t), Some(id)),
            None => (None, None),
        };
        let rows = self
            .db
            .query_all(stmt(
                &format!(
                    "SELECT {ACTIVATION_COLUMNS} FROM admin_activation_requests \
                     WHERE ($1::timestamptz IS NULL OR (created_at, request_id) < ($1::timestamptz, $2::uuid)) \
                     ORDER BY created_at DESC, request_id DESC LIMIT $3"
                ),
                vec![t.into(), id.into(), i64::from(limit).into()],
            ))
            .await
            .map_err(unavailable)?;
        rows.iter().map(activation_row).collect()
    }

    async fn activation_request(&self, id: uuid::Uuid) -> StoreResult<Option<ActivationRequest>> {
        let row = self
            .db
            .query_one(stmt(
                &format!("SELECT {ACTIVATION_COLUMNS} FROM admin_activation_requests WHERE request_id = $1"),
                vec![id.into()],
            ))
            .await
            .map_err(unavailable)?;
        row.as_ref().map(activation_row).transpose()
    }

    async fn activations_for_runs(&self, run_ids: &[i64]) -> StoreResult<Vec<ActivationRequest>> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .db
            .query_all(stmt(
                &format!("SELECT {ACTIVATION_COLUMNS} FROM admin_activation_requests WHERE run_id = ANY($1)"),
                vec![run_ids.to_vec().into()],
            ))
            .await
            .map_err(unavailable)?;
        rows.iter().map(activation_row).collect()
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
