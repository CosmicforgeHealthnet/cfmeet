//! Cross-service operations (design §6.15, A8): incidents and switches across the owners,
//! in one closed shape each.
//!
//! * `operations.incidents.list` — Messaging's platform incidents, Calling's incidents of
//!   the request's tenant and the Developer Platform's partner incident records, each with
//!   the window, one owner page per bridge page (up to `limit` items per owner; the cursor
//!   carries each owner's own cursor). An owner that does not answer leaves the page
//!   PARTIAL; when none answers, OWNER_UNAVAILABLE. Ids name their owner:
//!   `<owner>:<uuid>`.
//! * `operations.incident.resolve` — routed by that id to the owning service's resolve
//!   command through the command core; the ledger target is the owner's incident
//!   (`<owner>:incident:<uuid>`), never the generic capability. A partner incident record is
//!   immutable: there is nothing to resolve (RESOURCE_NOT_FOUND).
//! * `operations.switches.list` — Messaging's kill switches and Notifications' routing
//!   modes, one bounded page; switch ids `messaging:<NAME>[:<scope>]` and
//!   `notifications:<CATEGORY>:<CHANNEL>`.
//! * `operations.switch.change` — routed by that id to `messaging.switch.change` or
//!   `notifications.routing.change` semantics through the command core; the target is the
//!   owner's switch at its scope.

use serde_json::{json, Value};

use super::commands::{self, Applied, CommandSpec};
use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::contract::owner_commands::command;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const INCIDENTS: &str = "/internal/v1/operator/incidents";
/// Most items per owner page (the owners' own maximum).
const OWNER_PAGE: u32 = 100;
const NOTIFICATION_CHANNELS: &[&str] = &["EMAIL", "PUSH", "IN_APP", "OPERATOR"];
const ROUTING_MODES: &[&str] = &["OFF", "SHADOW", "ACTIVE"];

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    Some(match auth.capability.id {
        "operations.incidents.list" => incidents(bridge, auth).await,
        "operations.incident.resolve" => resolve(bridge, auth).await,
        "operations.switches.list" => switches(bridge, auth).await,
        "operations.switch.change" => change_switch(bridge, auth).await,
        _ => return None,
    })
}

