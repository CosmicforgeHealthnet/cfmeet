//! Messaging reads (design §6.3, MSG-017), served from Messaging's operator reads
//! (`/internal/v1/operator/*`, `messaging-firstparty.administer`, bridge only). Every call
//! carries `X-CF-Operator: hq:<sub>`.
//!
//! Privacy: Messaging holds end-to-end encrypted content. Ids, categories, states, counts
//! and times only: never message content, ciphertext, MLS state, keys, attachments, report
//! evidence (sealed or referenced), audit details, outbox payloads, export object keys,
//! legal case references, free-text summaries or error strings. The owner never sends
//! them; this translation also relays only the fields below, so a future owner field is
//! dropped until reviewed.

use serde_json::{json, Value};

use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::Messaging;
const BASE: &str = "/internal/v1/operator";

fn listing(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "next_cursor": next})
}

fn uuids(v: &Value, k: &str) -> T<Value> {
    let arr = v
        .get(k)
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if arr.len() > 20 {
        return Err(OwnerError::Contract);
    }
    arr.iter()
        .map(|id| {
            id.as_str()
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .map(|u| Value::String(u.hyphenated().to_string()))
                .ok_or(OwnerError::Contract)
        })
        .collect::<T<Vec<_>>>()
        .map(Value::Array)
}

fn scope(v: &Value) -> T<Vec<(&'static str, Value)>> {
    Ok(vec![
        ("tenant_id", t::uuid(v, "tenantId")?),
        ("application_id", t::uuid(v, "applicationId")?),
        ("environment_id", t::uuid(v, "environmentId")?),
    ])
}

fn report_fields(r: &Value) -> T<Vec<(&'static str, Value)>> {
    let mut f = vec![("report_id", t::uuid(r, "id")?)];
    f.extend(scope(r)?);
    f.extend([
        ("conversation_id", t::uuid(r, "conversationId")?),
        ("security_profile", t::string(r, "securityProfile")?),
        ("reporter_principal_id", t::uuid(r, "reporterPrincipalId")?),
        (
            "reported_principal_id",
            t::opt_uuid(r, "reportedPrincipalId")?,
        ),
        ("message_ids", uuids(r, "messageIds")?),
        ("category", t::string(r, "category")?),
        ("evidence_kind", t::string(r, "evidenceKind")?),
        ("status", t::string(r, "status")?),
        ("resolution", t::opt_string(r, "resolution")?),
        ("resolved_at", t::opt_time(r, "resolvedAt")?),
        ("created_at", t::time(r, "createdAt")?),
    ]);
    Ok(f)
}

/// One report: ids, category, states and times. The evidence itself is never relayed;
/// `evidence_kind` only says which kind the reporter gave.
pub fn report(r: &Value) -> T<Value> {
    Ok(obj(report_fields(r)?))
}

fn action(a: &Value) -> T<Value> {
    Ok(obj(vec![
        ("action_id", t::uuid(a, "id")?),
        ("kind", t::string(a, "kind")?),
        ("reason", t::string(a, "reason")?),
        ("conversation_id", t::opt_uuid(a, "conversationId")?),
        ("message_id", t::opt_uuid(a, "messageId")?),
        ("principal_id", t::opt_uuid(a, "principalId")?),
        ("effective_until", t::opt_time(a, "effectiveUntil")?),
        ("created_at", t::time(a, "createdAt")?),
    ]))
}

pub fn report_detail(r: &Value) -> T<Value> {
    let mut f = report_fields(r)?;
    let actions = r
        .get("actions")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if actions.len() > 100 {
        return Err(OwnerError::Contract);
    }
    f.push((
        "actions",
        Value::Array(actions.iter().map(action).collect::<T<Vec<_>>>()?),
    ));
    Ok(obj(f))
}

pub fn hold(h: &Value) -> T<Value> {
    let mut f = vec![("hold_id", t::uuid(h, "id")?)];
    f.extend(scope(h)?);
    f.extend([
        ("scope_kind", t::string(h, "scopeKind")?),
        ("conversation_id", t::opt_uuid(h, "conversationId")?),
        ("principal_id", t::opt_uuid(h, "principalId")?),
        ("reason_category", t::string(h, "reasonCategory")?),
        ("status", t::string(h, "status")?),
        ("created_at", t::time(h, "createdAt")?),
        ("expires_at", t::opt_time(h, "expiresAt")?),
        ("released_at", t::opt_time(h, "releasedAt")?),
        ("release_reason", t::opt_string(h, "releaseReason")?),
    ]);
    Ok(obj(f))
}

