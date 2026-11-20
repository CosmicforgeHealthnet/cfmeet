//! Persistence port of the bridge. `PgStore` (`store/pg.rs`) is production; `MemoryStore`
//! backs the security-chain tests. Both must give the same answers: the Postgres suite
//! (`tests/pg_store.rs`) runs the same behavioural checks as the unit tests here.
//!
//! The bridge stores no business data: only HQ enrolment (certificate fingerprints, HMAC
//! key digests, staff-token public keys), single-use claims, revocations, rate-limit
//! counters, signed access-policy revisions, the hash-chained audit and small summaries.

pub mod memory;
pub mod pg;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

pub use memory::MemoryStore;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store unavailable")]
    Unavailable(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("stored data invalid: {0}")]
    Invalid(&'static str),
}

pub type StoreResult<T> = Result<T, StoreError>;

/// What an enrolled HQ credential is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrincipalKind {
    /// `key_id` = lowercase hex SHA-256 fingerprint of the client certificate (DER).
    ClientCert,
    /// `key_id` = `X-HQ-Key-ID`; `material` = lowercase hex SHA-256 of the key bytes (the
    /// key itself lives only in a file, never in the database).
    HmacKey,
    /// `key_id` = JWS `kid`; `material` = lowercase hex Ed25519 public key (32 bytes).
    StaffKey,
}

impl PrincipalKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            PrincipalKind::ClientCert => "CLIENT_CERT",
            PrincipalKind::HmacKey => "HMAC_KEY",
            PrincipalKind::StaffKey => "STAFF_KEY",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "CLIENT_CERT" => Some(Self::ClientCert),
            "HMAC_KEY" => Some(Self::HmacKey),
            "STAFF_KEY" => Some(Self::StaffKey),
            _ => None,
        }
    }
}

/// One enrolled HQ credential (`admin_remote_principals`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub principal: String,
    pub kind: PrincipalKind,
    pub key_id: String,
    pub material: String,
    /// Staff keys only: enrolled `iss` / `aud`.
    pub issuer: Option<String>,
    pub audience: Option<String>,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Principal {
    pub fn live_at(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none_or(|r| r > now) && self.not_before <= now && now < self.expires_at
    }
}

/// What a single-use claim protects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimKind {
    Nonce,
    Jti,
    Approval,
}

impl ClaimKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ClaimKind::Nonce => "NONCE",
            ClaimKind::Jti => "JTI",
            ClaimKind::Approval => "APPROVAL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub kind: ClaimKind,
    pub value: String,
    /// The claim is kept (and a repeat refused) until then; cleanup removes it after.
    pub keep_until: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// Every claim was new and is now recorded (all in one transaction).
    Recorded,
    /// At least one claim already existed; nothing was recorded.
    Replay(ClaimKind),
}

/// A verified-in-memory access policy revision as stored (`admin_policy_revisions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRow {
    pub revision: i64,
    /// Exact signed bytes (UTF-8 JSON).
    pub document: Vec<u8>,
    /// Lowercase hex Ed25519 signature over `document`.
    pub signature: String,
    pub key_id: String,
}

/// Audit entry before it is chained.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditDraft {
    pub correlation_id: uuid::Uuid,
    pub occurred_at: DateTime<Utc>,
    /// `hq:<sub>` or `unauthenticated`.
    pub actor: String,
    pub session_id: Option<String>,
    pub capability: Option<String>,
    pub method: String,
    /// Path only (no query), at most 4096 bytes.
    pub path: String,
    pub purpose: Option<String>,
    /// `{market, organization, zone, tenant}` or null.
    pub scope: Value,
    /// `ALLOWED` or the refusal error code.
    pub decision: String,
    pub http_status: u16,
    pub request_digest: Option<String>,
    pub approval_id: Option<String>,
    pub approver: Option<String>,
    /// Extra non-personal detail (role used, risk, policy revision).
    pub detail: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditReceipt {
    pub sequence: i64,
    /// Lowercase hex SHA-256 chaining this entry to the previous one.
    pub entry_hash: String,
}

