//! Privacy-safe call evidence (Part D §36):
//! `GET /v1/privacy-relay/sessions/{relay_session_id}/evidence`.
//!
//! Calling is the composition layer: it authenticates the customer credential
//! (`privacy.evidence.read`), asks Privacy Relay — over the authenticated internal API,
//! in the caller's signed delegated scope — whether the Relay Session exists FOR THIS
//! CALLER (a foreign or unknown session is the same 404), then reads ONLY its own Call
//! records (the protected Calls whose opaque `privacy_relay_ref` is that session). No
//! cross-service database access, and Privacy Relay never stores call records.
//!
//! Each Call's evidence is support / dispute grade and privacy-safe: ids, the context /
//! session references, the canonical timestamps, the final state, the normalized
//! terminal reason and failure category (never a vendor string), the connected duration
//! from Calling's authoritative timing (never `requested_at → ended_at`), per-participant
//! connected time from session intervals, attempt timing and normalized outcomes, the
//! provider-independent route class and the Privacy Relay decision ids it relied on.
//! Never a telephone number (raw or masked), Contact Vault material, provider / carrier
//! identifiers, provider payloads or credentials. NO_ANSWER comes from the canonical
//! Call record, so it survives a lost push or realtime delivery.
//!
//! Keyset pagination (`created_at DESC, id DESC`), at most 100 per page.

use axum::{
    extract::{rejection::QueryRejection, Path, Query, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{PrivacyRelayError, ProxyRequest};
use crate::{
    calls::{
        ids::CallId,
        read_model::{ts, ts_opt, CallCursor},
        service::ListCallsQuery,
        CallScope,
    },
    error::CallingError,
    internal_auth::delegation::delegated_context,
    platform::{context::PlatformAuthContext, scopes::names},
    state::AppState,
};

const DEFAULT_LIMIT: u64 = 50;

fn session_not_found() -> Response {
    let mut r = (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": {
            "code": "SESSION_NOT_FOUND",
            "message": "The privacy relay session was not found.",
        }})),
    )
        .into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn query(pairs: &[(String, String)]) -> Result<(u64, Option<CallCursor>), CallingError> {
    let (mut limit, mut cursor, mut seen) = (DEFAULT_LIMIT, None, Vec::new());
    for (key, value) in pairs {
        if seen.contains(&key.as_str()) {
            return Err(CallingError::Validation("A query parameter is repeated."));
        }
        seen.push(key);
        match key.as_str() {
            "limit" => {
                limit = value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=ListCallsQuery::MAX_LIMIT).contains(n))
                    .ok_or(CallingError::Validation(
                        "limit must be an integer between 1 and 100.",
                    ))?
            }
            "cursor" => {
                cursor = Some(
                    CallCursor::decode(value)
                        .ok_or(CallingError::Validation("The cursor is invalid."))?,
                )
            }
            _ => return Err(CallingError::Validation("Unknown query parameter.")),
        }
    }
    Ok((limit, cursor))
}

/// The Privacy Relay decisions a Call relied on (Calling's own record; ids only).
async fn decisions(
    state: &AppState,
    scope: CallScope,
    call: Uuid,
) -> Result<Vec<Value>, CallingError> {
    let rows = state
        .db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT decision_ref, purpose, context_version, session_version, decided_at \
             FROM call_privacy_authorizations \
             WHERE call_id = $1 AND tenant_id = $2 AND application_id = $3 AND environment_id = $4 \
             ORDER BY decided_at, id",
            vec![
                call.into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
            ],
        ))
        .await
        .map_err(|_| CallingError::Internal)?;
    rows.iter()
        .map(|r| {
            let get = |e| CallingError::from(crate::calls::CallError::from(e));
            Ok(json!({
                "decisionId": r.try_get_by_index::<Uuid>(0).map_err(get)?,
                "purpose": r.try_get_by_index::<String>(1).map_err(get)?,
                "contextVersion": r.try_get_by_index::<i64>(2).map_err(get)?,
                "relaySessionVersion": r.try_get_by_index::<i64>(3).map_err(get)?,
                "decidedAt": ts(r.try_get_by_index(4).map_err(get)?),
            }))
        })
        .collect()
}

