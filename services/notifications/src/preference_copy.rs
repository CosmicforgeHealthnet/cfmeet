//! Roadmap N10 (Document 5 §6, Phase 4.2): the one-off copy of the Hub's notification
//! preferences into this service, the catch-up of Hub changes made after the copy began,
//! and the verification that gates the facade switch.
//!
//! Data moves only over the Hub's authenticated owner API (contract
//! `communications/notification-owner/v1`, `communications-preferences.export`, signed
//! first-party scope); this service writes only its own tables, the Hub only its own.
//!
//! **Control and progress** live in ONE row, `notif_preference_copy`:
//! * `mode` — `OFF` (default: nothing runs) or `RUN`; changed by the release tooling;
//! * `phase` `SNAPSHOT` → pages every Hub row of `ACCOUNT`, then `CONVERSATION`, then
//!   `CONTACT` (500 per page; the cursor is committed with the rows of its page, so a
//!   restart, a crash or the other replica resumes exactly where it stopped);
//! * the snapshot's first page records the Hub change-feed position read BEFORE its rows
//!   (`feed_cursor`); every change committed after that position is in the feed after it;
//! * `phase` `CATCH_UP` → polls the Hub change feed after `feed_cursor` and applies each
//!   entry's CURRENT Hub value, until the operator stops the job at the switch. The cursor
//!   only advances through entries the Hub reports as settled (older than 60 s), so an
//!   entry whose transaction committed late is re-read, never skipped; a pruned feed window
//!   restarts the snapshot;
//! * `verify_requested_at` → one FINAL PASS: every Hub row is read again, applied (a
//!   no-op unless the Hub row is newer) and compared with this service's row (values and
//!   `source_version`); the counts per kind are stored in `verification` with
//!   `complete = true` only when every Hub row is matched and no row was skipped. Run it
//!   inside the Hub write freeze: then it is exact, whatever the catch-up missed.
//!
//! Every write is idempotent by version (see `preferences::copy_*`): replays, overlapping
//! pages and re-runs never move a value backwards and never reset a preference to a
//! default (an absent Hub row is never copied as a value). Security events are not a Hub
//! row and are never copied: they stay fixed on.
//!
//! Replicas: one tick holds the row lock (`FOR UPDATE SKIP LOCKED`), so the job runs on
//! one replica at a time. The Hub answers 409 `PREFERENCE_AUTHORITY_MOVED` once its
//! facade switch is `SERVICE`; the job then records the error and copies nothing.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr, FromQueryResult,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    db,
    preferences::{self, HubContactPreference, HubConversationPreference, HubPreferences},
};

/// Rows per Hub page (contract maximum).
pub const PAGE: u32 = 500;
/// Change pages applied per catch-up tick.
pub const MAX_CHANGE_PAGES: usize = 20;
pub const KINDS: [&str; 3] = ["ACCOUNT", "CONVERSATION", "CONTACT"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HubError {
    #[error("the Hub preference export is not configured")]
    NotConfigured,
    #[error("the Hub preference export is unavailable")]
    Unavailable,
    #[error("the Hub preference facade already points at this service")]
    AuthorityMoved,
    #[error("the Hub answered an unexpected response")]
    InvalidResponse,
}

impl HubError {
    pub fn code(self) -> &'static str {
        match self {
            HubError::NotConfigured => "HUB_NOT_CONFIGURED",
            HubError::Unavailable => "HUB_UNAVAILABLE",
            HubError::AuthorityMoved => "PREFERENCE_AUTHORITY_MOVED",
            HubError::InvalidResponse => "HUB_INVALID_RESPONSE",
        }
    }
}

/// One page of `exportNotificationPreferences`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPage {
    pub items: Vec<Value>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    pub feed_position: i64,
    pub settled_position: i64,
    pub oldest_position: i64,
}

