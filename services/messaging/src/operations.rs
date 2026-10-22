//! Part V J §§130–145 — operations: kill switches, incidents, key-transparency rollback
//! detection and the operator audit (migration 13).
//!
//! Kill switches only ever STOP a capability (globally or for one application
//! environment); there is no switch that enables anything or weakens a control. A guarded
//! operation fails closed with `FEATURE_DISABLED` / `KILL_SWITCH_ENGAGED`; a guarded worker
//! pauses and its durable queue waits. Operators change switches and resolve incidents
//! through `messaging-admin` (no network API); every change writes
//! `messaging_operator_audit` in the same transaction.
//!
//! Key-transparency rollback: an operator-held checkpoint (the last tree head an
//! auditor / the service saw, kept OUTSIDE the database) is compared with the database.
//! A database that is behind the checkpoint (restored from an older backup) or whose
//! root at the checkpoint size differs (a fork) is a `KT_ROLLBACK_DETECTED` incident;
//! while it is OPEN, every operation that appends to the log (device registration,
//! KeyPackage publication) fails closed, because appending to a rolled-back log would
//! hand clients a forked history. There is no automatic repair.

use std::path::Path;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode, Result},
    transparency,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSwitch {
    /// New messages of any profile (MANAGED and PRIVATE_E2EE).
    MessageSend,
    /// New conversations.
    ConversationCreate,
    /// Device registration and KeyPackage publication (appends to the transparency log).
    E2eeOnboarding,
    /// New attachment upload intents.
    AttachmentUpload,
    /// New SMS bridges and outbound bridged submissions (the worker pauses).
    SmsBridge,
    /// The webhook relay worker (relayed events wait PENDING).
    WebhookRelay,
    /// New PRIVATE_E2EE conversations (MANAGED creation is unaffected; never a downgrade).
    E2eeConversationCreate,
    /// MANAGED messaging: new MANAGED conversations, sends and edits.
    ManagedMessaging,
    /// New realtime connections and tickets (established ones are revalidated as usual).
    RealtimeConnect,
    /// Server-side (MANAGED) search.
    Search,
    /// New compliance exports and export download grants.
    Exports,
    /// New moderation actions (high-risk administration).
    ModerationActions,
    /// Secure backup uploads, finalization and downloads.
    Backups,
    /// Key-transparency proof serving.
    KtProofs,
}

impl KillSwitch {
    pub const ALL: [KillSwitch; 14] = [
        KillSwitch::MessageSend,
        KillSwitch::ConversationCreate,
        KillSwitch::E2eeOnboarding,
        KillSwitch::AttachmentUpload,
        KillSwitch::SmsBridge,
        KillSwitch::WebhookRelay,
        KillSwitch::E2eeConversationCreate,
        KillSwitch::ManagedMessaging,
        KillSwitch::RealtimeConnect,
        KillSwitch::Search,
        KillSwitch::Exports,
        KillSwitch::ModerationActions,
        KillSwitch::Backups,
        KillSwitch::KtProofs,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            KillSwitch::MessageSend => "MESSAGE_SEND",
            KillSwitch::ConversationCreate => "CONVERSATION_CREATE",
            KillSwitch::E2eeOnboarding => "E2EE_ONBOARDING",
            KillSwitch::AttachmentUpload => "ATTACHMENT_UPLOAD",
            KillSwitch::SmsBridge => "SMS_BRIDGE",
            KillSwitch::WebhookRelay => "WEBHOOK_RELAY",
            KillSwitch::E2eeConversationCreate => "E2EE_CONVERSATION_CREATE",
            KillSwitch::ManagedMessaging => "MANAGED_MESSAGING",
            KillSwitch::RealtimeConnect => "REALTIME_CONNECT",
            KillSwitch::Search => "SEARCH",
            KillSwitch::Exports => "EXPORTS",
            KillSwitch::ModerationActions => "MODERATION_ACTIONS",
            KillSwitch::Backups => "BACKUPS",
            KillSwitch::KtProofs => "KT_PROOFS",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Whether `switch` is engaged globally or for `environment_id`.
pub async fn engaged<C: ConnectionTrait>(
    db: &C,
    switch: KillSwitch,
    environment_id: Option<Uuid>,
) -> Result<bool> {
    #[derive(FromQueryResult)]
    struct N {
        n: i64,
    }
    let scope = environment_id
        .map(|e| e.to_string())
        .unwrap_or_else(|| "GLOBAL".into());
    let r: Option<N> = db::one(
        db,
        "SELECT count(*) AS n FROM messaging_kill_switches WHERE switch_name = $1 AND engaged \
           AND scope_key IN ('GLOBAL', $2)",
        vec![switch.as_str().into(), scope.into()],
    )
    .await?;
    Ok(r.map(|r| r.n > 0).unwrap_or(false))
}

/// Fails closed while `switch` is engaged for the environment (or globally).
pub async fn require_open<C: ConnectionTrait>(
    db: &C,
    switch: KillSwitch,
    environment_id: Uuid,
) -> Result<()> {
    if engaged(db, switch, Some(environment_id)).await? {
        crate::metrics::inc(
            "messaging_kill_switch_refusals_total",
            &[("switch", switch.as_str())],
        );
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "KILL_SWITCH_ENGAGED",
        ));
    }
    Ok(())
}