/// The privacy-safe evidence of one protected Call.
async fn call_evidence(
    state: &AppState,
    scope: CallScope,
    call: CallId,
) -> Result<Value, CallingError> {
    let detail = state.calls.call_detail(scope, call).await?;
    let attempts = state.calls.attempts(scope, call).await?;
    let routes = state.calls.routes(scope, call).await?;
    let c = &detail.call;
    let t = &c.timestamps;
    let first_attempt = attempts.iter().map(|a| a.started_at).min();
    let participants: Vec<Value> = detail
        .participants
        .iter()
        .map(|p| {
            json!({
                "participantId": p.participant.id.to_string(),
                "principalId": p.participant.principal_id.as_uuid(),
                "role": p.participant.role.as_str(),
                "connectedDurationMs": p.connected_duration_ms,
            })
        })
        .collect();
    let attempt_views: Vec<Value> = attempts
        .iter()
        .map(|a| {
            json!({
                "attemptNumber": a.attempt_number,
                "startedAt": ts(a.started_at),
                "finishedAt": ts_opt(a.finished_at),
                "outcome": a.outcome.as_str(),
                "unknownOutcome": a.unknown_outcome,
                "failureCategory": a.failure_category.map(|f| f.as_str()),
            })
        })
        .collect();
    Ok(json!({
        "callId": c.id.to_string(),
        "contextId": c.communication_context_ref,
        "relaySessionId": c.privacy_relay_ref,
        "kind": c.kind.as_str(),
        "state": c.state.as_str(),
        "terminalReason": c.terminal_reason.map(|r| r.as_str()),
        "failureCategory": c.failure_category.map(|f| f.as_str()),
        "requestedAt": ts(t.requested_at),
        "firstAttemptAt": ts_opt(first_attempt),
        "ringingAt": ts_opt(t.ringing_at),
        "answeredAt": ts_opt(t.answered_at),
        "connectedAt": ts_opt(t.connected_at),
        "endedAt": ts_opt(t.ended_at),
        "connectedDurationMs": detail.connected_duration_ms,
        "durationsAsOf": ts(detail.as_of),
        "routeClass": routes.last().map(|r| r.route_class.as_str()),
        "attemptCount": attempts.len(),
        "attempts": attempt_views,
        "participants": participants,
        "privacyDecisions": decisions(state, scope, c.id.as_uuid()).await?,
    }))
}

pub(crate) async fn session_evidence(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    raw: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::PRIVACY_EVIDENCE_READ)?;
    let Ok(session) = Uuid::parse_str(&id) else {
        return Ok(session_not_found());
    };
    let Query(pairs) = raw.map_err(|_| CallingError::Validation("The query string is invalid."))?;
    let (limit, cursor) = query(&pairs)?;
    // Ownership through Privacy Relay, in the caller's signed scope: knowing a session
    // id is never enough.
    let delegated = delegated_context(&ctx, None).map_err(|_| CallingError::Internal)?;
    let owned = state
        .privacy_relay
        .proxy(ProxyRequest {
            delegated,
            correlation_id: ctx.correlation_id().clone(),
            method: Method::GET,
            path: format!("/internal/v1/sessions/{session}"),
            body: None,
        })
        .await;
    let relay = match owned {
        Ok(r) if r.status == 200 => r.body,
        Ok(r) if r.status == 404 => return Ok(session_not_found()),
        Ok(_) | Err(PrivacyRelayError::NotFound | PrivacyRelayError::Unavailable) => {
            return Err(CallingError::ServiceUnavailable)
        }
    };
    if relay.get("relaySessionId").and_then(Value::as_str) != Some(session.to_string().as_str()) {
        return Err(CallingError::ServiceUnavailable);
    }
    // Calling's OWN records of that session.
    let scope = CallScope::of_platform(&ctx);
    let page = state
        .calls
        .list_calls(
            scope,
            ListCallsQuery {
                limit,
                cursor,
                privacy_relay_ref: Some(session),
                ..Default::default()
            },
        )
        .await?;
    let mut calls = Vec::with_capacity(page.calls.len());
    for call in &page.calls {
        calls.push(call_evidence(&state, scope, call.id).await?);
    }
    let mut response = Json(json!({
        "relaySessionId": session,
        "contextId": relay.get("contextId").cloned().unwrap_or(Value::Null),
        "calls": calls,
        "nextCursor": page.next_cursor.map(|c| c.encode()),
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
