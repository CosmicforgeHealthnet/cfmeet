//! B2: cached "allowed" personal group send decisions.
//!
//! Every personal group message asks Communications whether the sender's account may still
//! send (`require_send`, `PersonalActivity`, twice per E2EE send). One-to-one sends
//! (`MessageSend`) are NOT cached: a block must refuse the very next message, and the
//! events below reach Messaging through Communications' asynchronous outbox relay, so a
//! cached allow could outlive a block by the relay delay. The answer changes only
//! when a block, a contact, a privacy setting or an account's status changes — and
//! Communications announces each of those to Messaging as a durable event
//! (`communications.block.changed`, `communications.connection.*`,
//! `communications.privacy.changed`, `communications.account.changed`, ...).
//!
//! An ALLOWED decision is therefore reused for at most [`ALLOW_TTL`] per (scope, action,
//! sender, recipient, credential), and only while neither principal's invalidation
//! sequence (`messaging_policy_invalidations`, bumped in the same transaction that applies
//! such an event) moved since the decision was taken. The sequence lives in PostgreSQL, so
//! every Messaging instance stops using a stale decision as soon as the event is applied,
//! not only the one that received it. The sequence is read BEFORE Communications is
//! asked, so an event that lands while the check is in flight still invalidates it.
//!
//! A DENIED decision (or an unavailable Communications) is never cached: the next send
//! asks again. Causes Communications does not announce (a platform principal disabled, a
//! legal hold) are bounded by [`ALLOW_TTL`].

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use cf_communications_policy::Action;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

use crate::identity::TrustedContext;

/// How long an allowed send decision is reused (absent an invalidating event).
pub const ALLOW_TTL: Duration = Duration::from_secs(30);

/// Bound on cached decisions per instance (cleared wholesale when reached).
const MAX_ENTRIES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    action: u8,
    actor: Uuid,
    target: Uuid,
    credential_issued_at: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    at: Instant,
    seq: i64,
}

static CACHE: LazyLock<Mutex<HashMap<Key, Entry>>> = LazyLock::new(Default::default);

fn action_code(action: Action) -> Option<u8> {
    match action {
        // A one-to-one send is never cached: a block takes effect at once (privacy
        // guarantee), while the invalidating event reaches Messaging asynchronously.
        Action::PersonalActivity => Some(2),
        _ => None,
    }
}

fn key(ctx: &TrustedContext, action: u8, actor: Uuid, target: Uuid) -> Key {
    Key {
        tenant_id: ctx.scope.tenant_id,
        application_id: ctx.scope.application_id,
        environment_id: ctx.scope.environment_id,
        action,
        actor,
        target,
        credential_issued_at: ctx.personal_session_issued_at,
    }
}

/// The current invalidation sequence of the pair (0: never invalidated). `None` when it
/// cannot be read (the decision is then neither served from nor stored in the cache).
async fn sequence<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    actor: Uuid,
    target: Uuid,
) -> Option<i64> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COALESCE(max(seq), 0) AS seq FROM messaging_policy_invalidations \
              WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
                AND principal_id IN ($4, $5)",
            [
                ctx.scope.tenant_id.into(),
                ctx.scope.application_id.into(),
                ctx.scope.environment_id.into(),
                actor.into(),
                target.into(),
            ],
        ))
        .await
        .ok()??;
    row.try_get::<i64>("", "seq").ok()
}

/// [`super::require`] for `MessageSend` / `PersonalActivity`, reusing a recent allowed
/// decision (module docs). Any other action is never cached.
pub(super) async fn require_cached<C: ConnectionTrait>(
    db: &C,
    state: &crate::state::AppState,
    ctx: &TrustedContext,
    action: Action,
    actor: Uuid,
    target: Uuid,
) -> crate::error::Result<()> {
    let Some(code) = action_code(action) else {
        return super::require(state, ctx, action, actor, target).await;
    };
    let Some(seq) = sequence(db, ctx, actor, target).await else {
        return super::require(state, ctx, action, actor, target).await;
    };
    let k = key(ctx, code, actor, target);
    let hit = {
        let cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        cache
            .get(&k)
            .is_some_and(|e| e.seq == seq && e.at.elapsed() < ALLOW_TTL)
    };
    if hit {
        crate::metrics::inc("messaging_send_policy_cache_total", &[("outcome", "hit")]);
        return Ok(());
    }
    crate::metrics::inc("messaging_send_policy_cache_total", &[("outcome", "miss")]);
    super::require(state, ctx, action, actor, target).await?;
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if cache.len() >= MAX_ENTRIES {
        cache.retain(|_, e| e.at.elapsed() < ALLOW_TTL);
        if cache.len() >= MAX_ENTRIES {
            cache.clear();
        }
    }
    cache.insert(
        k,
        Entry {
            at: Instant::now(),
            seq,
        },
    );
    Ok(())
}

/// The principals a Communications event is about (its relationship / account fields).
pub(super) fn event_principals(data: &serde_json::Value) -> Vec<Uuid> {
    let mut out: Vec<Uuid> = ["actorPrincipalId", "targetPrincipalId", "principalId"]
        .iter()
        .filter_map(|f| data.get(*f)?.as_str())
        .filter_map(|s| Uuid::parse_str(s).ok())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Bumps the invalidation sequence of `principals` in the event's transaction: every
/// cached decision involving one of them stops being used on every instance.
pub(super) async fn invalidate<C: ConnectionTrait>(
    db: &C,
    scope: crate::identity::OwnerScope,
    principals: &[Uuid],
) -> crate::error::Result<()> {
    for principal in principals {
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO messaging_policy_invalidations (tenant_id, application_id, environment_id, principal_id) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (tenant_id, application_id, environment_id, principal_id) \
             DO UPDATE SET seq = nextval('messaging_policy_invalidation_seq'), updated_at = now()",
            [
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                (*principal).into(),
            ],
        ))
        .await
        .map_err(crate::error::Error::from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_principals_are_the_relationship_and_account_fields_only() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let data =
            serde_json::json!({"actorPrincipalId": a, "targetPrincipalId": b, "blocked": true});
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(event_principals(&data), want);
        let data = serde_json::json!({"principalId": a, "state": "SUSPENDED", "conversationId": b});
        assert_eq!(event_principals(&data), vec![a]);
        assert!(event_principals(&serde_json::json!({"reason": "CALL_STARTED"})).is_empty());
    }

    #[test]
    fn only_send_decisions_are_cacheable() {
        assert_eq!(action_code(Action::MessageSend), None);
        assert_eq!(action_code(Action::PersonalActivity), Some(2));
        assert_eq!(action_code(Action::DirectCall), None);
        assert_eq!(action_code(Action::MessageNotification), None);
    }
}