fn valid_operator(op: &str) -> bool {
    !op.is_empty()
        && op.len() <= 64
        && op
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._@-".contains(&b))
}

fn valid_code(s: &str) -> bool {
    s.len() >= 2
        && s.len() <= 48
        && s.as_bytes()[0].is_ascii_uppercase()
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Records one operator action (in the caller's transaction).
pub async fn audit<C: ConnectionTrait>(
    db: &C,
    operator: &str,
    action: &str,
    target: &str,
    detail: Value,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        db,
        "INSERT INTO messaging_operator_audit (id, occurred_at, operator_id, action, target, detail) VALUES ($1, $2, $3, $4, $5, $6)",
        vec![Uuid::new_v4().into(), now.into(), operator.into(), action.into(), target.into(), detail.into()],
    )
    .await?;
    Ok(())
}

/// Engages or releases `switch` for `environment_id` (None = global). Audited.
pub async fn set_switch(
    db: &DatabaseConnection,
    switch: KillSwitch,
    environment_id: Option<Uuid>,
    engage: bool,
    reason: &str,
    operator: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    if !valid_operator(operator) {
        return Err(Error::validation(
            "operator must be 1-64 characters of [A-Za-z0-9._@-]",
        ));
    }
    if !valid_code(reason) {
        return Err(Error::validation(
            "reason must be an UPPER_SNAKE_CASE code of 2-48 characters",
        ));
    }
    let scope = environment_id
        .map(|e| e.to_string())
        .unwrap_or_else(|| "GLOBAL".into());
    let txn = db.begin().await?;
    let changed = db::exec(
        &txn,
        "INSERT INTO messaging_kill_switches (switch_name, scope_key, engaged, reason, changed_by, changed_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (switch_name, scope_key) DO UPDATE SET engaged = EXCLUDED.engaged, reason = EXCLUDED.reason, \
           changed_by = EXCLUDED.changed_by, changed_at = EXCLUDED.changed_at, version = messaging_kill_switches.version + 1 \
         WHERE messaging_kill_switches.engaged IS DISTINCT FROM EXCLUDED.engaged",
        vec![switch.as_str().into(), scope.clone().into(), engage.into(), reason.into(), operator.into(), now.into()],
    )
    .await?
        == 1;
    if changed {
        audit(
            &txn,
            operator,
            if engage {
                "KILL_SWITCH_ENGAGED"
            } else {
                "KILL_SWITCH_RELEASED"
            },
            &format!("{}:{scope}", switch.as_str()),
            json!({ "reason": reason }),
            now,
        )
        .await?;
    }
    txn.commit().await?;
    Ok(changed)
}

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct SwitchRow {
    pub switch_name: String,
    pub scope_key: String,
    pub engaged: bool,
    pub reason: String,
    pub changed_by: String,
    pub changed_at: DateTime<Utc>,
    pub version: i32,
}

pub async fn switches<C: ConnectionTrait>(db: &C) -> Result<Vec<SwitchRow>> {
    db::all(
        db,
        "SELECT switch_name, scope_key, engaged, reason, changed_by, changed_at, version FROM messaging_kill_switches \
         ORDER BY switch_name, scope_key",
        vec![],
    )
    .await
    .map_err(Error::from)
}

// ---------------------------------------------------------------------------------------
// Incidents
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentCategory {
    OutboxDeadLetter,
    WebhookRelayDeadLetter,
    SmsTransportFailed,
    AttachmentDeleteDeadLetter,
    KtRollbackDetected,
    ScannerUnavailable,
    RetentionBacklog,
    StorageAccountingDrift,
    AbuseWave,
}

impl IncidentCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            IncidentCategory::OutboxDeadLetter => "OUTBOX_DEAD_LETTER",
            IncidentCategory::WebhookRelayDeadLetter => "WEBHOOK_RELAY_DEAD_LETTER",
            IncidentCategory::SmsTransportFailed => "SMS_TRANSPORT_FAILED",
            IncidentCategory::AttachmentDeleteDeadLetter => "ATTACHMENT_DELETE_DEAD_LETTER",
            IncidentCategory::KtRollbackDetected => "KT_ROLLBACK_DETECTED",
            IncidentCategory::ScannerUnavailable => "SCANNER_UNAVAILABLE",
            IncidentCategory::RetentionBacklog => "RETENTION_BACKLOG",
            IncidentCategory::StorageAccountingDrift => "STORAGE_ACCOUNTING_DRIFT",
            IncidentCategory::AbuseWave => "ABUSE_WAVE",
        }
    }

    fn severity(self) -> &'static str {
        match self {
            IncidentCategory::OutboxDeadLetter => "MEDIUM",
            IncidentCategory::WebhookRelayDeadLetter => "MEDIUM",
            IncidentCategory::SmsTransportFailed => "LOW",
            IncidentCategory::AttachmentDeleteDeadLetter => "HIGH",
            IncidentCategory::KtRollbackDetected => "CRITICAL",
            IncidentCategory::ScannerUnavailable => "HIGH",
            IncidentCategory::RetentionBacklog => "MEDIUM",
            IncidentCategory::StorageAccountingDrift => "MEDIUM",
            IncidentCategory::AbuseWave => "HIGH",
        }
    }

    /// Fixed, safe summary (never content, a number, a key or a secret).
    fn summary(self) -> &'static str {
        match self {
            IncidentCategory::OutboxDeadLetter => "A committed Messaging event could not be published and was dead-lettered.",
            IncidentCategory::WebhookRelayDeadLetter => "A public Messaging event was refused by the platform webhook ingress and dead-lettered.",
            IncidentCategory::SmsTransportFailed => "A bridged SMS submission failed after its retries.",
            IncidentCategory::AttachmentDeleteDeadLetter => "An attachment object could not be deleted from storage after its retries.",
            IncidentCategory::KtRollbackDetected => "The key-transparency log in the database is behind or diverges from the operator checkpoint; log appends are refused.",
            IncidentCategory::ScannerUnavailable => "The attachment scanner stayed unavailable through the retry budget; an attachment was rejected (never delivered unscanned).",
            IncidentCategory::RetentionBacklog => "Messages due for purge have waited longer than the retention backlog threshold.",
            IncidentCategory::StorageAccountingDrift => "An environment's recorded attachment storage differs from the sum of its objects.",
            IncidentCategory::AbuseWave => "Many principals of one environment were automatically restricted within an hour.",
        }
    }
}

