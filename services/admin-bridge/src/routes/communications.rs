//! Communications reads (design §6.5), served from the Communications Hub's operator reads
//! (`/internal/v1/communications/operator/*`, `communications.administer`, bridge only).
//! Every call carries `X-CF-Operator: hq:<sub>`. Profiles relay the CF Meet ID, display
//! name (an R1 detail read), activation state, operator hold and times: never a phone
//! number, email address or account reference. Reports are safety evidence: ids, reasons,
//! states and times only (they carry no free text). The audit drops free-form details.
//! Metrics use HQ's `Summary` shape (`cfmeet.admin-summary.v1`).

use serde_json::{json, Value};

use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::{Owner, PRODUCT};
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::Communications;
const BASE: &str = "/internal/v1/communications/operator";

/// The Summary contract version HQ's report code expects.
pub const SUMMARY_VERSION: &str = "cfmeet.admin-summary.v1";

/// The Hub's counters relayed as Summary cells (owner key -> cell key).
const METRIC_CELLS: &[(&str, &str)] = &[
    ("profiles", "profiles"),
    ("connections", "connections"),
    ("devicesActive", "devices_active"),
    ("outboxDead", "outbox_dead"),
    ("operatorHolds", "operator_holds"),
    ("reportsOpen", "reports_open"),
];

fn listing(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "next_cursor": next})
}

fn hold(h: &Value) -> T<Value> {
    match h {
        Value::Null => Ok(Value::Null),
        Value::Object(_) => Ok(obj(vec![
            ("reason", t::string(h, "reason")?),
            ("active", t::boolean(h, "active")?),
            ("updated_at", t::time(h, "updatedAt")?),
        ])),
        _ => Err(OwnerError::Contract),
    }
}

pub fn profile(p: &Value) -> T<Value> {
    Ok(obj(vec![
        ("cfmeet_id", t::string(p, "cfMeetId")?),
        ("display_name", t::string(p, "displayName")?),
        ("activation_state", t::string(p, "activationState")?),
        ("hold", hold(p.get("hold").unwrap_or(&Value::Null))?),
        ("created_at", t::time(p, "createdAt")?),
        ("updated_at", t::time(p, "updatedAt")?),
        ("suspended_at", t::opt_time(p, "suspendedAt")?),
        ("deactivated_at", t::opt_time(p, "deactivatedAt")?),
        ("version", t::int(p, "version")?),
    ]))
}

fn review(r: &Value) -> T<Value> {
    match r {
        Value::Null => Ok(Value::Null),
        Value::Object(_) => Ok(obj(vec![
            ("decision", t::string(r, "decision")?),
            ("reason", t::string(r, "reason")?),
            ("held", t::boolean(r, "held")?),
            ("reviewed_at", t::time(r, "reviewedAt")?),
        ])),
        _ => Err(OwnerError::Contract),
    }
}

pub fn report(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("report_id", t::uuid(r, "reportId")?),
        ("reason", t::string(r, "reason")?),
        ("reporter_cfmeet_id", t::opt_string(r, "reporterCfMeetId")?),
        ("target_cfmeet_id", t::opt_string(r, "targetCfMeetId")?),
        ("target_report_count", t::int(r, "targetReportCount")?),
        ("version", t::int(r, "version")?),
        ("review", review(r.get("review").unwrap_or(&Value::Null))?),
        ("created_at", t::time(r, "createdAt")?),
    ]))
}

pub fn hold_item(h: &Value) -> T<Value> {
    Ok(obj(vec![
        ("cfmeet_id", t::opt_string(h, "cfMeetId")?),
        ("reason", t::string(h, "reason")?),
        ("active", t::boolean(h, "active")?),
        ("version", t::int(h, "version")?),
        ("updated_at", t::time(h, "updatedAt")?),
    ]))
}