/// One page of `listNotificationPreferenceChanges`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePage {
    pub items: Vec<Value>,
    pub next_after: i64,
    pub more: bool,
    pub feed_position: i64,
    pub settled_position: i64,
    pub oldest_position: i64,
}

/// The Hub's preference owner API (HTTP in production, in-memory in tests).
#[async_trait]
pub trait HubPreferenceSource: Send + Sync {
    async fn export(
        &self,
        kind: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<ExportPage, HubError>;
    async fn changes(&self, after: i64, limit: u32) -> Result<ChangePage, HubError>;
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct CopyState {
    pub mode: String,
    pub phase: String,
    pub snapshot_kind: String,
    pub snapshot_cursor: Option<String>,
    pub feed_cursor: Option<i64>,
    pub snapshots_completed: i32,
    pub rows_written: i64,
    pub rows_unchanged: i64,
    pub rows_skipped: i64,
    pub changes_applied: i64,
    pub verify_requested_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub verification: Option<Value>,
    pub last_error: Option<String>,
}

const STATE_COLUMNS: &str = "mode, phase, snapshot_kind, snapshot_cursor, feed_cursor, snapshots_completed, rows_written,
    rows_unchanged, rows_skipped, changes_applied, verify_requested_at, verified_at, verification, last_error";

/// The job row (read-only).
pub async fn state<C: ConnectionTrait>(conn: &C) -> Result<CopyState, DbErr> {
    db::one(
        conn,
        &format!("SELECT {STATE_COLUMNS} FROM notif_preference_copy WHERE singleton"),
        vec![],
    )
    .await?
    .ok_or_else(|| DbErr::Custom("notif_preference_copy row missing".into()))
}

/// What one tick did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    /// `mode = OFF`.
    Off,
    /// The other replica holds the job row.
    Busy,
    /// One snapshot page of `kind`; `done` when the whole snapshot finished.
    Snapshot {
        kind: &'static str,
        written: u64,
        done: bool,
    },
    /// Catch-up pages applied (`applied` rows written or removed).
    CatchUp { applied: u64 },
    /// A final pass + verification ran; `complete` as stored.
    Verified { complete: bool },
    /// The Hub could not be used; nothing was copied by this tick.
    HubError(HubError),
}

#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    written: u64,
    unchanged: u64,
    skipped: u64,
}