/// Raises (or bumps) an incident. Best effort: never fails the caller's operation.
pub async fn raise(
    db: &DatabaseConnection,
    category: IncidentCategory,
    dedupe_key: &str,
    now: DateTime<Utc>,
) {
    let key = format!(
        "{}:{}",
        category.as_str(),
        dedupe_key.chars().take(96).collect::<String>()
    );
    let r = db::exec(
        db,
        "INSERT INTO messaging_incidents (id, category, severity, dedupe_key, summary, first_seen_at, last_seen_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $6) \
         ON CONFLICT (dedupe_key) WHERE status = 'OPEN' DO UPDATE SET occurrences = messaging_incidents.occurrences + 1, \
           last_seen_at = EXCLUDED.last_seen_at",
        vec![Uuid::new_v4().into(), category.as_str().into(), category.severity().into(), key.into(), category.summary().into(), now.into()],
    )
    .await;
    match r {
        Ok(_) => crate::metrics::inc(
            "messaging_incidents_raised_total",
            &[("category", category.as_str())],
        ),
        Err(e) => {
            tracing::error!(error = %e, category = category.as_str(), "incident could not be recorded")
        }
    }
}

/// Whether an incident of `category` is OPEN.
pub async fn open<C: ConnectionTrait>(db: &C, category: IncidentCategory) -> Result<bool> {
    #[derive(FromQueryResult)]
    struct N {
        n: i64,
    }
    let r: Option<N> = db::one(
        db,
        "SELECT count(*) AS n FROM messaging_incidents WHERE category = $1 AND status = 'OPEN'",
        vec![category.as_str().into()],
    )
    .await?;
    Ok(r.map(|r| r.n > 0).unwrap_or(false))
}

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct IncidentRow {
    pub id: Uuid,
    pub category: String,
    pub severity: String,
    pub status: String,
    pub summary: String,
    pub occurrences: i32,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub resolved_by: Option<String>,
}

