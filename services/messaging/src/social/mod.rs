//! Status (WhatsApp-style 24-hour updates) and Channels (ADR 0069).
//!
//! * [`media`]: photos, videos and voice notes. One object per part in protected object
//!   storage, uploaded with signed single-object writes (size and SHA-256 fixed by the
//!   signature): resumable (the server lists the parts it holds), idempotent per part,
//!   parallel, cancellable. Usable only once COMPLETE; read through short-lived signed
//!   reads minted only after the reader is authorized.
//! * [`status`]: statuses with an audience SNAPSHOT taken at publish time. Every read
//!   (feed, detail, rings, media, viewers, reactions, replies, shares) goes through one
//!   gate: the reader is the author, or the status is live AND the reader is in its
//!   snapshot AND the two are still MUTUAL, unblocked contacts whose relationship has not
//!   restarted since the snapshot (Communications Hub, `contacts/eligible`) AND no
//!   Messaging block exists either way. Anything else is `404 NOT_FOUND`.
//! * [`audience`]: the remembered default, close friends, named lists, mutes, the
//!   eligible-contact list of the audience picker.
//! * [`channels`]: broadcast channels (ADR 0072: handles, private channels, admins, rich
//!   posts, polls, scheduling, discovery, analytics; verification and moderation by HQ).
//! * [`notices`]: mention / share notices for the Notification Service (off by default).
//!
//! Status content is stored server-side (it is NOT end-to-end encrypted) with a
//! server-enforced audience.

pub mod audience;
pub mod channels;
pub mod http;
/// ADR 0069: one-time import of the retired Meeting Service channels (`messaging-admin`).
pub mod legacy_import;
pub mod media;
pub mod notices;
pub mod status;

use std::collections::{HashMap, HashSet};

use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode, Result},
    identity::TrustedContext,
    state::AppState,
};

/// A status lives this long.
pub const STATUS_LIFETIME_HOURS: i64 = 24;
/// A video status segment, a voice status or a music clip: at most one minute.
pub const MAX_SEGMENT_MS: i32 = 60_000;
/// Rounding slack on a media duration measured by the client.
pub const DURATION_SLACK_MS: i32 = 1_000;

/// The actor's eligible contacts: principal → when the relationship last (re)started.
pub type Contacts = HashMap<Uuid, DateTime<Utc>>;

pub fn not_found() -> Error {
    Error::code(ErrorCode::NotFound)
}

/// The actor's MUTUAL, unblocked, active contacts (Communications Hub, the source of truth
/// for contacts and personal blocks). A scope Communications does not govern (or no
/// Communications configured) has none: statuses are then visible to their author only.
/// Communications unreachable is `503` (fail closed: never a wider audience).
pub async fn eligible_contacts(
    state: &AppState,
    ctx: &TrustedContext,
    actor: Uuid,
    among: Option<&[Uuid]>,
) -> Result<Contacts> {
    let Some(client) = state
        .communications
        .as_ref()
        .filter(|c| c.governs(crate::communications::scope(ctx)))
    else {
        return Ok(HashMap::new());
    };
    if among.is_some_and(|a| a.is_empty()) {
        return Ok(HashMap::new());
    }
    let mut out = HashMap::new();
    let chunks: Vec<Option<&[Uuid]>> = match among {
        Some(a) => a.chunks(256).map(Some).collect(),
        None => vec![None],
    };
    for chunk in chunks {
        let list = client
            .eligible_contacts(ctx.scope.delegated(), actor, chunk)
            .await
            .map_err(|e| match e {
                cf_communications_policy::PolicyError::Denied => not_found(),
                cf_communications_policy::PolicyError::Unavailable => {
                    Error::reason(ErrorCode::ServiceUnavailable, "COMMUNICATIONS_UNAVAILABLE")
                }
            })?;
        for c in list {
            if c.principal_id == actor {
                continue;
            }
            if let Ok(t) = DateTime::parse_from_rfc3339(&c.eligible_since) {
                out.insert(c.principal_id, t.with_timezone(&Utc));
            }
        }
    }
    Ok(out)
}

#[derive(FromQueryResult)]
struct BlockRow {
    other: Uuid,
}

/// Principals with a Messaging block (either direction) against `me`.
pub async fn blocked_with<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    me: Uuid,
) -> Result<HashSet<Uuid>> {
    let rows: Vec<BlockRow> = db::all(
        db,
        "SELECT blocked_principal_id AS other FROM messaging_blocks WHERE environment_id = $1 AND blocker_principal_id = $2 \
         UNION SELECT blocker_principal_id AS other FROM messaging_blocks WHERE environment_id = $1 AND blocked_principal_id = $2",
        vec![environment_id.into(), me.into()],
    )
    .await?;
    Ok(rows.into_iter().map(|r| r.other).collect())
}

/// Trimmed text of at most `max` characters (line breaks kept); empty is `None`.
pub fn clean_text(value: &Option<String>, max: usize, what: &str) -> Result<Option<String>> {
    match value.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(t) if t.chars().count() > max => Err(Error::validation(format!(
            "{what} can be at most {max} characters"
        ))),
        Some(t)
            if t.chars()
                .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t') =>
        {
            Err(Error::validation(format!(
                "{what} contains control characters"
            )))
        }
        Some(t) => Ok(Some(t.to_string())),
    }
}