/// Applies one exported item of `kind`. `Ok(None)`: skipped (invalid for this service).
async fn apply_item<C: ConnectionTrait>(
    conn: &C,
    kind: &str,
    item: &Value,
) -> Result<Option<bool>, DbErr> {
    match kind {
        "ACCOUNT" => match serde_json::from_value::<HubPreferences>(item.clone()) {
            Ok(p) if p.valid() => preferences::copy_account(conn, &p).await.map(Some),
            _ => Ok(None),
        },
        "CONVERSATION" => match serde_json::from_value::<HubConversationPreference>(item.clone()) {
            Ok(p) if p.valid() => preferences::copy_conversation(conn, &p).await.map(Some),
            _ => Ok(None),
        },
        "CONTACT" => match serde_json::from_value::<HubContactPreference>(item.clone()) {
            Ok(p) if p.valid() => preferences::copy_contact(conn, &p).await.map(Some),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

/// Whether this service holds exactly the exported item (after it was applied).
async fn item_matches<C: ConnectionTrait>(
    conn: &C,
    kind: &str,
    item: &Value,
) -> Result<bool, DbErr> {
    match kind {
        "ACCOUNT" => match serde_json::from_value::<HubPreferences>(item.clone()) {
            Ok(p) => preferences::account_matches(conn, &p).await,
            Err(_) => Ok(false),
        },
        "CONVERSATION" => match serde_json::from_value::<HubConversationPreference>(item.clone()) {
            Ok(p) => preferences::conversation_matches(conn, &p).await,
            Err(_) => Ok(false),
        },
        "CONTACT" => match serde_json::from_value::<HubContactPreference>(item.clone()) {
            Ok(p) => preferences::contact_matches(conn, &p).await,
            Err(_) => Ok(false),
        },
        _ => Ok(false),
    }
}

/// Applies one change-feed entry (its CURRENT Hub value). Returns whether a row changed.
async fn apply_change<C: ConnectionTrait>(conn: &C, entry: &Value) -> Result<bool, DbErr> {
    let account = entry["accountRef"].as_str().unwrap_or_default();
    let preference = &entry["preference"];
    match entry["kind"].as_str() {
        Some("ACCOUNT") if preference.is_object() => {
            let mut item = preference.clone();
            item["accountRef"] = json!(account);
            Ok(apply_item(conn, "ACCOUNT", &item).await?.unwrap_or(false))
        }
        Some("CONVERSATION") => {
            let Some(conversation) = entry["conversationId"]
                .as_str()
                .and_then(|c| Uuid::parse_str(c).ok())
            else {
                return Ok(false);
            };
            if preference.is_object() {
                let mut item = preference.clone();
                item["accountRef"] = json!(account);
                item["conversationId"] = json!(conversation);
                Ok(apply_item(conn, "CONVERSATION", &item)
                    .await?
                    .unwrap_or(false))
            } else if preferences::account_ref_ok(account) {
                preferences::remove_copied_conversation(conn, account, conversation).await
            } else {
                Ok(false)
            }
        }
        Some("CONTACT") if preference.is_object() => {
            let mut item = preference.clone();
            item["accountRef"] = json!(account);
            item["contactRef"] = entry["contactRef"].clone();
            Ok(apply_item(conn, "CONTACT", &item).await?.unwrap_or(false))
        }
        _ => Ok(false),
    }
}

async fn record_error(txn: &DatabaseTransaction, e: HubError) -> Result<(), DbErr> {
    db::exec(
        txn,
        "UPDATE notif_preference_copy SET last_error = $1, last_error_at = now(), updated_at = now() WHERE singleton",
        vec![e.code().into()],
    )
    .await?;
    Ok(())
}

fn next_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "ACCOUNT" => Some("CONVERSATION"),
        "CONVERSATION" => Some("CONTACT"),
        _ => None,
    }
}

fn kind_static(kind: &str) -> &'static str {
    KINDS
        .iter()
        .copied()
        .find(|k| *k == kind)
        .unwrap_or("ACCOUNT")
}

/// One step of the job: a verification when requested, else one snapshot page, else one
/// catch-up poll. Everything a step copies commits together with its progress.
pub async fn tick(conn: &DatabaseConnection, hub: &dyn HubPreferenceSource) -> Result<Tick, DbErr> {
    let txn = conn.begin().await?;
    let Some(s) = db::one::<CopyState, _>(
        &txn,
        &format!(
            "SELECT {STATE_COLUMNS} FROM notif_preference_copy WHERE singleton FOR UPDATE SKIP LOCKED"
        ),
        vec![],
    )
    .await?
    else {
        return Ok(Tick::Busy);
    };
    if s.mode != "RUN" {
        return Ok(Tick::Off);
    }
    let verify_due = s
        .verify_requested_at
        .is_some_and(|r| s.verified_at.is_none_or(|v| v < r));
    let out = if verify_due {
        match final_pass(&txn, hub).await? {
            Ok(report) => {
                let complete = report["complete"] == true;
                db::exec(
                    &txn,
                    "UPDATE notif_preference_copy SET verification = $1, verified_at = now(), last_error = NULL, updated_at = now() WHERE singleton",
                    vec![report.into()],
                )
                .await?;
                Tick::Verified { complete }
            }
            Err(e) => {
                record_error(&txn, e).await?;
                Tick::HubError(e)
            }
        }
    } else if s.phase == "SNAPSHOT" {
        snapshot_page(&txn, hub, &s).await?
    } else {
        catch_up(&txn, hub, &s).await?
    };
    txn.commit().await?;
    Ok(out)
}