pub fn export(e: &Value) -> T<Value> {
    let mut f = vec![("export_id", t::uuid(e, "id")?)];
    f.extend(scope(e)?);
    f.extend([
        ("conversation_id", t::uuid(e, "conversationId")?),
        ("status", t::string(e, "status")?),
        ("size_bytes", t::opt_int(e, "size")?),
        ("message_count", t::opt_int(e, "messageCount")?),
        ("failure", t::opt_string(e, "failure")?),
        ("attempts", t::int(e, "attempts")?),
        ("created_at", t::time(e, "createdAt")?),
        ("completed_at", t::opt_time(e, "completedAt")?),
        ("expires_at", t::time(e, "expiresAt")?),
    ]);
    Ok(obj(f))
}

pub fn deletion_request(d: &Value) -> T<Value> {
    let mut f = vec![("request_id", t::uuid(d, "id")?)];
    f.extend(scope(d)?);
    f.extend([
        ("kind", t::string(d, "kind")?),
        ("principal_id", t::opt_uuid(d, "principalId")?),
        ("conversation_id", t::opt_uuid(d, "conversationId")?),
        ("status", t::string(d, "status")?),
        ("messages_purged", t::int(d, "messagesPurged")?),
        ("attachments_deleted", t::int(d, "attachmentsDeleted")?),
        ("held_items", t::int(d, "heldItems")?),
        ("created_at", t::time(d, "createdAt")?),
        ("completed_at", t::opt_time(d, "completedAt")?),
    ]);
    Ok(obj(f))
}

pub fn audit_entry(a: &Value) -> T<Value> {
    let mut f = vec![
        ("entry_id", t::uuid(a, "id")?),
        ("occurred_at", t::time(a, "occurredAt")?),
    ];
    f.extend(scope(a)?);
    f.extend([
        ("credential_id", t::opt_uuid(a, "credentialId")?),
        ("principal_id", t::opt_uuid(a, "principalId")?),
        ("action", t::string(a, "action")?),
        ("target_kind", t::string(a, "targetKind")?),
        ("target_id", t::uuid(a, "targetId")?),
        ("correlation_id", t::string(a, "correlationId")?),
    ]);
    Ok(obj(f))
}

pub fn switch(s: &Value) -> T<Value> {
    Ok(obj(vec![
        ("switch_name", t::string(s, "switchName")?),
        ("scope_key", t::string(s, "scopeKey")?),
        ("engaged", t::boolean(s, "engaged")?),
        ("reason", t::string(s, "reason")?),
        ("changed_by", t::string(s, "changedBy")?),
        ("changed_at", t::time(s, "changedAt")?),
        ("version", t::int(s, "version")?),
    ]))
}

pub fn incident(i: &Value) -> T<Value> {
    Ok(obj(vec![
        ("incident_id", t::uuid(i, "id")?),
        ("category", t::string(i, "category")?),
        ("severity", t::string(i, "severity")?),
        ("status", t::string(i, "status")?),
        ("occurrences", t::int(i, "occurrences")?),
        ("first_seen_at", t::time(i, "firstSeenAt")?),
        ("last_seen_at", t::time(i, "lastSeenAt")?),
        ("resolved_at", t::opt_time(i, "resolvedAt")?),
        ("resolved_by", t::opt_string(i, "resolvedBy")?),
    ]))
}

pub fn failed_message(m: &Value) -> T<Value> {
    Ok(obj(vec![
        ("event_id", t::uuid(m, "id")?),
        ("sequence", t::int(m, "sequence")?),
        ("event_type", t::string(m, "eventType")?),
        ("aggregate_type", t::string(m, "aggregateType")?),
        ("aggregate_id", t::uuid(m, "aggregateId")?),
        ("tenant_id", t::uuid(m, "tenantId")?),
        ("environment_id", t::uuid(m, "environmentId")?),
        ("attempts", t::int(m, "attempts")?),
        ("occurred_at", t::time(m, "occurredAt")?),
    ]))
}

fn root_hash(v: &Value) -> T<Value> {
    let h = v
        .get("rootHash")
        .and_then(Value::as_str)
        .ok_or(OwnerError::Contract)?;
    (h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| Value::String(h.to_string()))
    .ok_or(OwnerError::Contract)
}

