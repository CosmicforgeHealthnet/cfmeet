//! Bridge-local capabilities (no owner call): the bridge's own audit search and the
//! operations health view.
//!
//! * `audit.entries.list` — the hash-chained audit of every HQ request the bridge answered
//!   (allowed or refused) in a window, oldest first: who (`hq:<sub>`), what capability,
//!   purpose, decision and status, the approval used. Never a body, a scope value beyond
//!   the four scope keys, or a request digest.
//! * `operations.health.read` — the aggregate health (`bridge.health.read`'s answer) for the
//!   operations purposes.
//! * Privacy requests (design §6.14, A7): one data-subject request tracked across the
//!   services holding the person's data; each service's outcome is recorded by staff after
//!   the owner action (two people), and the request completes when every service has one.
//!   The bridge keeps the kind, the opaque CF Meet account id and the outcomes only.

use chrono::SecondsFormat;
use serde_json::{json, Value};

use super::commands;
use crate::chain::{Answer, Authorized, Bridge};
use crate::errors::ErrorCode;
use crate::store::{LocalChange, PrivacyRequest, PRIVACY_SERVICES};

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    Some(match auth.capability.id {
        "audit.entries.list" => audit(bridge, auth).await,
        "operations.health.read" => Ok(super::health_answer(bridge).await),
        "privacy.requests.list" => privacy_list(bridge, auth).await,
        "privacy.requests.read" => privacy_read(bridge, auth).await,
        "privacy.request.register" => privacy_register(bridge, auth).await,
        "privacy.request.complete-service" => privacy_complete(bridge, auth).await,
        _ => return None,
    })
}

async fn audit(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let q = &auth.query;
    let (start, end) = (
        q.start.ok_or(ErrorCode::QueryInvalid)?,
        q.end.ok_or(ErrorCode::QueryInvalid)?,
    );
    let after = match &q.cursor {
        None => None,
        Some(c) => Some(
            c.parse::<i64>()
                .ok()
                .filter(|s| *s >= 0)
                .ok_or(ErrorCode::QueryInvalid)?,
        ),
    };
    let limit = q.limit.unwrap_or(50);
    let rows = bridge
        .store
        .audit_window(start, end, after, limit + 1)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?;
    let more = rows.len() > limit as usize;
    let items: Vec<Value> = rows
        .iter()
        .take(limit as usize)
        .map(|(d, r)| {
            json!({
                "sequence": r.sequence,
                "correlation_id": d.correlation_id.to_string(),
                "occurred_at": d.occurred_at.to_rfc3339_opts(SecondsFormat::Secs, true),
                "actor": d.actor,
                "capability_id": d.capability,
                "method": d.method,
                "purpose": d.purpose,
                "decision": d.decision,
                "http_status": d.http_status,
                "approval_id": d.approval_id,
                "approver": d.approver,
            })
        })
        .collect();
    let next = if more {
        items.last().map(|i| json!(i["sequence"].to_string()))
    } else {
        None
    };
    Ok(Answer {
        result: json!({"items": items, "next_cursor": next.unwrap_or(Value::Null)}),
        freshness: "LIVE",
    })
}