async fn snapshot_page(
    txn: &DatabaseTransaction,
    hub: &dyn HubPreferenceSource,
    s: &CopyState,
) -> Result<Tick, DbErr> {
    let kind = kind_static(&s.snapshot_kind);
    let page = match hub.export(kind, s.snapshot_cursor.as_deref(), PAGE).await {
        Ok(p) => p,
        Err(e) => {
            record_error(txn, e).await?;
            return Ok(Tick::HubError(e));
        }
    };
    let starting = kind == "ACCOUNT" && s.snapshot_cursor.is_none();
    let mut c = Counts::default();
    for item in &page.items {
        match apply_item(txn, kind, item).await? {
            Some(true) => c.written += 1,
            Some(false) => c.unchanged += 1,
            None => c.skipped += 1,
        }
    }
    if c.skipped > 0 {
        tracing::warn!(
            kind,
            skipped = c.skipped,
            "preference copy skipped Hub rows this service cannot hold"
        );
    }
    let (next_kind, next_cursor, done) = match &page.next_cursor {
        Some(cursor) => (kind, Some(cursor.clone()), false),
        None => match next_kind(kind) {
            Some(k) => (k, None, false),
            None => ("ACCOUNT", None, true),
        },
    };
    // The first page of a snapshot records the feed position read before its rows: the
    // catch-up starts there (settled, so a late commit just before it is re-read).
    db::exec(
        txn,
        "UPDATE notif_preference_copy SET
                snapshot_kind = $1, snapshot_cursor = $2,
                phase = CASE WHEN $3 THEN 'CATCH_UP' ELSE 'SNAPSHOT' END,
                snapshot_completed_at = CASE WHEN $3 THEN now() ELSE snapshot_completed_at END,
                snapshots_completed = snapshots_completed + CASE WHEN $3 THEN 1 ELSE 0 END,
                feed_cursor = CASE WHEN $4 THEN $5 ELSE feed_cursor END,
                snapshot_started_at = CASE WHEN $4 THEN now() ELSE snapshot_started_at END,
                rows_written = rows_written + $6, rows_unchanged = rows_unchanged + $7, rows_skipped = rows_skipped + $8,
                last_error = NULL, updated_at = now()
          WHERE singleton",
        vec![
            next_kind.into(),
            next_cursor.into(),
            done.into(),
            starting.into(),
            page.settled_position.min(page.feed_position).into(),
            (c.written as i64).into(),
            (c.unchanged as i64).into(),
            (c.skipped as i64).into(),
        ],
    )
    .await?;
    Ok(Tick::Snapshot {
        kind,
        written: c.written,
        done,
    })
}

async fn catch_up(
    txn: &DatabaseTransaction,
    hub: &dyn HubPreferenceSource,
    s: &CopyState,
) -> Result<Tick, DbErr> {
    let start = s.feed_cursor.unwrap_or(0);
    let mut after = start;
    let mut applied = 0u64;
    let mut settled = start;
    for page_no in 0..MAX_CHANGE_PAGES {
        let page = match hub.changes(after, PAGE).await {
            Ok(p) => p,
            Err(e) => {
                record_error(txn, e).await?;
                return Ok(Tick::HubError(e));
            }
        };
        // Entries after the cursor were pruned (30-day feed retention): the catch-up
        // cannot prove it saw every change, so the snapshot runs again (idempotent).
        if page_no == 0 && page.oldest_position > start + 1 && page.feed_position > 0 {
            db::exec(
                txn,
                "UPDATE notif_preference_copy SET phase = 'SNAPSHOT', snapshot_kind = 'ACCOUNT', snapshot_cursor = NULL,
                        last_error = 'FEED_PRUNED_RESYNC', last_error_at = now(), updated_at = now() WHERE singleton",
                vec![],
            )
            .await?;
            return Ok(Tick::CatchUp { applied: 0 });
        }
        for entry in &page.items {
            if apply_change(txn, entry).await? {
                applied += 1;
            }
        }
        settled = page.settled_position;
        after = page.next_after.max(after);
        if !page.more {
            break;
        }
    }
    // Advance only through settled entries: a younger entry is read again next time.
    let cursor = start.max(after.min(settled));
    db::exec(
        txn,
        "UPDATE notif_preference_copy SET feed_cursor = $1, last_catch_up_at = now(), changes_applied = changes_applied + $2,
                last_error = NULL, updated_at = now() WHERE singleton",
        vec![cursor.into(), (applied as i64).into()],
    )
    .await?;
    Ok(Tick::CatchUp { applied })
}

