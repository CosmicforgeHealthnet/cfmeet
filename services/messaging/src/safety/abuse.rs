//! Abuse controls from metadata and operational signals (§88).
//!
//! Signals are rates and counts (message velocity, new direct contacts, conversation
//! bursts, uploads, reports, MANAGED link velocity) — never the content of a
//! PRIVATE_E2EE conversation, which the server cannot see. When a principal accumulates
//! `MESSAGING_ABUSE_SIGNAL_THRESHOLD` signals within an hour it is automatically
//! restricted from sending for `MESSAGING_ABUSE_RESTRICTION_SECONDS` and a
//! `SUSPENSION_RECOMMENDED` audit entry is written: suspending a principal is Platform
//! Access's authority, never Messaging's. Moderators can also restrict / lift.

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde_json::json;
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode, Result},
    identity::TrustedContext,
    state::AppState,
};

/// Refuses sending while an active restriction exists.
pub async fn require_not_restricted(
    state: &AppState,
    ctx: &TrustedContext,
    principal: Uuid,
) -> Result<()> {
    if let Some(until) = restricted_until(state, ctx.scope.environment_id, principal).await? {
        tracing::info!(until = %until, "restricted principal refused");
        return Err(Error::reason(
            ErrorCode::SendRestricted,
            "PRINCIPAL_RESTRICTED",
        ));
    }
    Ok(())
}

pub async fn restricted_until(
    state: &AppState,
    environment_id: Uuid,
    principal: Uuid,
) -> Result<Option<DateTime<Utc>>> {
    #[derive(FromQueryResult)]
    struct R {
        ends_at: DateTime<Utc>,
    }
    Ok(db::one::<R, _>(
        &state.db,
        "SELECT max(ends_at) AS ends_at FROM messaging_principal_restrictions WHERE environment_id = $1 \
           AND principal_id = $2 AND lifted_at IS NULL AND starts_at <= $3 AND ends_at > $3 HAVING max(ends_at) IS NOT NULL",
        vec![environment_id.into(), principal.into(), state.now().into()],
    )
    .await?
    .map(|r| r.ends_at))
}

/// Records one abuse signal and applies the automatic restriction when the threshold is
/// crossed. Never fails the caller's request (a failure is logged).
pub async fn signal(state: &AppState, ctx: &TrustedContext, principal: Uuid, kind: &'static str) {
    if let Err(e) = record(state, ctx, principal, kind).await {
        tracing::warn!(error = %e, "abuse signal not recorded");
    }
}

async fn record(
    state: &AppState,
    ctx: &TrustedContext,
    principal: Uuid,
    kind: &'static str,
) -> Result<()> {
    let now = state.now();
    let env = ctx.scope.environment_id;
    db::exec(
        &state.db,
        "INSERT INTO messaging_abuse_signals (id, environment_id, principal_id, signal, occurred_at) VALUES ($1, $2, $3, $4, $5)",
        vec![Uuid::new_v4().into(), env.into(), principal.into(), kind.into(), now.into()],
    )
    .await?;
    crate::metrics::inc("messaging_abuse_signals_total", &[("signal", kind)]);
    let recent = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_abuse_signals WHERE environment_id = $1 AND principal_id = $2 AND occurred_at > $3",
        vec![env.into(), principal.into(), (now - chrono::Duration::hours(1)).into()],
    )
    .await?;
    if recent >= state.conversations.abuse_signal_threshold
        && restricted_until(state, env, principal).await?.is_none()
    {
        let ends = now + chrono::Duration::seconds(state.conversations.abuse_restriction_seconds);
        db::exec(
            &state.db,
            "INSERT INTO messaging_principal_restrictions (id, tenant_id, application_id, environment_id, principal_id, \
               reason, source, starts_at, ends_at) VALUES ($1, $2, $3, $4, $5, $6, 'AUTOMATIC', $7, $8)",
            vec![
                Uuid::new_v4().into(),
                ctx.scope.tenant_id.into(),
                ctx.scope.application_id.into(),
                env.into(),
                principal.into(),
                kind.into(),
                now.into(),
                ends.into(),
            ],
        )
        .await?;
        crate::audit::record(
            &state.db,
            crate::audit::AuditEntry {
                ctx,
                principal_id: Some(principal),
                action: "SUSPENSION_RECOMMENDED",
                target_kind: "PRINCIPAL",
                target_id: principal,
                detail: json!({ "signals_last_hour": recent, "automatic_restriction_until": ends.to_rfc3339(), "last_signal": kind }),
            },
            &crate::conversations::service::request_context(),
            now,
        )
        .await?;
        crate::metrics::inc(
            "messaging_abuse_restrictions_total",
            &[("source", "automatic")],
        );
    }
    Ok(())
}
