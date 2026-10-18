//! Direct-message blocks (§87).
//!
//! A block between two principals (either direction) prevents: creating a DIRECT
//! conversation between them, new direct messages and MLS commits in their DIRECT
//! conversations. Blocks never change Calling or Meeting permissions (Messaging owns
//! only its own data).
//!
//! Concurrency: block creation takes the pair's EXCLUSIVE advisory lock, every direct
//! send its SHARED lock (inside the send transaction) and re-checks — so a message
//! either committed before the block or is refused; it can never slip in after it.
//! Duplicate DIRECT conversations cannot bypass a block: creation checks it under the
//! same lock and the pair is unique per profile.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::service::{rfc3339, ConversationRow},
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::{PrincipalStatus, TrustedContext},
    state::AppState,
};

/// The canonical pair key (the same format as DIRECT conversations').
pub fn pair_key(a: Uuid, b: Uuid) -> String {
    let (x, y) = if a < b { (a, b) } else { (b, a) };
    format!("{}:{}", x.hyphenated(), y.hyphenated())
}

async fn advisory<C: ConnectionTrait>(db: &C, pair: &str, shared: bool) -> Result<()> {
    db::exec(
        db,
        if shared {
            "SELECT pg_advisory_xact_lock_shared(hashtextextended($1, 87))"
        } else {
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 87))"
        },
        vec![format!("messaging-pair:{pair}").into()],
    )
    .await?;
    Ok(())
}

/// Shared lock of a DIRECT conversation's pair (send side).
pub async fn lock_pair_shared<C: ConnectionTrait>(
    db: &C,
    conversation: &ConversationRow,
) -> Result<()> {
    match direct_pair_of(db, conversation.id).await? {
        Some(pair) => advisory(db, &pair, true).await,
        None => Ok(()),
    }
}

/// Exclusive lock of a pair (block / DIRECT creation side).
pub async fn lock_pair<C: ConnectionTrait>(db: &C, a: Uuid, b: Uuid) -> Result<()> {
    advisory(db, &pair_key(a, b), false).await
}

async fn direct_pair_of<C: ConnectionTrait>(db: &C, conversation: Uuid) -> Result<Option<String>> {
    #[derive(FromQueryResult)]
    struct P {
        direct_pair_key: Option<String>,
    }
    Ok(db::one::<P, _>(
        db,
        "SELECT direct_pair_key FROM messaging_conversations WHERE id = $1",
        vec![conversation.into()],
    )
    .await?
    .and_then(|p| p.direct_pair_key))
}

/// Whether a block exists between `a` and `b` in either direction.
pub async fn blocked_between<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    a: Uuid,
    b: Uuid,
) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_blocks WHERE environment_id = $1 AND \
           ((blocker_principal_id = $2 AND blocked_principal_id = $3) OR (blocker_principal_id = $3 AND blocked_principal_id = $2))",
        vec![environment_id.into(), a.into(), b.into()],
    )
    .await?
        > 0)
}

/// Whether `sender` may not message the other principal of a DIRECT conversation.
pub async fn direct_blocked<C: ConnectionTrait>(
    db: &C,
    conversation: &ConversationRow,
    sender: Uuid,
) -> Result<bool> {
    let Some(pair) = direct_pair_of(db, conversation.id).await? else {
        return Ok(false);
    };
    let other = pair
        .split(':')
        .filter_map(|p| Uuid::parse_str(p).ok())
        .find(|p| *p != sender);
    match other {
        Some(o) => {
            #[derive(FromQueryResult)]
            struct E {
                environment_id: Uuid,
            }
            let e: E = db::one(
                db,
                "SELECT environment_id FROM messaging_conversations WHERE id = $1",
                vec![conversation.id.into()],
            )
            .await?
            .ok_or(Error::code(ErrorCode::NotFound))?;
            blocked_between(db, e.environment_id, sender, o).await
        }
        None => Ok(false),
    }
}

pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    blocked: Uuid,
) -> Result<(u16, Value)> {
    if blocked == acting.principal_id {
        return Err(Error::validation("a principal cannot block itself"));
    }
    // Only principals of the same environment can be blocked (no existence oracle
    // beyond that: an unknown principal is the same refusal as an inactive one).
    match state.principals.resolve(&ctx.scope, blocked).await {
        Ok(PrincipalStatus::NotFound) => {
            return Err(Error::reason(
                ErrorCode::PrincipalNotAuthorized,
                "PRINCIPAL_NOT_AVAILABLE",
            ))
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "principal directory unavailable");
            return Err(Error::code(ErrorCode::ServiceUnavailable));
        }
    }
    let st = state.clone();
    let env = ctx.scope.environment_id;
    let me = acting.principal_id;
    let ctx2 = ctx.clone();
    state
        .db
        .transaction::<_, (u16, Value), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                lock_pair(txn, me, blocked).await?;
                let n = db::exec(
                    txn,
                    "INSERT INTO messaging_blocks (environment_id, blocker_principal_id, blocked_principal_id, created_at) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                    vec![env.into(), me.into(), blocked.into(), now.into()],
                )
                .await?;
                if n == 1 {
                    crate::audit::record(
                        txn,
                        crate::audit::AuditEntry {
                            ctx: &ctx2,
                            principal_id: Some(me),
                            action: "PRINCIPAL_BLOCKED",
                            target_kind: "PRINCIPAL",
                            target_id: blocked,
                            detail: json!({}),
                        },
                        &crate::conversations::service::request_context(),
                        now,
                    )
                    .await?;
                    crate::metrics::inc("messaging_blocks_created_total", &[]);
                }
                Ok((if n == 1 { 201 } else { 200 }, view(blocked, now)))
            })
        })
        .await
        .map_err(Error::from)
}

fn view(blocked: Uuid, at: DateTime<Utc>) -> Value {
    json!({ "blockedPrincipalId": blocked, "createdAt": rfc3339(at) })
}

pub async fn remove(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    blocked: Uuid,
) -> Result<Value> {
    let n = db::exec(
        &state.db,
        "DELETE FROM messaging_blocks WHERE environment_id = $1 AND blocker_principal_id = $2 AND blocked_principal_id = $3",
        vec![ctx.scope.environment_id.into(), acting.principal_id.into(), blocked.into()],
    )
    .await?;
    if n == 0 {
        return Err(Error::code(ErrorCode::NotFound));
    }
    Ok(json!({ "blockedPrincipalId": blocked, "removed": true }))
}

pub async fn list(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    #[derive(FromQueryResult)]
    struct B {
        blocked_principal_id: Uuid,
        created_at: DateTime<Utc>,
    }
    let rows: Vec<B> = db::all(
        &state.db,
        "SELECT blocked_principal_id, created_at FROM messaging_blocks WHERE environment_id = $1 AND blocker_principal_id = $2 \
         ORDER BY created_at LIMIT 1000",
        vec![ctx.scope.environment_id.into(), acting.principal_id.into()],
    )
    .await?;
    Ok(
        json!({ "blocks": rows.iter().map(|b| view(b.blocked_principal_id, b.created_at)).collect::<Vec<_>>() }),
    )
}