pub async fn incidents<C: ConnectionTrait>(db: &C, all: bool) -> Result<Vec<IncidentRow>> {
    db::all(
        db,
        &format!(
            "SELECT id, category, severity, status, summary, occurrences, first_seen_at, last_seen_at, resolved_by \
               FROM messaging_incidents {} ORDER BY last_seen_at DESC LIMIT 500",
            if all { "" } else { "WHERE status = 'OPEN'" }
        ),
        vec![],
    )
    .await
    .map_err(Error::from)
}

/// Resolves an OPEN incident (operator action, audited in the same transaction).
pub async fn resolve(
    db: &DatabaseConnection,
    id: Uuid,
    operator: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    if !valid_operator(operator) {
        return Err(Error::validation(
            "operator must be 1-64 characters of [A-Za-z0-9._@-]",
        ));
    }
    let txn = db.begin().await?;
    let done = db::exec(
        &txn,
        "UPDATE messaging_incidents SET status = 'RESOLVED', resolved_at = $2, resolved_by = $3 WHERE id = $1 AND status = 'OPEN'",
        vec![id.into(), now.into(), operator.into()],
    )
    .await?
        == 1;
    if done {
        audit(
            &txn,
            operator,
            "INCIDENT_RESOLVED",
            &id.to_string(),
            json!({}),
            now,
        )
        .await?;
    }
    txn.commit().await?;
    Ok(done)
}

// ---------------------------------------------------------------------------------------
// Dead letters (§136)
// ---------------------------------------------------------------------------------------

/// A dead-letter queue an operator can inspect and replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadLetterKind {
    /// Committed domain events that could not be published.
    Outbox,
    /// Public events the platform webhook ingress refused.
    WebhookRelay,
    /// Storage objects that could not be deleted.
    ObjectDeletion,
}