/// `#rrggbb`.
pub fn valid_color(v: &str) -> bool {
    v.len() == 7 && v.starts_with('#') && v[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

// ------------------------------------------------------------------ idempotency records

/// The optional `Idempotency-Key` of a creation (Messaging's idempotency records).
pub fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>> {
    let Some(v) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let key = v
        .to_str()
        .ok()
        .filter(|k| crate::conversations::service::valid_idempotency_key(k))
        .ok_or_else(|| {
            Error::validation("Idempotency-Key must be 8-128 characters of [A-Za-z0-9._:-]")
        })?;
    Ok(Some(key.to_string()))
}

/// One idempotent creation: the operation, the client's key and the request digest
/// (the acting principal included, so another principal's reuse is refused).
pub struct Idempotent {
    pub operation: &'static str,
    pub key: String,
    pub digest: String,
}

impl Idempotent {
    pub fn new(operation: &'static str, key: String, actor: Uuid, request: &Value) -> Self {
        let canonical = crate::conversations::policy::canonical_json(&serde_json::json!({
            "request": request,
            "actingPrincipalId": actor,
        }));
        Self {
            operation,
            key,
            digest: hex::encode(Sha256::digest(canonical.as_bytes())),
        }
    }
}

#[derive(FromQueryResult)]
struct IdempotencyRow {
    request_digest: String,
    response_view: Option<Value>,
}

/// An earlier completed creation with this key: its stored response. Another request
/// under the same key is `422 IDEMPOTENCY_KEY_REUSED`; one still in flight `409`.
pub async fn replayed<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    idem: &Idempotent,
    now: DateTime<Utc>,
) -> Result<Option<Value>> {
    let row: Option<IdempotencyRow> = db::one(
        db,
        "SELECT request_digest, response_view FROM messaging_idempotency_keys \
          WHERE environment_id = $1 AND operation = $2 AND idempotency_key = $3 AND expires_at > $4",
        vec![
            environment_id.into(),
            idem.operation.into(),
            idem.key.clone().into(),
            now.into(),
        ],
    )
    .await?;
    match row {
        None => Ok(None),
        Some(r) if r.request_digest != idem.digest => {
            Err(Error::code(ErrorCode::IdempotencyKeyReused))
        }
        Some(IdempotencyRow {
            response_view: Some(v),
            ..
        }) => Ok(Some(v)),
        Some(_) => Err(Error::code(ErrorCode::Conflict)),
    }
}

/// Claims the key inside the creation's transaction (a concurrent retry waits for it).
/// `Ok(Some(view))`: an earlier creation finished meanwhile — replay it.
pub async fn claim<C: ConnectionTrait>(
    txn: &C,
    environment_id: Uuid,
    idem: &Idempotent,
    now: DateTime<Utc>,
) -> Result<Option<Value>> {
    // Expired records of this key are replaced (a key is remembered for 24 hours).
    db::exec(
        txn,
        "DELETE FROM messaging_idempotency_keys WHERE environment_id = $1 AND operation = $2 \
           AND idempotency_key = $3 AND expires_at <= $4",
        vec![
            environment_id.into(),
            idem.operation.into(),
            idem.key.clone().into(),
            now.into(),
        ],
    )
    .await?;
    let claimed = db::exec(
        txn,
        "INSERT INTO messaging_idempotency_keys (environment_id, idempotency_key, operation, request_digest, \
           created_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
        vec![
            environment_id.into(),
            idem.key.clone().into(),
            idem.operation.into(),
            idem.digest.clone().into(),
            now.into(),
            (now + chrono::Duration::hours(24)).into(),
        ],
    )
    .await?;
    if claimed == 1 {
        return Ok(None);
    }
    replayed(txn, environment_id, idem, now).await
}

/// Records the creation's response under its key (same transaction).
pub async fn remember<C: ConnectionTrait>(
    txn: &C,
    environment_id: Uuid,
    idem: &Idempotent,
    view: &Value,
) -> Result<()> {
    db::exec(
        txn,
        "UPDATE messaging_idempotency_keys SET response_status = 201, response_view = $4 \
          WHERE environment_id = $1 AND operation = $2 AND idempotency_key = $3",
        vec![
            environment_id.into(),
            idem.operation.into(),
            idem.key.clone().into(),
            view.clone().into(),
        ],
    )
    .await?;
    Ok(())
}

// ------------------------------------------------------------------------- the sweep

/// Background sweep: expired statuses (and reshares of them), unused or abandoned media
/// with their stored bytes, settled notices. Bounded and idempotent on every instance.
pub async fn run_sweep(state: AppState, interval: std::time::Duration) {
    loop {
        match sweep(&state).await {
            Ok((0, 0)) => {}
            Ok((statuses, media)) => {
                tracing::info!(
                    statuses,
                    media,
                    "status / channel sweep removed expired data"
                )
            }
            Err(e) => tracing::warn!(error = %e, "status / channel sweep failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

/// One sweep pass: (statuses removed, media removed). A database without the tables is
/// a no-op.
pub async fn sweep(state: &AppState) -> Result<(u64, u64)> {
    let now = state.now();
    let statuses = match status::purge_expired(&state.db, now).await {
        Err(e) if matches!(&e, Error::Database { summary, .. } if summary.contains("42P01")) => {
            return Ok((0, 0))
        }
        other => other?,
    };
    // ADR 0072: due scheduled channel posts, follower-count drift.
    channels::sweep(state).await?;
    let media = media::purge_unused(state, now, 200).await?;
    notices::maintain(state, 500).await?;
    Ok((statuses, media))
}
