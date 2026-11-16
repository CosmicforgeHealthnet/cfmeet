//! Routing fence protocol v1, producer side (Notification Service specification,
//! Document 4 §9): one system per message.
//!
//! Every routable message gets exactly one routing decision (F1), made and recorded
//! inside the producer's own transaction — the transaction that also writes either the
//! legacy push jobs (route `OLD`) or the Notification Service publication (route `NEW`),
//! never both (F2). The tables live in the Messaging database
//! (`messaging_notification_routing_state`, `messaging_notification_route_decisions`,
//! history `messaging_notification_fence_seaql_migrations`); a guard trigger on
//! `messaging_push_jobs` refuses legacy pushes without an `OLD` decision once a category
//! has left `LEGACY_ONLY` (§9.4).
//!
//! Foundation only: no producer calls [`route_message`] yet, every category is
//! `LEGACY_ONLY`, and push behaviour is unchanged. [`fence_protocol`] feeds the
//! `routingProtocol` readiness fact the activation operation certifies (§9.6).
//!
//! Push key (§9.3): the Messaging outbox event id, in its envelope form
//! ([`push_key`], `evt_<32 hex>`); one decision covers every device fan-out of the event.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db;

/// The routing fence protocol this binary implements (reported in `/ready` as
/// `routingProtocol` once its history is applied).
pub const ROUTING_PROTOCOL: i16 = 1;

/// Message wake-ups (`messaging.message.created` → push). The category of every
/// `messaging_push_jobs` row.
pub const MESSAGE_WAKEUPS: &str = "MESSAGE_WAKEUPS";

/// Every routing category Messaging produces with a legacy delivery path (seeded
/// `LEGACY_ONLY` by the fence migration).
pub const CATEGORIES: &[&str] = &[MESSAGE_WAKEUPS];

/// CRITICAL categories (for which `critical_fallback = FORCED` pins the route to `OLD`).
/// Messaging produces none; kept so the §9.3 algorithm is complete.
const CRITICAL_CATEGORIES: &[&str] = &[];

/// Longest idempotency key the decision ledger accepts.
pub const MAX_KEY_LEN: usize = 160;

/// A category's routing mode (`notification_routing_state.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    LegacyOnly,
    Shadow,
    FencedOld,
    FencedNew,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "LEGACY_ONLY" => Self::LegacyOnly,
            "SHADOW" => Self::Shadow,
            "FENCED_OLD" => Self::FencedOld,
            "FENCED_NEW" => Self::FencedNew,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LegacyOnly => "LEGACY_ONLY",
            Self::Shadow => "SHADOW",
            Self::FencedOld => "FENCED_OLD",
            Self::FencedNew => "FENCED_NEW",
        }
    }
}

/// Which system delivers a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The legacy path (`messaging_push_jobs`).
    Old,
    /// The Notification Service (Messaging publication).
    New,
}

impl Route {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "OLD" => Some(Self::Old),
            "NEW" => Some(Self::New),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Old => "OLD",
            Self::New => "NEW",
        }
    }
}

/// The routing decision of one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub decision_id: Uuid,
    pub route: Route,
    pub decision_version: i32,
    /// The `routing_version` of the state the decision was made under.
    pub routing_version: i32,
    /// `true` when this call recorded the decision; `false` when an earlier decision for
    /// the key was found and followed (sticky, nothing written).
    pub created: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FenceError {
    #[error("routing key must be 1..={MAX_KEY_LEN} printable characters without spaces")]
    InvalidKey,
    #[error("no routing state for category {0}")]
    UnknownCategory(String),
    #[error("routing key already decided for category {existing}, not {requested}")]
    CategoryMismatch { existing: String, requested: String },
    #[error("unreadable routing row")]
    Corrupt,
    #[error("database: {}", db::summarize_db_error(.0))]
    Db(#[from] DbErr),
}

/// The routing key of the push wake-ups of one Messaging outbox event: the event's
/// envelope id (`OutboxEvent::event_id`), as the legacy guard trigger computes it from
/// `messaging_push_jobs.event_id`.
pub fn push_key(outbox_event_id: Uuid) -> String {
    format!("evt_{}", outbox_event_id.simple())
}

/// The canary rule (§9.3): a pure function of the key, `hash(key) mod 100 < percent`,
/// with the first eight bytes of SHA-256 (big-endian) as the hash. A retry therefore
/// always yields the same route, even if it bypassed the existing-decision check.
pub fn canary(key: &str, canary_percent: i16) -> bool {
    let digest = Sha256::digest(key.as_bytes());
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(prefix) % 100) < u64::from(canary_percent.clamp(0, 100) as u16)
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && !key.chars().any(|c| c.is_control() || c.is_whitespace())
}