impl DeadLetterKind {
    pub const ALL: [DeadLetterKind; 3] = [
        DeadLetterKind::Outbox,
        DeadLetterKind::WebhookRelay,
        DeadLetterKind::ObjectDeletion,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DeadLetterKind::Outbox => "outbox",
            DeadLetterKind::WebhookRelay => "webhook-relay",
            DeadLetterKind::ObjectDeletion => "object-deletion",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// (table, id column, dead status, reset assignments).
    fn sql(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            DeadLetterKind::Outbox => (
                "messaging_outbox_events",
                "id",
                "DEAD_LETTER",
                "status = 'PENDING', attempts = 0, next_attempt_at = $1, lease_token = NULL, lease_expires_at = NULL",
            ),
            DeadLetterKind::WebhookRelay => (
                "messaging_webhook_relay",
                "event_id",
                "DEAD_LETTER",
                "status = 'PENDING', attempts = 0, next_attempt_at = $1",
            ),
            DeadLetterKind::ObjectDeletion => (
                "messaging_object_deletions",
                "id",
                "DEAD",
                "status = 'PENDING', attempts = 0, next_attempt_at = $1, lease_token = NULL, lease_expires_at = NULL",
            ),
        }
    }
}

/// How many entries each dead-letter queue holds.
pub async fn dead_letters<C: ConnectionTrait>(db: &C) -> Result<Vec<(DeadLetterKind, i64)>> {
    let mut out = Vec::new();
    for kind in DeadLetterKind::ALL {
        let (table, _, dead, _) = kind.sql();
        let n = db::scalar_i64(
            db,
            &format!("SELECT count(*) FROM {table} WHERE status = '{dead}'"),
            vec![],
        )
        .await?;
        out.push((kind, n));
    }
    Ok(out)
}

/// Re-queues dead-lettered entries of `kind` (one by id, or all of them) for their
/// normal worker, with a fresh retry budget. The workers stay idempotent (event ids,
/// relay keys, deletion of an absent object), so a replay never duplicates an effect.
/// Audited in the same transaction. Returns how many entries were re-queued.
pub async fn replay_dead_letters(
    db: &DatabaseConnection,
    kind: DeadLetterKind,
    id: Option<Uuid>,
    operator: &str,
    now: DateTime<Utc>,
) -> Result<u64> {
    if !valid_operator(operator) {
        return Err(Error::validation(
            "operator must be 1-64 characters of [A-Za-z0-9._@-]",
        ));
    }
    let (table, id_column, dead, reset) = kind.sql();
    let txn = db.begin().await?;
    let n = db::exec(
        &txn,
        &format!(
            "UPDATE {table} SET {reset} WHERE status = '{dead}' AND ($2::uuid IS NULL OR {id_column} = $2)"
        ),
        vec![now.into(), id.into()],
    )
    .await?;
    if n > 0 {
        audit(
            &txn,
            operator,
            "DEAD_LETTERS_REPLAYED",
            kind.as_str(),
            json!({ "count": n, "id": id }),
            now,
        )
        .await?;
    }
    txn.commit().await?;
    crate::metrics::inc(
        "messaging_dead_letters_replayed_total",
        &[("kind", kind.as_str())],
    );
    Ok(n)
}

// ---------------------------------------------------------------------------------------
// Key-transparency rollback detection
// ---------------------------------------------------------------------------------------

/// An operator-held checkpoint of the transparency log (kept outside the database).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KtCheckpoint {
    pub tree_size: u64,
    /// Lower-case hex SHA-256 root at `tree_size`.
    pub root_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KtVerdict {
    /// The database extends the checkpoint (or equals it).
    Consistent { tree_size: u64 },
    /// The database has fewer leaves than the checkpoint (an older restore).
    Behind {
        database_size: u64,
        checkpoint_size: u64,
    },
    /// The database root at the checkpoint size differs (a fork / tampering).
    Forked { checkpoint_size: u64 },
}

/// The current head as a checkpoint.
pub async fn kt_checkpoint<C: ConnectionTrait>(db: &C) -> Result<KtCheckpoint> {
    let head = transparency::tree_head(db).await?;
    Ok(KtCheckpoint {
        tree_size: head.tree_size,
        root_hash: hex::encode(head.root_hash),
    })
}

