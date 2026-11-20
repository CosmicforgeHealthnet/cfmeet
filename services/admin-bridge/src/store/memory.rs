//! In-memory [`Store`] for tests. Same semantics as `PgStore`, including all-or-nothing
//! claims and the audit hash chain; `fail_audit` simulates an audit outage.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};

use super::*;

#[derive(Default)]
struct Inner {
    principals: HashMap<(PrincipalKind, String), Principal>,
    revoked_subs: HashMap<String, DateTime<Utc>>,
    revoked_sessions: HashMap<String, DateTime<Utc>>,
    claims: HashMap<(ClaimKind, String), DateTime<Utc>>,
    rates: HashMap<(String, DateTime<Utc>), u32>,
    policies: HashMap<i64, PolicyRow>,
    audit: Vec<(AuditDraft, AuditReceipt)>,
    datasets: Vec<DatasetDay>,
    projections: Vec<ProjectionRow>,
    operations: Vec<Operation>,
}

#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
    fail_audit: AtomicBool,
    fail_all: AtomicBool,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tests: moves an operation's timestamps `seconds` into the past.
    pub fn backdate_operation(&self, key: uuid::Uuid, seconds: i64) {
        let mut inner = self.inner.lock().unwrap();
        for o in inner
            .operations
            .iter_mut()
            .filter(|o| o.idempotency_key == key)
        {
            o.created_at -= Duration::seconds(seconds);
            o.sent_at = o.sent_at.map(|t| t - Duration::seconds(seconds));
        }
    }

    pub fn operations(&self) -> Vec<Operation> {
        self.inner.lock().unwrap().operations.clone()
    }

    pub fn enroll(&self, p: Principal) {
        self.inner
            .lock()
            .unwrap()
            .principals
            .insert((p.kind, p.key_id.clone()), p);
    }

    pub fn revoke_sub(&self, sub: &str, at: DateTime<Utc>) {
        self.inner
            .lock()
            .unwrap()
            .revoked_subs
            .insert(sub.into(), at);
    }

    pub fn revoke_session(&self, session: &str, at: DateTime<Utc>) {
        self.inner
            .lock()
            .unwrap()
            .revoked_sessions
            .insert(session.into(), at);
    }

    pub fn put_policy(&self, row: PolicyRow) {
        self.inner
            .lock()
            .unwrap()
            .policies
            .insert(row.revision, row);
    }

    pub fn put_dataset_day(&self, day: DatasetDay) {
        let mut g = self.inner.lock().unwrap();
        g.datasets
            .retain(|d| !(d.dataset_id == day.dataset_id && d.day == day.day));
        g.datasets.push(day);
    }

    pub fn put_projection(&self, row: ProjectionRow) {
        let mut g = self.inner.lock().unwrap();
        g.projections.retain(|p| p.name != row.name);
        g.projections.push(row);
    }

    pub fn fail_audit(&self, fail: bool) {
        self.fail_audit.store(fail, Ordering::SeqCst);
    }

    pub fn fail_all(&self, fail: bool) {
        self.fail_all.store(fail, Ordering::SeqCst);
    }

    pub fn audit_entries(&self) -> Vec<(AuditDraft, AuditReceipt)> {
        self.inner.lock().unwrap().audit.clone()
    }

    fn check(&self) -> StoreResult<()> {
        if self.fail_all.load(Ordering::SeqCst) {
            Err(StoreError::Unavailable("simulated outage".into()))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn ping(&self) -> StoreResult<()> {
        self.check()
    }

    async fn principal(&self, kind: PrincipalKind, key_id: &str) -> StoreResult<Option<Principal>> {
        self.check()?;
        Ok(self
            .inner
            .lock()
            .unwrap()
            .principals
            .get(&(kind, key_id.to_string()))
            .cloned())
    }

    async fn is_revoked(
        &self,
        sub: &str,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<bool> {
        self.check()?;
        let g = self.inner.lock().unwrap();
        Ok(g.revoked_subs.get(sub).is_some_and(|at| *at <= now)
            || g.revoked_sessions
                .get(session_id)
                .is_some_and(|at| *at <= now))
    }

    async fn claim_once(&self, claims: &[Claim]) -> StoreResult<ClaimOutcome> {
        self.check()?;
        let mut g = self.inner.lock().unwrap();
        let mut seen = HashSet::new();
        for c in claims {
            if g.claims.contains_key(&(c.kind, c.value.clone()))
                || !seen.insert((c.kind, c.value.clone()))
            {
                return Ok(ClaimOutcome::Replay(c.kind));
            }
        }
        for c in claims {
            g.claims.insert((c.kind, c.value.clone()), c.keep_until);
        }
        Ok(ClaimOutcome::Recorded)
    }

    async fn rate_allow(&self, key: &RateKey, limit: u32) -> StoreResult<bool> {
        self.check()?;
        let mut g = self.inner.lock().unwrap();
        let n = g
            .rates
            .entry((key.bucket.clone(), key.window_start))
            .or_insert(0);
        *n += 1;
        Ok(*n <= limit)
    }

    async fn policy_revision(&self, revision: i64) -> StoreResult<Option<PolicyRow>> {
        self.check()?;
        Ok(self.inner.lock().unwrap().policies.get(&revision).cloned())
    }

    async fn append_audit(&self, draft: &AuditDraft) -> StoreResult<AuditReceipt> {
        self.check()?;
        if self.fail_audit.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable("simulated audit outage".into()));
        }
        let mut g = self.inner.lock().unwrap();
        let previous = g
            .audit
            .last()
            .map(|(_, r)| r.entry_hash.clone())
            .unwrap_or_else(|| GENESIS_HASH.to_string());
        let receipt = AuditReceipt {
            sequence: g.audit.len() as i64 + 1,
            entry_hash: chain_hash(&previous, draft),
        };
        g.audit.push((draft.clone(), receipt.clone()));
        Ok(receipt)
    }

    async fn dataset_days(
        &self,
        dataset_id: &str,
        start: NaiveDate,
        end: NaiveDate,
        limit: u32,
    ) -> StoreResult<Vec<DatasetDay>> {
        self.check()?;
        let g = self.inner.lock().unwrap();
        let mut v: Vec<DatasetDay> = g
            .datasets
            .iter()
            .filter(|d| d.dataset_id == dataset_id && d.day >= start && d.day <= end)
            .cloned()
            .collect();
        v.sort_by_key(|d| d.day);
        v.truncate(limit as usize);
        Ok(v)
    }

    async fn projections(&self) -> StoreResult<Vec<ProjectionRow>> {
        self.check()?;
        let mut v = self.inner.lock().unwrap().projections.clone();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn begin_operation(&self, op: &Operation) -> StoreResult<BeginOutcome> {
        self.check()?;
        let mut inner = self.inner.lock().unwrap();
        if let Some(existing) = inner.operations.iter().find(|o| {
            o.principal_sub == op.principal_sub && o.idempotency_key == op.idempotency_key
        }) {
            return Ok(BeginOutcome::Existing(Box::new(existing.clone())));
        }
        if inner.operations.iter().any(|o| {
            o.target == op.target
                && o.expected_version == op.expected_version
                && o.status != OpStatus::Failed
        }) {
            return Ok(BeginOutcome::VersionTaken);
        }
        let mut row = op.clone();
        row.status = OpStatus::Pending;
        row.sent_at = None;
        row.settled_at = None;
        inner.operations.push(row);
        Ok(BeginOutcome::Started)
    }

    async fn mark_sent(&self, sub: &str, key: uuid::Uuid, at: DateTime<Utc>) -> StoreResult<bool> {
        self.check()?;
        let mut inner = self.inner.lock().unwrap();
        match inner
            .operations
            .iter_mut()
            .find(|o| o.principal_sub == sub && o.idempotency_key == key)
        {
            Some(o) if o.status == OpStatus::Pending => {
                o.status = OpStatus::Sent;
                o.sent_at = Some(at);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn settle_operation(
        &self,
        sub: &str,
        key: uuid::Uuid,
        st: &Settlement,
    ) -> StoreResult<bool> {
        self.check()?;
        let mut inner = self.inner.lock().unwrap();
        match inner
            .operations
            .iter_mut()
            .find(|o| o.principal_sub == sub && o.idempotency_key == key)
        {
            Some(o) if o.status.may_become(st.status) => {
                o.status = st.status;
                o.source_status = st.source_status.clone();
                o.source_version = st.source_version.clone();
                o.error_code = st.error_code.clone();
                if st.status.settled() {
                    o.settled_at = Some(st.at);
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn operation(&self, sub: &str, key: uuid::Uuid) -> StoreResult<Option<Operation>> {
        self.check()?;
        Ok(self
            .inner
            .lock()
            .unwrap()
            .operations
            .iter()
            .find(|o| o.principal_sub == sub && o.idempotency_key == key)
            .cloned())
    }

    async fn open_operations(
        &self,
        before: DateTime<Utc>,
        limit: u32,
    ) -> StoreResult<Vec<Operation>> {
        self.check()?;
        let mut open: Vec<Operation> = self
            .inner
            .lock()
            .unwrap()
            .operations
            .iter()
            .filter(|o| !o.status.settled() && o.created_at < before)
            .cloned()
            .collect();
        open.sort_by_key(|o| o.created_at);
        open.truncate(limit as usize);
        Ok(open)
    }

    async fn cleanup(&self, now: DateTime<Utc>) -> StoreResult<CleanupStats> {
        self.check()?;
        let mut g = self.inner.lock().unwrap();
        let (c0, r0) = (g.claims.len(), g.rates.len());
        g.claims.retain(|_, keep| *keep > now);
        g.rates
            .retain(|(_, start), _| *start > now - Duration::hours(1));
        Ok(CleanupStats {
            claims: (c0 - g.claims.len()) as u64,
            rate_buckets: (r0 - g.rates.len()) as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(kind: ClaimKind, v: &str) -> Claim {
        Claim {
            kind,
            value: v.into(),
            keep_until: Utc::now() + Duration::minutes(11),
        }
    }

    #[tokio::test]
    async fn claims_are_all_or_nothing() {
        let s = MemoryStore::new();
        assert_eq!(
            s.claim_once(&[claim(ClaimKind::Nonce, "n1"), claim(ClaimKind::Jti, "j1")])
                .await
                .unwrap(),
            ClaimOutcome::Recorded
        );
        assert_eq!(
            s.claim_once(&[claim(ClaimKind::Nonce, "n2"), claim(ClaimKind::Jti, "j1")])
                .await
                .unwrap(),
            ClaimOutcome::Replay(ClaimKind::Jti)
        );
        // n2 was not recorded by the refused batch.
        assert_eq!(
            s.claim_once(&[claim(ClaimKind::Nonce, "n2")])
                .await
                .unwrap(),
            ClaimOutcome::Recorded
        );
    }

    #[tokio::test]
    async fn audit_chain_links_entries() {
        let s = MemoryStore::new();
        let d = AuditDraft {
            correlation_id: uuid::Uuid::nil(),
            occurred_at: Utc::now(),
            actor: "hq:a".into(),
            session_id: None,
            capability: None,
            method: "GET".into(),
            path: "/x".into(),
            purpose: None,
            scope: Value::Null,
            decision: "ALLOWED".into(),
            http_status: 200,
            request_digest: None,
            approval_id: None,
            approver: None,
            detail: Value::Null,
        };
        let a = s.append_audit(&d).await.unwrap();
        let b = s.append_audit(&d).await.unwrap();
        assert_eq!(a.entry_hash, chain_hash(GENESIS_HASH, &d));
        assert_eq!(b.entry_hash, chain_hash(&a.entry_hash, &d));
        assert_eq!((a.sequence, b.sequence), (1, 2));
    }
}
