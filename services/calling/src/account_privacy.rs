//! S2 — account deletion and data download: Calling's part, over internal service
//! authentication (Meeting Service only, `calling-account.privacy`).
//!
//! The Meeting Service is the account authority. When a person deletes their CF Meet
//! account it queues one durable, retried erasure job per owner service; for Calling it
//! names the person's principal in the CF Meet FIRST_PARTY scope (the signed delegated
//! context). The data download reads the same history back.
//!
//! * `POST /internal/v1/account-privacy/erasure` `{principalId}` — every FINISHED Call the
//!   principal took part in leaves its history (`history_hidden_at`, reason `USER`: the
//!   person asked for it) and the display names of its participations in those Calls are
//!   erased (`display_minimized_at`). Calls, events, usage and spend stay as evidence with
//!   the opaque principal id only (exactly what the retention worker does after
//!   `CALLING_RETENTION_CALL_HISTORY_DAYS`). A Call still in progress is left alone and
//!   reported in `pendingLiveCalls`; `complete` is true only when none remains, so the caller
//!   retries until then. Idempotent: a repeat changes nothing and answers the same counts
//!   of what is left (zero).
//! * `POST /internal/v1/account-privacy/export` `{principalId}` — the principal's Calls
//!   (newest first, at most [`EXPORT_LIMIT`]): id, kind, state, its role, times. Safe
//!   fields only: no provider, edge or contact data, no other participant's name.
//!
//! Both act only inside the delegated scope; a principal of another scope is simply not
//! found (empty answer), never an error that tells scopes apart.

use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    response::Response,
    routing::post,
    Json, Router,
};
use cf_service_auth::InternalPermission;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    calls::app::http::{parse_body, respond},
    error::CallingError,
    internal_auth::middleware::InternalCaller,
    retention::{
        audit::{self, RetentionAuditEntry},
        DataClass, RetentionAction, RetentionOutcome,
    },
    state::AppState,
};

/// At most this many Calls in one export.
pub const EXPORT_LIMIT: i64 = 1000;
/// The policy label of an account erasure in the retention audit.
pub const ERASURE_POLICY: &str = "ACCOUNT_ERASURE";
/// Call states after which a Call is finished (history may hide it).
const TERMINAL: &str = "('ENDED','DECLINED','BUSY','NO_ANSWER','FAILED','CANCELLED')";

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/account-privacy/erasure", post(erasure))
        .route("/account-privacy/export", post(export))
        .layer(axum::extract::DefaultBodyLimit::max(4 * 1024))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrincipalBody {
    principal_id: Uuid,
}