pub fn audit_item(e: &Value) -> T<Value> {
    Ok(obj(vec![
        ("sequence", t::int(e, "seq")?),
        ("operation", t::string(e, "operation")?),
        ("actor_profile_id", t::opt_uuid(e, "actorProfileId")?),
        ("resource_id", t::opt_uuid(e, "resourceId")?),
        ("occurred_at", t::time(e, "createdAt")?),
    ]))
}

/// HQ `Summary` of the Hub's counters, scoped to the request. Counts are relayed as
/// reported; a missing or mistyped counter fails the answer (never a guessed zero).
pub fn metrics_summary(m: &Value, auth: &Authorized) -> T<Value> {
    let q = &auth.query;
    let mut cells = Vec::with_capacity(METRIC_CELLS.len());
    for (owner_key, cell_key) in METRIC_CELLS {
        let value = m
            .get(*owner_key)
            .and_then(Value::as_i64)
            .filter(|n| *n >= 0)
            .ok_or(OwnerError::Contract)?;
        cells.push(json!({"key": cell_key, "value": value, "unit": "count", "currency": null, "suppression": "NONE"}));
    }
    Ok(json!({
        "contract_version": SUMMARY_VERSION,
        "scope": {
            "product": PRODUCT,
            "market": q.market,
            "organization": q.organization,
            "zone": q.zone,
            "tenant": q.tenant,
        },
        "as_of": t::time(m, "asOf")?,
        "coverage": "COMPLETE",
        "window": null,
        "cells": cells,
    }))
}

/// `<micros>.<uuid>`: the Hub's keyset cursor (checked here so a bad one never reaches it).
fn time_cursor(auth: &Authorized) -> Result<Option<String>, ErrorCode> {
    match &auth.query.cursor {
        None => Ok(None),
        Some(c) => {
            let (t, id) = c.split_once('.').ok_or(ErrorCode::QueryInvalid)?;
            let ok = !t.is_empty()
                && t.bytes().all(|b| b.is_ascii_digit())
                && uuid::Uuid::parse_str(id).is_ok();
            ok.then(|| Some(c.clone())).ok_or(ErrorCode::QueryInvalid)
        }
    }
}

fn seq_cursor(auth: &Authorized) -> Result<Option<String>, ErrorCode> {
    match &auth.query.cursor {
        None => Ok(None),
        Some(c) => c
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0)
            .map(|n| Some(n.to_string()))
            .ok_or(ErrorCode::QueryInvalid),
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
    Some(match auth.capability.id {
        "communications.profiles.read" => {
            let Some(id) = auth.params.get("cfmeet_id").cloned() else {
                return Some(Err(ErrorCode::Internal));
            };
            match read(format!("{BASE}/profiles/{id}"), vec![]).await {
                Ok(v) => answer(profile(&v)),
                Err(e) => Err(e.into()),
            }
        }
        "communications.reports.list" | "communications.holds.list" => {
            let cursor = match time_cursor(auth) {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };
            let mut query: Vec<(&'static str, String)> = Vec::new();
            query.extend(limit.clone());
            query.extend(cursor.map(|c| ("cursor", c)));
            let (path, item): (&str, fn(&Value) -> T<Value>) =
                if auth.capability.id == "communications.reports.list" {
                    ("reports", report)
                } else {
                    ("holds", hold_item)
                };
            match read(format!("{BASE}/{path}"), query).await {
                Ok(v) => answer(t::page(&v, item).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
        "communications.audit.list" => {
            let cursor = match seq_cursor(auth) {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };
            let mut query: Vec<(&'static str, String)> = Vec::new();
            query.extend(q.start.map(|s| ("since", rfc(s))));
            query.extend(q.end.map(|e| ("until", rfc(e))));
            query.extend(limit.clone());
            query.extend(cursor.map(|c| ("cursor", c)));
            match read(format!("{BASE}/audit"), query).await {
                Ok(v) => answer(t::page(&v, audit_item).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
        "communications.metrics.read" => match read(format!("{BASE}/metrics"), vec![]).await {
            Ok(v) => answer(metrics_summary(&v, auth)),
            Err(e) => Err(e.into()),
        },
        _ => return None,
    })
}