fn rfc(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn operator(auth: &Authorized) -> String {
    format!("hq:{}", auth.staff.claims.sub)
}

fn canonical_uuid(raw: &str) -> Option<String> {
    uuid::Uuid::parse_str(raw)
        .ok()
        .map(|u| u.hyphenated().to_string())
}

fn code_ok(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.as_bytes()[0].is_ascii_uppercase()
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

// ---- Incidents ------------------------------------------------------------------------

/// The incident sources in page order: (owner, cursor tag).
const INCIDENT_SOURCES: &[(Owner, &str)] = &[
    (Owner::Messaging, "m"),
    (Owner::Calling, "c"),
    (Owner::DeveloperPlatform, "p"),
];

fn owner_cursor_ok(owner: Owner, c: &str) -> bool {
    match owner {
        // `<micros>.<uuid>`
        Owner::Messaging => c.split_once('.').is_some_and(|(m, id)| {
            !m.is_empty()
                && m.len() <= 20
                && m.bytes().all(|b| b.is_ascii_digit())
                && canonical_uuid(id).as_deref() == Some(id)
        }),
        // `<micros>_<32 hex>`
        Owner::Calling => c.split_once('_').is_some_and(|(m, id)| {
            !m.is_empty()
                && m.len() <= 20
                && m.bytes().all(|b| b.is_ascii_digit())
                && id.len() == 32
                && id.bytes().all(|b| b.is_ascii_hexdigit())
        }),
        _ => {
            !c.is_empty()
                && c.len() <= 128
                && c.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:@+=-".contains(&b))
        }
    }
}

/// The bridge cursor `<tag>.<owner cursor>[~...]`: the owners still to page and where.
/// `None` (first page): every owner from its start.
fn split_cursor(c: Option<&str>) -> Result<Vec<(Owner, Option<String>)>, ErrorCode> {
    let Some(c) = c else {
        return Ok(INCIDENT_SOURCES.iter().map(|(o, _)| (*o, None)).collect());
    };
    let mut out = Vec::new();
    for part in c.split('~') {
        let (tag, cursor) = part.split_once('.').ok_or(ErrorCode::QueryInvalid)?;
        let (owner, _) = INCIDENT_SOURCES
            .iter()
            .find(|(_, t)| *t == tag)
            .ok_or(ErrorCode::QueryInvalid)?;
        if out.iter().any(|(o, _)| o == owner) || !owner_cursor_ok(*owner, cursor) {
            return Err(ErrorCode::QueryInvalid);
        }
        out.push((*owner, Some(cursor.to_string())));
    }
    Ok(out)
}

fn incident_item(owner: Owner, i: &Value) -> T<Value> {
    let id = |k: &str| -> T<Value> {
        let v = t::uuid(i, k)?;
        Ok(Value::String(format!(
            "{}:{}",
            owner.as_str(),
            v.as_str().ok_or(OwnerError::Contract)?
        )))
    };
    Ok(match owner {
        Owner::Messaging | Owner::Calling => {
            let x = if owner == Owner::Messaging {
                super::messaging::incident(i)?
            } else {
                super::calling::incident(i)?
            };
            obj(vec![
                (
                    "incident_id",
                    id(if owner == Owner::Messaging {
                        "id"
                    } else {
                        "incidentId"
                    })?,
                ),
                ("owner", Value::from(owner.as_str())),
                ("category", x["category"].clone()),
                ("severity", x["severity"].clone()),
                ("status", x["status"].clone()),
                ("occurrences", x["occurrences"].clone()),
                ("opened_at", x["first_seen_at"].clone()),
                ("last_seen_at", x["last_seen_at"].clone()),
                ("resolved_at", x["resolved_at"].clone()),
                ("resolvable", Value::Bool(x["status"] != "RESOLVED")),
            ])
        }
        _ => {
            let x = super::partners::incident(i)?;
            let opened = if x["occurred_at"].is_null() {
                x["recorded_at"].clone()
            } else {
                x["occurred_at"].clone()
            };
            obj(vec![
                ("incident_id", id("incidentRecordId")?),
                ("owner", Value::from(owner.as_str())),
                ("category", Value::Null),
                ("severity", Value::Null),
                // A partner incident is an immutable support record.
                ("status", Value::from("RECORDED")),
                ("occurrences", Value::Null),
                ("opened_at", opened),
                ("last_seen_at", Value::Null),
                ("resolved_at", Value::Null),
                ("resolvable", Value::Bool(false)),
            ])
        }
    })
}

async fn incidents(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let q = &auth.query;
    let window = [
        ("since", rfc(q.start.ok_or(ErrorCode::QueryInvalid)?)),
        ("until", rfc(q.end.ok_or(ErrorCode::QueryInvalid)?)),
    ];
    let limit = q.limit.unwrap_or(50).min(OWNER_PAGE);
    let plan = split_cursor(q.cursor.as_deref())?;
    let tenant = canonical_uuid(&q.tenant);
    let operator = operator(auth);
    let (mut items, mut next, mut answered, mut failed) = (Vec::new(), Vec::new(), 0, 0);
    for (owner, cursor) in plan {
        let mut query: Vec<(&str, String)> = Vec::new();
        if owner == Owner::Calling {
            // Calling's incidents belong to a Platform Access tenant: a scope that is not
            // one has none.
            match &tenant {
                Some(t) => query.push(("tenantId", t.clone())),
                None => continue,
            }
        }
        query.extend(window.iter().cloned());
        query.push(("limit", limit.to_string()));
        query.extend(cursor.map(|c| ("cursor", c)));
        let page = match bridge
            .owners
            .read(owner, INCIDENTS, &query, &operator)
            .await
        {
            Ok(v) => t::page(&v, |i| incident_item(owner, i)),
            Err(e) => Err(e),
        };
        match page {
            Ok((list, n)) => {
                answered += 1;
                items.extend(list);
                if let Some(c) = n.as_str() {
                    if !owner_cursor_ok(owner, c) {
                        return Err(ErrorCode::Internal);
                    }
                    let tag = INCIDENT_SOURCES
                        .iter()
                        .find(|(o, _)| *o == owner)
                        .map(|(_, t)| *t)
                        .unwrap_or_default();
                    next.push(format!("{tag}.{c}"));
                }
            }
            Err(e) => {
                tracing::warn!(owner = owner.as_str(), error = %e, "incident list unavailable");
                failed += 1;
            }
        }
    }
    if answered == 0 && failed > 0 {
        return Err(ErrorCode::OwnerUnavailable);
    }
    // Newest first across owners.
    items.sort_by(|a, b| {
        b["opened_at"]
            .as_str()
            .cmp(&a["opened_at"].as_str())
            .then_with(|| a["incident_id"].as_str().cmp(&b["incident_id"].as_str()))
    });
    let next_cursor = (!next.is_empty()).then(|| next.join("~"));
    Ok(Answer {
        result: json!({"items": items, "next_cursor": next_cursor}),
        freshness: if failed > 0 { "PARTIAL" } else { "LIVE" },
    })
}

async fn resolve(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let raw = auth
        .params
        .get("incident_id")
        .ok_or(ErrorCode::Internal)?
        .clone();
    let (base, _) = commands::parse_command_body(&auth.body, &[], false)?;
    let reason = base
        .reason_code
        .clone()
        .ok_or(ErrorCode::CommandBodyInvalid)?;
    let (tag, id) = raw.split_once(':').ok_or(ErrorCode::ResourceNotFound)?;
    let id = canonical_uuid(id).ok_or(ErrorCode::ResourceNotFound)?;
    let (owner, row, body, target) = match tag {
        "messaging" => (
            Owner::Messaging,
            "messaging.incident.record",
            json!({"reasonCode": reason}),
            format!("messaging:incident:{id}"),
        ),
        "calling" => {
            let tenant = canonical_uuid(&auth.query.tenant).ok_or(ErrorCode::ResourceNotFound)?;
            (
                Owner::Calling,
                "calling.incident.record",
                json!({"tenantId": tenant}),
                format!("calling:incident:{tenant}:{id}"),
            )
        }
        // A partner incident record is immutable; any other owner names nothing.
        _ => return Err(ErrorCode::ResourceNotFound),
    };
    let spec = command(row).ok_or(ErrorCode::Internal)?;
    let incident_id = format!("{tag}:{id}");
    commands::execute(
        bridge,
        auth,
        CommandSpec {
            owner,
            owner_path: format!("{INCIDENTS}/{id}/resolve"),
            owner_body: body,
            // Unversioned: every resolve is its own slot.
            target: format!("{target}:{}", base.operation_id),
            expected_version: "0".into(),
            operation_id: base.operation_id,
        },
        move |v: &Value| {
            let a = super::owner_commands::applied(spec, v)?;
            Some(Applied {
                view: json!({
                    "incident_id": incident_id,
                    "owner": owner.as_str(),
                    "incident_status": a.view["incident_status"],
                    "resolved_at": a.view["resolved_at"],
                }),
                ..a
            })
        },
    )
    .await
}

// ---- Switches -------------------------------------------------------------------------

fn messaging_switch(s: &Value) -> T<Value> {
    let x = super::messaging::switch(s)?;
    let name = x["switch_name"].as_str().ok_or(OwnerError::Contract)?;
    let scope = x["scope_key"].as_str().ok_or(OwnerError::Contract)?;
    let id = if scope == "GLOBAL" {
        format!("messaging:{name}")
    } else {
        format!("messaging:{name}:{scope}")
    };
    Ok(obj(vec![
        ("switch_id", Value::from(id)),
        ("owner", Value::from("messaging")),
        ("name", x["switch_name"].clone()),
        ("scope", x["scope_key"].clone()),
        (
            "state",
            Value::from(if x["engaged"] == true {
                "ENGAGED"
            } else {
                "RELEASED"
            }),
        ),
        ("version", x["version"].clone()),
        ("changed_at", x["changed_at"].clone()),
    ]))
}

fn notification_route(r: &Value) -> T<Value> {
    let category = t::string(r, "category")?;
    let channel = t::string(r, "channel")?;
    let id = format!(
        "notifications:{}:{}",
        category.as_str().ok_or(OwnerError::Contract)?,
        channel.as_str().ok_or(OwnerError::Contract)?
    );
    Ok(obj(vec![
        ("switch_id", Value::from(id)),
        ("owner", Value::from("notifications")),
        ("name", category),
        ("scope", channel),
        ("state", t::string(r, "mode")?),
        ("version", t::int(r, "version")?),
        ("changed_at", Value::Null),
    ]))
}

async fn switches(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    // One bounded page: neither owner pages these lists.
    if auth.query.cursor.is_some() {
        return Err(ErrorCode::QueryInvalid);
    }
    let operator = operator(auth);
    let (mut items, mut failed) = (Vec::new(), 0);
    let m = bridge
        .owners
        .read(
            Owner::Messaging,
            "/internal/v1/operator/switches",
            &[],
            &operator,
        )
        .await
        .and_then(|v| t::page(&v, messaging_switch).map(|(i, _)| i));
    match m {
        Ok(list) => items.extend(list),
        Err(e) => {
            tracing::warn!(error = %e, "messaging switches unavailable");
            failed += 1;
        }
    }
    let n = bridge
        .owners
        .read(
            Owner::Notifications,
            "/internal/v1/admin/routing",
            &[],
            &operator,
        )
        .await
        .and_then(|v| {
            let rows = v
                .get("routing")
                .and_then(Value::as_array)
                .filter(|r| r.len() <= 200)
                .ok_or(OwnerError::Contract)?;
            rows.iter().map(notification_route).collect::<T<Vec<_>>>()
        });
    match n {
        Ok(list) => items.extend(list),
        Err(e) => {
            tracing::warn!(error = %e, "notification routing unavailable");
            failed += 1;
        }
    }
    if failed == 2 {
        return Err(ErrorCode::OwnerUnavailable);
    }
    Ok(Answer {
        result: json!({"items": items, "next_cursor": null}),
        freshness: if failed > 0 { "PARTIAL" } else { "LIVE" },
    })
}

async fn change_switch(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let raw = auth
        .params
        .get("switch_id")
        .ok_or(ErrorCode::Internal)?
        .clone();
    let bad = ErrorCode::CommandBodyInvalid;
    let (base, body) = commands::parse_command_body(&auth.body, &["engaged", "mode"], true)?;
    let reason = base.reason_code.clone().ok_or(bad)?;
    let version: i64 = base.expected_version.parse().map_err(|_| bad)?;
    let parts: Vec<&str> = raw.split(':').collect();
    let (owner, row, path, owner_body, target, min) = match parts.as_slice() {
        ["messaging", name, rest @ ..] if code_ok(name) && rest.len() <= 1 => {
            let env = match rest.first() {
                None => None,
                Some(e) => Some(canonical_uuid(e).ok_or(ErrorCode::ResourceNotFound)?),
            };
            let engaged = body.get("engaged").and_then(Value::as_bool).ok_or(bad)?;
            if body.get("mode").is_some() {
                return Err(bad);
            }
            (
                Owner::Messaging,
                "messaging.switch.change",
                format!("/internal/v1/operator/switches/{name}"),
                json!({"environmentId": env, "engaged": engaged, "expectedVersion": version, "reasonCode": reason}),
                format!(
                    "messaging:switch:{name}:{}",
                    env.as_deref().unwrap_or("GLOBAL")
                ),
                0,
            )
        }
        ["notifications", category, channel]
            if code_ok(category) && NOTIFICATION_CHANNELS.contains(channel) =>
        {
            let mode = body
                .get("mode")
                .and_then(Value::as_str)
                .filter(|m| ROUTING_MODES.contains(m))
                .ok_or(bad)?;
            if body.get("engaged").is_some() {
                return Err(bad);
            }
            (
                Owner::Notifications,
                "notifications.routing.change",
                format!("/internal/v1/operator/routing/{category}/{channel}/mode"),
                json!({"mode": mode, "expectedVersion": version, "reasonCode": reason}),
                format!("notifications:routing:{category}:{channel}"),
                1,
            )
        }
        _ => return Err(ErrorCode::ResourceNotFound),
    };
    if version < min {
        return Err(bad);
    }
    let spec = command(row).ok_or(ErrorCode::Internal)?;
    let switch_id = raw.clone();
    commands::execute(
        bridge,
        auth,
        CommandSpec {
            owner,
            owner_path: path,
            owner_body,
            target,
            expected_version: version.to_string(),
            operation_id: base.operation_id,
        },
        move |v: &Value| {
            let a = super::owner_commands::applied(spec, v)?;
            let version = a
                .source_version
                .as_deref()
                .and_then(|s| s.parse::<i64>().ok())
                .map_or(Value::Null, Value::from);
            Some(Applied {
                view: json!({
                    "switch_id": switch_id,
                    "owner": owner.as_str(),
                    "state": a.source_status,
                    "version": version,
                    "changed_at": a.view["changed_at"],
                }),
                ..a
            })
        },
    )
    .await
}