#[derive(FromQueryResult)]
struct DecisionRow {
    decision_id: Uuid,
    category: String,
    route: String,
    decision_version: i32,
    routing_version: i32,
}

#[derive(FromQueryResult)]
struct StateRow {
    mode: String,
    canary_percent: i16,
    critical_fallback: String,
    routing_version: i32,
}

fn sticky(row: DecisionRow, category: &str) -> Result<Decision, FenceError> {
    if row.category != category {
        return Err(FenceError::CategoryMismatch {
            existing: row.category,
            requested: category.to_owned(),
        });
    }
    Ok(Decision {
        decision_id: row.decision_id,
        route: Route::parse(&row.route).ok_or(FenceError::Corrupt)?,
        decision_version: row.decision_version,
        routing_version: row.routing_version,
        created: false,
    })
}

const SELECT_DECISION: &str =
    "SELECT decision_id, category, route, decision_version, routing_version \
       FROM messaging_notification_route_decisions WHERE idempotency_key = $1 FOR UPDATE";

/// Decides the route of one message (§9.3) inside the caller's transaction, which must
/// also write the legacy rows (route `OLD`) or the publication (route `NEW`).
///
/// * An existing decision for `key` is locked and followed (sticky; nothing written),
///   whatever the category's mode is now.
/// * Otherwise the route is `NEW` only in `FENCED_NEW` for keys inside the canary, and
///   `OLD` in every other mode (`LEGACY_ONLY`, `SHADOW`, `FENCED_OLD`); a CRITICAL
///   category with `critical_fallback = FORCED` is always `OLD`. The decision is recorded
///   with reason `ROUTED` and the state's `routing_version`.
///
/// Concurrent first decisions for one key converge: the loser's insert waits for the
/// winner's commit, inserts nothing and follows the winner's decision.
pub async fn route_message(
    txn: &DatabaseTransaction,
    key: &str,
    category: &str,
    expires_at: DateTime<Utc>,
) -> Result<Decision, FenceError> {
    if !valid_key(key) {
        return Err(FenceError::InvalidKey);
    }
    if let Some(existing) =
        db::one::<DecisionRow, _>(txn, SELECT_DECISION, vec![key.into()]).await?
    {
        return sticky(existing, category);
    }
    let state = db::one::<StateRow, _>(
        txn,
        "SELECT mode, canary_percent, critical_fallback, routing_version \
           FROM messaging_notification_routing_state WHERE category = $1",
        vec![category.into()],
    )
    .await?
    .ok_or_else(|| FenceError::UnknownCategory(category.to_owned()))?;
    let mode = Mode::parse(&state.mode).ok_or(FenceError::Corrupt)?;
    let mut route = match mode {
        Mode::FencedNew if canary(key, state.canary_percent) => Route::New,
        _ => Route::Old,
    };
    if CRITICAL_CATEGORIES.contains(&category) && state.critical_fallback == "FORCED" {
        route = Route::Old;
    }
    let inserted = db::one::<DecisionRow, _>(
        txn,
        "INSERT INTO messaging_notification_route_decisions \
           (idempotency_key, decision_id, category, route, routing_version, reason, expires_at) \
         VALUES ($1, $2, $3, $4, $5, 'ROUTED', $6) \
         ON CONFLICT (idempotency_key) DO NOTHING \
         RETURNING decision_id, category, route, decision_version, routing_version",
        vec![
            key.into(),
            Uuid::new_v4().into(),
            category.into(),
            route.as_str().into(),
            state.routing_version.into(),
            expires_at.into(),
        ],
    )
    .await?;
    match inserted {
        Some(row) => Ok(Decision {
            created: true,
            ..sticky(row, category)?
        }),
        // A concurrent transaction decided the key first and has committed.
        None => sticky(
            db::one::<DecisionRow, _>(txn, SELECT_DECISION, vec![key.into()])
                .await?
                .ok_or(FenceError::Corrupt)?,
            category,
        ),
    }
}

/// `Some(ROUTING_PROTOCOL)` when exactly the routing fence history this binary knows is
/// applied; `None` otherwise (history absent or different, or the database unreachable).
/// Read-only.
pub async fn fence_protocol<C: ConnectionTrait>(db: &C) -> Option<i16> {
    match messaging_migration::notification_fence_ready(db).await {
        Ok(true) => Some(ROUTING_PROTOCOL),
        _ => None,
    }
}
