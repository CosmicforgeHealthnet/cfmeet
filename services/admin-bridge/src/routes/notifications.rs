//! Notifications reads (design §6.13), served from the Notification Service.
//!
//! Operator reads (`notifications.administer`, `/internal/v1/operator/*`): deliveries,
//! failed deliveries, suppression lookup, provider status. Existing observability read
//! (`notifications-observability.read`): routing. Every call carries
//! `X-CF-Operator: hq:<sub>`. No address, content or credential ever reaches HQ:
//! suppressions are found by address or digest, but only the owner's keyed digest is
//! relayed.

use serde_json::{json, Value};

use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::Notifications;

fn rfc(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Owner delivery cursor `<micros>.<uuid>`; anything else is the caller's error.
fn delivery_cursor(auth: &Authorized) -> Result<Option<String>, ErrorCode> {
    match &auth.query.cursor {
        None => Ok(None),
        Some(c) => {
            let ok = c.split_once('.').is_some_and(|(m, id)| {
                !m.is_empty()
                    && m.len() <= 20
                    && m.bytes().all(|b| b.is_ascii_digit())
                    && uuid::Uuid::parse_str(id).is_ok()
            });
            if ok {
                Ok(Some(c.clone()))
            } else {
                Err(ErrorCode::QueryInvalid)
            }
        }
    }
}

pub fn delivery_item(d: &Value) -> T<Value> {
    Ok(obj(vec![
        ("delivery_id", t::uuid(d, "deliveryId")?),
        ("notification_id", t::uuid(d, "notificationId")?),
        ("category", t::string(d, "category")?),
        ("lane", t::string(d, "lane")?),
        ("channel", t::string(d, "channel")?),
        ("status", t::string(d, "status")?),
        ("attempts", t::int(d, "attempts")?),
        ("last_error_class", t::opt_string(d, "lastErrorClass")?),
        ("created_at", t::time(d, "createdAt")?),
        ("completed_at", t::opt_time(d, "completedAt")?),
    ]))
}

pub fn suppression_item(s: &Value) -> T<Value> {
    Ok(obj(vec![
        ("recipient_digest", t::string(s, "digest")?),
        ("channel", json!("EMAIL")),
        ("reason", t::string(s, "reason")?),
        ("created_at", t::time(s, "createdAt")?),
        ("version", t::int(s, "version")?),
    ]))
}

fn routing(v: &Value) -> T<Value> {
    let rows = v
        .get("routing")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    let audit = v
        .get("audit")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    let protocols = v
        .get("routingProtocol")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if rows.len() > 200 || audit.len() > 2000 || protocols.len() > 16 {
        return Err(OwnerError::Contract);
    }
    let routes = rows
        .iter()
        .map(|r| {
            Ok(obj(vec![
                ("category", t::string(r, "category")?),
                ("channel", t::string(r, "channel")?),
                ("mode", t::string(r, "mode")?),
                ("version", t::int(r, "version")?),
                ("canary_percent", t::int(r, "canaryPercent")?),
                ("fence_at", t::time(r, "fenceAt")?),
            ]))
        })
        .collect::<T<Vec<_>>>()?;
    let hours = audit
        .iter()
        .map(|a| {
            Ok(obj(vec![
                ("producer", t::string(a, "producer")?),
                ("category", t::string(a, "category")?),
                ("hour", t::time(a, "hour")?),
                ("accepted_decisions", t::int(a, "acceptedDecisions")?),
            ]))
        })
        .collect::<T<Vec<_>>>()?;
    let protocols = protocols
        .iter()
        .map(|p| p.as_i64().map(Value::from).ok_or(OwnerError::Contract))
        .collect::<T<Vec<_>>>()?;
    Ok(json!({"routing_protocols": protocols, "routes": routes, "audit": hours}))
}

fn providers(v: &Value) -> T<Value> {
    let list = v
        .get("providers")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if list.len() > 8 {
        return Err(OwnerError::Contract);
    }
    let items = list
        .iter()
        .map(|p| {
            Ok(obj(vec![
                ("channel", t::string(p, "channel")?),
                ("configured", t::boolean(p, "configured")?),
                ("status", t::string(p, "status")?),
                ("last_success_at", t::opt_time(p, "lastSuccessAt")?),
                ("last_failure_at", t::opt_time(p, "lastFailureAt")?),
                ("last_error_class", t::opt_string(p, "lastErrorClass")?),
                ("recent_deliveries", t::int(p, "recentDeliveries")?),
            ]))
        })
        .collect::<T<Vec<_>>>()?;
    Ok(json!({"checked_at": t::time(v, "checkedAt")?, "providers": items}))
}

fn listing(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "next_cursor": next})
}

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    let op = format!("hq:{}", auth.staff.claims.sub);
    let q = &auth.query;
    let read = |path: &'static str, query: Vec<(&'static str, String)>| {
        let op = op.clone();
        async move { bridge.owners.read(O, path, &query, &op).await }
    };
    let answer = |r: T<Value>| -> Result<Answer, ErrorCode> {
        r.map(|result| Answer {
            result,
            freshness: "LIVE",
        })
        .map_err(ErrorCode::from)
    };
    let window = |cursor: Option<String>| {
        let mut query: Vec<(&'static str, String)> = Vec::new();
        query.extend(q.start.map(|s| ("since", rfc(s))));
        query.extend(q.end.map(|e| ("until", rfc(e))));
        query.extend(cursor.map(|c| ("cursor", c)));
        query.extend(q.limit.map(|l| ("limit", l.to_string())));
        query
    };
    Some(match auth.capability.id {
        "notifications.deliveries.list" | "notifications.failed-deliveries.list" => {
            let cursor = match delivery_cursor(auth) {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };
            let path = if auth.capability.id == "notifications.deliveries.list" {
                "/internal/v1/operator/deliveries"
            } else {
                "/internal/v1/operator/failed-deliveries"
            };
            match read(path, window(cursor)).await {
                Ok(v) => answer(t::page(&v, delivery_item).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
        "notifications.suppressions.list" => {
            // q is an address (digested by the owner, never stored here) or a 64-hex digest.
            let term = q.q.clone().unwrap_or_default();
            let query = if term.contains('@') {
                vec![("address", term)]
            } else if term.len() == 64 && term.bytes().all(|b| b.is_ascii_hexdigit()) {
                vec![("digest", term.to_ascii_lowercase())]
            } else {
                return Some(Err(ErrorCode::QueryInvalid));
            };
            match read("/internal/v1/operator/email-suppressions", query).await {
                Ok(v) => {
                    answer(t::page(&v, suppression_item).map(|(i, _)| listing(i, Value::Null)))
                }
                Err(e) => Err(e.into()),
            }
        }
        "notifications.routing.read" => match read("/internal/v1/admin/routing", vec![]).await {
            Ok(v) => answer(routing(&v)),
            Err(e) => Err(e.into()),
        },
        "notifications.providers.read" => {
            match read("/internal/v1/operator/providers", vec![]).await {
                Ok(v) => answer(providers(&v)),
                Err(e) => Err(e.into()),
            }
        }
        _ => return None,
    })
}