/// Rate-limit bucket: fixed one-minute windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateKey {
    /// Lowercase hex SHA-256 of `sub LF permission LF market LF organization LF zone LF tenant`.
    pub bucket: String,
    pub window_start: DateTime<Utc>,
}

/// One day of one dataset (`admin_dataset_days`).
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetDay {
    pub dataset_id: String,
    pub day: NaiveDate,
    /// metric -> integer | null.
    pub metrics: serde_json::Map<String, Value>,
    /// Sources (`<service>:<via>`) that reported for this day.
    pub sources_reported: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

/// Freshness of one projection (`admin_projections`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionRow {
    pub name: String,
    pub last_event_at: Option<DateTime<Utc>>,
    pub open_gaps: i64,
    pub quarantined: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupStats {
    pub claims: u64,
    pub rate_buckets: u64,
}

#[async_trait]
pub trait Store: Send + Sync + 'static {
    async fn ping(&self) -> StoreResult<()>;

    /// The enrolled credential of `kind` with `key_id`, if any (live or not: callers
    /// check [`Principal::live_at`]).
    async fn principal(&self, kind: PrincipalKind, key_id: &str) -> StoreResult<Option<Principal>>;

    /// True when `sub` (any session) or `session_id` is revoked at `now`.
    async fn is_revoked(
        &self,
        sub: &str,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<bool>;

    /// Records every claim in ONE transaction, or none if any already exists.
    async fn claim_once(&self, claims: &[Claim]) -> StoreResult<ClaimOutcome>;

    /// Counts one request in the bucket's window; true when the count (including this
    /// one) is within `limit`.
    async fn rate_allow(&self, key: &RateKey, limit: u32) -> StoreResult<bool>;

    async fn policy_revision(&self, revision: i64) -> StoreResult<Option<PolicyRow>>;

    /// Appends to the hash chain: `entry_hash = sha256(previous_hash LF canonical(entry))`,
    /// the genesis previous hash being 64 zeros. Serialized: no two entries share a
    /// previous hash.
    async fn append_audit(&self, draft: &AuditDraft) -> StoreResult<AuditReceipt>;

    /// Days of `dataset_id` in `[start, end]` (inclusive), ascending, at most `limit`.
    async fn dataset_days(
        &self,
        dataset_id: &str,
        start: NaiveDate,
        end: NaiveDate,
        limit: u32,
    ) -> StoreResult<Vec<DatasetDay>>;

    async fn projections(&self) -> StoreResult<Vec<ProjectionRow>>;

    /// Removes expired claims and rate buckets older than `now - 1 hour`.
    async fn cleanup(&self, now: DateTime<Utc>) -> StoreResult<CleanupStats>;
}

/// Canonical bytes hashed into the audit chain (stable key order, no whitespace).
pub fn audit_canonical(draft: &AuditDraft) -> Vec<u8> {
    let v = serde_json::json!({
        "correlation_id": draft.correlation_id.to_string(),
        "occurred_at": draft.occurred_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        "actor": draft.actor,
        "session_id": draft.session_id,
        "capability": draft.capability,
        "method": draft.method,
        "path": draft.path,
        "purpose": draft.purpose,
        "scope": draft.scope,
        "decision": draft.decision,
        "http_status": draft.http_status,
        "request_digest": draft.request_digest,
        "approval_id": draft.approval_id,
        "approver": draft.approver,
        "detail": draft.detail,
    });
    serde_json::to_vec(&v).expect("serializable")
}

pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub fn chain_hash(previous: &str, draft: &AuditDraft) -> String {
    let mut bytes = previous.as_bytes().to_vec();
    bytes.push(b'\n');
    bytes.extend(audit_canonical(draft));
    crate::wire::sha256_hex(&bytes)
}