/// Compares the database log with `checkpoint`; raises `KT_ROLLBACK_DETECTED` on a
/// rollback or fork.
pub async fn kt_verify(
    db: &DatabaseConnection,
    checkpoint: &KtCheckpoint,
    now: DateTime<Utc>,
) -> Result<KtVerdict> {
    let head = transparency::tree_head(db).await?;
    let verdict = if head.tree_size < checkpoint.tree_size {
        KtVerdict::Behind {
            database_size: head.tree_size,
            checkpoint_size: checkpoint.tree_size,
        }
    } else if checkpoint.tree_size == 0 {
        KtVerdict::Consistent {
            tree_size: head.tree_size,
        }
    } else {
        let root = hex::encode(transparency::root_at(db, checkpoint.tree_size).await?);
        if root != checkpoint.root_hash.to_ascii_lowercase() {
            KtVerdict::Forked {
                checkpoint_size: checkpoint.tree_size,
            }
        } else {
            KtVerdict::Consistent {
                tree_size: head.tree_size,
            }
        }
    };
    if !matches!(verdict, KtVerdict::Consistent { .. }) {
        tracing::error!(?verdict, "key-transparency rollback detected");
        raise(
            db,
            IncidentCategory::KtRollbackDetected,
            "transparency-log",
            now,
        )
        .await;
    }
    Ok(verdict)
}

/// Reads a checkpoint file (absent = no checkpoint yet).
pub fn read_checkpoint(path: &Path) -> std::io::Result<Option<KtCheckpoint>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Writes a checkpoint atomically (`<file>.tmp` + rename, mode 0600).
pub fn write_checkpoint(path: &Path, checkpoint: &KtCheckpoint) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(checkpoint)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)
}

/// Log appends fail closed while a rollback incident is OPEN (or onboarding is
/// switched off for the environment).
pub async fn require_kt_appendable<C: ConnectionTrait>(db: &C, environment_id: Uuid) -> Result<()> {
    require_open(db, KillSwitch::E2eeOnboarding, environment_id).await?;
    if open(db, IncidentCategory::KtRollbackDetected).await? {
        crate::metrics::inc("messaging_kt_append_refusals_total", &[]);
        return Err(Error::reason(
            ErrorCode::ServiceUnavailable,
            "KT_LOG_ROLLBACK_DETECTED",
        ));
    }
    Ok(())
}

/// The KT rollback monitor: verifies the database log against the checkpoint file and,
/// when consistent, advances the checkpoint to the current head. A rollback / fork
/// raises the CRITICAL incident (appends then fail closed) and leaves the checkpoint
/// untouched. The checkpoint must live on storage that is NOT restored with the database
/// (otherwise a restore rolls both back together); external auditors comparing signed
/// tree heads remain the independent check.
pub async fn run_kt_monitor(
    db: DatabaseConnection,
    clock: crate::domain::SharedClock,
    path: std::path::PathBuf,
    interval: std::time::Duration,
) {
    loop {
        if let Err(e) = kt_monitor_once(&db, clock.now(), &path).await {
            tracing::warn!(error = %e, "key-transparency monitor pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}

/// One monitor pass (see [`run_kt_monitor`]).
pub async fn kt_monitor_once(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    path: &Path,
) -> std::result::Result<KtVerdict, String> {
    let current = kt_checkpoint(db).await.map_err(|e| e.to_string())?;
    let verdict = match read_checkpoint(path).map_err(|e| format!("checkpoint unreadable: {e}"))? {
        None => KtVerdict::Consistent {
            tree_size: current.tree_size,
        },
        Some(checkpoint) => kt_verify(db, &checkpoint, now)
            .await
            .map_err(|e| e.to_string())?,
    };
    if matches!(verdict, KtVerdict::Consistent { .. }) {
        write_checkpoint(path, &current).map_err(|e| format!("checkpoint not written: {e}"))?;
    }
    Ok(verdict)
}