fn ts(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn privacy_json(p: &PrivacyRequest) -> Value {
    json!({
        "request_id": p.request_id.to_string(),
        "kind": p.kind,
        "subject_ref": p.subject_ref.to_string(),
        "state": p.state,
        "created_by": p.created_by,
        "created_at": ts(p.created_at),
        "updated_at": ts(p.updated_at),
        "completed_at": p.completed_at.map(ts),
        "version": p.version,
        "services": p.services.iter().map(|s| json!({
            "service": s.service,
            "outcome": s.outcome,
            "recorded_by": s.recorded_by,
            "recorded_at": s.recorded_at.map(ts),
        })).collect::<Vec<_>>(),
    })
}

/// `<created_at micros>.<request id>`.
fn privacy_cursor(c: &str) -> Option<(chrono::DateTime<chrono::Utc>, uuid::Uuid)> {
    let (m, id) = c.split_once('.')?;
    let t = chrono::DateTime::from_timestamp_micros(m.parse().ok()?)?;
    let id = uuid::Uuid::parse_str(id).ok()?;
    (id.hyphenated().to_string() == c.split_once('.')?.1).then_some((t, id))
}

async fn privacy_list(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let q = &auth.query;
    let after = match &q.cursor {
        None => None,
        Some(c) => Some(privacy_cursor(c).ok_or(ErrorCode::QueryInvalid)?),
    };
    let limit = q.limit.unwrap_or(50);
    let rows = bridge
        .store
        .privacy_requests(after, limit + 1)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?;
    let next = (rows.len() > limit as usize)
        .then(|| rows.get(limit as usize - 1))
        .flatten()
        .map(|p| format!("{}.{}", p.created_at.timestamp_micros(), p.request_id));
    let items: Vec<Value> = rows.iter().take(limit as usize).map(privacy_json).collect();
    Ok(Answer {
        result: json!({"items": items, "next_cursor": next}),
        freshness: "LIVE",
    })
}

fn request_id(auth: &Authorized) -> Result<uuid::Uuid, ErrorCode> {
    auth.params
        .get("request_id")
        .and_then(|r| uuid::Uuid::parse_str(r).ok())
        .ok_or(ErrorCode::ResourceNotFound)
}

async fn privacy_read(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let p = bridge
        .store
        .privacy_request(request_id(auth)?)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?
        .ok_or(ErrorCode::ResourceNotFound)?;
    Ok(Answer {
        result: privacy_json(&p),
        freshness: "LIVE",
    })
}

async fn privacy_answer(
    bridge: &Bridge,
    op: crate::store::Operation,
    id: uuid::Uuid,
) -> Result<Answer, ErrorCode> {
    let p = bridge
        .store
        .privacy_request(id)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?
        .ok_or(ErrorCode::Internal)?;
    Ok(Answer {
        result: json!({"operation": commands::readback_json(&op), "resource": privacy_json(&p)}),
        freshness: "LIVE",
    })
}

async fn privacy_register(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let (base, body) = commands::parse_body(&auth.body, &["kind", "subject_ref"])?;
    if base.expected_version != "0" {
        return Err(ErrorCode::CommandBodyInvalid);
    }
    let kind = body
        .get("kind")
        .and_then(Value::as_str)
        .filter(|k| ["ACCESS", "ERASURE", "CORRECTION", "RESTRICTION"].contains(k))
        .ok_or(ErrorCode::CommandBodyInvalid)?
        .to_string();
    let subject_ref = body
        .get("subject_ref")
        .and_then(Value::as_str)
        .and_then(|s| {
            uuid::Uuid::parse_str(s)
                .ok()
                .filter(|u| u.hyphenated().to_string() == s)
        })
        .ok_or(ErrorCode::CommandBodyInvalid)?;
    let id = base.operation_id;
    let op = commands::execute_local(
        bridge,
        auth,
        format!("admin-bridge:privacy-request:{id}"),
        base.expected_version,
        base.operation_id,
        LocalChange::PrivacyRegister { kind, subject_ref },
    )
    .await?;
    privacy_answer(bridge, op, id).await
}

async fn privacy_complete(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let id = request_id(auth)?;
    let service = auth
        .params
        .get("service")
        .filter(|s| PRIVACY_SERVICES.contains(&s.as_str()))
        .ok_or(ErrorCode::ResourceNotFound)?
        .clone();
    let (base, body) = commands::parse_body(&auth.body, &["outcome"])?;
    let outcome = body
        .get("outcome")
        .and_then(Value::as_str)
        .filter(|o| ["DONE", "NOT_HELD", "LEGAL_HOLD"].contains(o))
        .ok_or(ErrorCode::CommandBodyInvalid)?
        .to_string();
    let expected: i64 = base
        .expected_version
        .parse()
        .map_err(|_| ErrorCode::CommandBodyInvalid)?;
    if expected < 1 {
        return Err(ErrorCode::CommandBodyInvalid);
    }
    let op = commands::execute_local(
        bridge,
        auth,
        format!("admin-bridge:privacy-request:{id}"),
        base.expected_version,
        base.operation_id,
        LocalChange::PrivacyRecord {
            request_id: id,
            service,
            outcome,
            expected_version: expected,
        },
    )
    .await?;
    privacy_answer(bridge, op, id).await
}