/// Tree head and witness state only: sizes, root hashes and times. No key material.
pub fn key_transparency(k: &Value) -> T<Value> {
    let head = match k.get("head") {
        None | Some(Value::Null) => Value::Null,
        Some(h) if h.is_object() => obj(vec![
            ("tree_size", t::int(h, "treeSize")?),
            ("root_hash", root_hash(h)?),
            ("updated_at", t::time(h, "updatedAt")?),
        ]),
        _ => return Err(OwnerError::Contract),
    };
    let witnesses = k
        .get("witnesses")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if witnesses.len() > 32 {
        return Err(OwnerError::Contract);
    }
    let witnesses = witnesses
        .iter()
        .map(|w| {
            Ok(obj(vec![
                ("witness_id", t::string(w, "witnessId")?),
                ("tree_size", t::int(w, "treeSize")?),
                ("root_hash", root_hash(w)?),
                ("updated_at", t::time(w, "updatedAt")?),
                ("last_cosigned_size", t::opt_int(w, "lastCosignedSize")?),
            ]))
        })
        .collect::<T<Vec<_>>>()?;
    Ok(obj(vec![
        ("head", head),
        ("witnesses", Value::Array(witnesses)),
    ]))
}

/// `<micros>.<uuid>`: Messaging's keyset cursor (checked here so a bad one never reaches it).
fn time_cursor(auth: &Authorized) -> Result<Option<String>, ErrorCode> {
    match &auth.query.cursor {
        None => Ok(None),
        Some(c) => {
            let (t, id) = c.split_once('.').ok_or(ErrorCode::QueryInvalid)?;
            let ok = !t.is_empty()
                && t.len() <= 20
                && t.bytes().all(|b| b.is_ascii_digit())
                && uuid::Uuid::parse_str(id).is_ok_and(|u| u.hyphenated().to_string() == id);
            ok.then(|| Some(c.clone())).ok_or(ErrorCode::QueryInvalid)
        }
    }
}

fn rfc(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    let operator = format!("hq:{}", auth.staff.claims.sub);
    let q = &auth.query;
    let limit = q.limit.map(|l| ("limit", l.to_string()));
    let read = |path: String, query: Vec<(&'static str, String)>| {
        let operator = operator.clone();
        async move { bridge.owners.read(O, &path, &query, &operator).await }
    };
    let answer = |r: T<Value>| -> Result<Answer, ErrorCode> {
        r.map(|result| Answer {
            result,
            freshness: "LIVE",
        })
        .map_err(ErrorCode::from)
    };
    let paged = |window: bool| -> Result<Vec<(&'static str, String)>, ErrorCode> {
        let cursor = time_cursor(auth)?;
        let mut query: Vec<(&'static str, String)> = Vec::new();
        if window {
            query.extend(q.start.map(|s| ("since", rfc(s))));
            query.extend(q.end.map(|e| ("until", rfc(e))));
        }
        query.extend(limit.clone());
        query.extend(cursor.map(|c| ("cursor", c)));
        Ok(query)
    };
    let list = |path: &'static str, window: bool, item: fn(&Value) -> T<Value>| {
        let query = paged(window);
        async move {
            let query = query?;
            match read(format!("{BASE}/{path}"), query).await {
                Ok(v) => answer(t::page(&v, item).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
    };
    Some(match auth.capability.id {
        "messaging.reports.list" => list("reports", false, report).await,
        "messaging.reports.read" => {
            let Some(id) = auth.params.get("report_id").cloned() else {
                return Some(Err(ErrorCode::Internal));
            };
            // Messaging's report ids are UUIDs: anything else cannot exist.
            if !uuid::Uuid::parse_str(&id).is_ok_and(|u| u.hyphenated().to_string() == id) {
                return Some(Err(ErrorCode::ResourceNotFound));
            }
            match read(format!("{BASE}/reports/{id}"), vec![]).await {
                Ok(v) => answer(report_detail(&v)),
                Err(e) => Err(e.into()),
            }
        }
        "messaging.holds.list" => list("holds", false, hold).await,
        "messaging.exports.list" => list("exports", false, export).await,
        "messaging.deletion-requests.list" => {
            list("deletion-requests", false, deletion_request).await
        }
        "messaging.audit.list" => list("audit", true, audit_entry).await,
        "messaging.switches.list" => match read(format!("{BASE}/switches"), vec![]).await {
            Ok(v) => answer(t::page(&v, switch).map(|(i, n)| listing(i, n))),
            Err(e) => Err(e.into()),
        },
        "messaging.incidents.list" => list("incidents", true, incident).await,
        "messaging.failed-messages.list" => list("failed-messages", true, failed_message).await,
        "messaging.key-transparency.read" => {
            match read(format!("{BASE}/key-transparency"), vec![]).await {
                Ok(v) => answer(key_transparency(&v)),
                Err(e) => Err(e.into()),
            }
        }
        _ => return None,
    })
}