/// Reads every Hub row again, applies it and compares; the per-kind counts.
async fn final_pass(
    txn: &DatabaseTransaction,
    hub: &dyn HubPreferenceSource,
) -> Result<Result<Value, HubError>, DbErr> {
    let mut report = serde_json::Map::new();
    let mut complete = true;
    let mut feed_position = None;
    for kind in KINDS {
        let (mut hub_rows, mut equal, mut different, mut skipped, mut written) =
            (0i64, 0i64, 0i64, 0i64, 0i64);
        let mut cursor: Option<String> = None;
        loop {
            let page = match hub.export(kind, cursor.as_deref(), PAGE).await {
                Ok(p) => p,
                Err(e) => return Ok(Err(e)),
            };
            feed_position.get_or_insert(page.feed_position);
            for item in &page.items {
                hub_rows += 1;
                match apply_item(txn, kind, item).await? {
                    None => {
                        skipped += 1;
                        continue;
                    }
                    Some(true) => written += 1,
                    Some(false) => {}
                }
                if item_matches(txn, kind, item).await? {
                    equal += 1;
                } else {
                    different += 1;
                }
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        let table = match kind {
            "ACCOUNT" => "notif_preferences",
            "CONVERSATION" => "notif_conversation_preferences",
            _ => "notif_contact_preferences",
        };
        let service_rows = db::scalar_i64(
            txn,
            &format!("SELECT count(*)::bigint AS n FROM {table} WHERE source_version IS NOT NULL"),
            vec![],
        )
        .await?;
        let kind_complete =
            different == 0 && skipped == 0 && equal == hub_rows && service_rows == hub_rows;
        complete &= kind_complete;
        report.insert(
            kind.to_ascii_lowercase(),
            json!({ "hubRows": hub_rows, "serviceRows": service_rows, "equal": equal, "different": different,
                    "skipped": skipped, "writtenByThisPass": written, "complete": kind_complete }),
        );
    }
    report.insert("complete".into(), json!(complete));
    report.insert("hubFeedPosition".into(), json!(feed_position.unwrap_or(0)));
    Ok(Ok(Value::Object(report)))
}

/// The job loop (one per replica; the row lock keeps it single-runner).
pub async fn run(
    db_conn: DatabaseConnection,
    hub: std::sync::Arc<dyn HubPreferenceSource>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        let wait = match tick(&db_conn, hub.as_ref()).await {
            Ok(Tick::Snapshot { done: false, .. }) => Duration::from_millis(50),
            Ok(Tick::Snapshot { done: true, .. }) | Ok(Tick::Verified { .. }) => {
                Duration::from_millis(500)
            }
            Ok(Tick::CatchUp { .. }) => Duration::from_secs(2),
            Ok(Tick::HubError(e)) => {
                tracing::warn!(error = e.code(), "preference copy tick: Hub unavailable");
                Duration::from_secs(10)
            }
            Ok(Tick::Off) | Ok(Tick::Busy) => Duration::from_secs(5),
            Err(e) => {
                tracing::warn!(summary = %db::summarize_db_error(&e), "preference copy tick failed");
                Duration::from_secs(10)
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => {}
        }
    }
}