fn scope_of(caller: &InternalCaller) -> Result<[Uuid; 3], CallingError> {
    let d = caller
        .context()
        .delegated_platform_context()
        .ok_or(CallingError::Validation(
            "A delegated platform context is required.",
        ))?;
    Ok([
        d.tenant_id(),
        d.application_id(),
        d.application_environment_id(),
    ])
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn unavailable(_: sea_orm::DbErr) -> CallingError {
    CallingError::ServiceUnavailable
}

async fn erasure(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<PrincipalBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    caller.require_permission(InternalPermission::CallingAccountPrivacy)?;
    let body = parse_body(body)?;
    if body.principal_id.is_nil() {
        return Err(CallingError::Validation("principalId is invalid."));
    }
    let [tenant, application, environment] = scope_of(&caller)?;
    let now = Utc::now();
    let scoped = |extra: Vec<sea_orm::Value>| {
        let mut v: Vec<sea_orm::Value> = vec![
            tenant.into(),
            application.into(),
            environment.into(),
            body.principal_id.into(),
        ];
        v.extend(extra);
        v
    };
    let txn = state.db.begin().await.map_err(unavailable)?;
    // The only way the minimization guard accepts a change (same as the retention worker).
    txn.execute_unprepared("SET LOCAL calling.retention_worker = 'on'")
        .await
        .map_err(unavailable)?;
    let hidden = txn
        .execute(stmt(
            &format!(
                "UPDATE call_participants p SET history_hidden_at = $5, history_hidden_reason = 'USER' \
                   FROM calls c WHERE c.id = p.call_id AND c.current_state IN {TERMINAL} \
                    AND p.tenant_id = $1 AND p.application_id = $2 AND p.environment_id = $3 \
                    AND p.principal_id = $4 AND p.history_hidden_at IS NULL"
            ),
            scoped(vec![now.into()]),
        ))
        .await
        .map_err(unavailable)?
        .rows_affected();
    let minimized = txn
        .execute(stmt(
            &format!(
                "UPDATE call_participants p SET display_name = NULL, display_minimized_at = $5 \
                   FROM calls c WHERE c.id = p.call_id AND c.current_state IN {TERMINAL} \
                    AND p.tenant_id = $1 AND p.application_id = $2 AND p.environment_id = $3 \
                    AND p.principal_id = $4 AND p.display_minimized_at IS NULL"
            ),
            scoped(vec![now.into()]),
        ))
        .await
        .map_err(unavailable)?
        .rows_affected();
    txn.execute_unprepared("SET LOCAL calling.retention_worker = 'off'")
        .await
        .map_err(unavailable)?;
    let live: i64 = txn
        .query_one(stmt(
            &format!(
                "SELECT count(*) FROM call_participants p JOIN calls c ON c.id = p.call_id \
                  WHERE p.tenant_id = $1 AND p.application_id = $2 AND p.environment_id = $3 \
                    AND p.principal_id = $4 AND c.current_state NOT IN {TERMINAL}"
            ),
            scoped(Vec::new()),
        ))
        .await
        .map_err(unavailable)?
        .and_then(|r| r.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);
    if hidden + minimized > 0 {
        audit::record(
            &txn,
            &RetentionAuditEntry {
                class: DataClass::Principal,
                action: RetentionAction::Minimize,
                resource_ref: Some(body.principal_id),
                batch_count: u32::try_from(hidden.max(minimized)).unwrap_or(u32::MAX),
                policy: ERASURE_POLICY.to_string(),
                actor: caller
                    .context()
                    .authenticated_service()
                    .as_str()
                    .to_string(),
                outcome: RetentionOutcome::Minimized,
            },
            now,
        )
        .await
        .map_err(|_| CallingError::ServiceUnavailable)?;
    }
    txn.commit().await.map_err(unavailable)?;
    // `outcome` is on the bounded label allow-list (tests/metric_safety.rs).
    crate::metrics::inc(
        "calling_account_erasures_total",
        &[(
            "outcome",
            if live == 0 {
                "complete"
            } else {
                "pending_live_calls"
            },
        )],
    );
    tracing::info!(
        hidden,
        minimized,
        pending_live_calls = live,
        "account call history erased"
    );
    Ok(respond(
        StatusCode::OK,
        json!({
            "schemaVersion": 1,
            "principalId": body.principal_id,
            "historyEntriesHidden": hidden,
            "displayNamesErased": minimized,
            "pendingLiveCalls": live,
            "complete": live == 0,
        }),
        false,
    ))
}

async fn export(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<PrincipalBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    caller.require_permission(InternalPermission::CallingAccountPrivacy)?;
    let body = parse_body(body)?;
    if body.principal_id.is_nil() {
        return Err(CallingError::Validation("principalId is invalid."));
    }
    let [tenant, application, environment] = scope_of(&caller)?;
    let row = state
        .db
        .query_one(stmt(
            "SELECT COALESCE(jsonb_agg(x ORDER BY x.\"requestedAt\" DESC), '[]'::jsonb) FROM ( \
               SELECT c.id AS \"callId\", c.call_kind AS \"kind\", c.current_state AS \"state\", \
                      p.role AS \"role\", c.requested_at AS \"requestedAt\", c.answered_at AS \"answeredAt\", \
                      c.ended_at AS \"endedAt\", p.history_hidden_at IS NOT NULL AS \"hiddenFromHistory\" \
                 FROM call_participants p JOIN calls c ON c.id = p.call_id \
                WHERE p.tenant_id = $1 AND p.application_id = $2 AND p.environment_id = $3 \
                  AND p.principal_id = $4 \
                ORDER BY c.requested_at DESC LIMIT $5) x",
            vec![
                tenant.into(),
                application.into(),
                environment.into(),
                body.principal_id.into(),
                EXPORT_LIMIT.into(),
            ],
        ))
        .await
        .map_err(unavailable)?;
    let calls: Value = row
        .and_then(|r| r.try_get_by_index::<Value>(0).ok())
        .unwrap_or_else(|| json!([]));
    let truncated = calls
        .as_array()
        .map(|a| a.len() as i64 >= EXPORT_LIMIT)
        .unwrap_or(false);
    Ok(respond(
        StatusCode::OK,
        json!({
            "schemaVersion": 1,
            "principalId": body.principal_id,
            "calls": calls,
            "truncated": truncated,
        }),
        false,
    ))
}
