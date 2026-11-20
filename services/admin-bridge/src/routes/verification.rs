//! Verification (design §6.10, phase A3; agreed with HQ, design doc §17.6): the queue of
//! partner go-live requests and app reviews in HQ's shared case shape, and the decisions.
//!
//! * Reads come from the Developer Platform's verification views, which already speak
//!   HQ's shape; the bridge re-extracts every field strictly (a missing or mistyped field
//!   fails the answer) so nothing else is ever relayed.
//! * Commands run through the command core ([`super::commands`]): one UUID per command
//!   (`Idempotency-Key` = `operation_id`), `expected_version` = the case version (an
//!   integer), sent once to the owner's decision route with HQ's approver for two-person
//!   decisions; the owner answers with the case, whose state and version settle the
//!   operation.
//! * The scope is CF Meet's first-party tenant (the queue is an operator function); the
//!   partner's tenant is the case field `partner_tenant_id`.

use serde_json::{json, Value};

use super::commands::{self, Applied, CommandSpec};
use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::DeveloperPlatform;
const BASE: &str = "/internal/v1/operator";
pub const PARTNER_GO_LIVE: &str = "PARTNER_GO_LIVE";
pub const APP_REVIEW: &str = "APP_REVIEW";
const STATES: [&str; 7] = [
    "OPEN",
    "CLAIMED",
    "APPROVED",
    "REJECTED",
    "PUBLISHED",
    "SUSPENDED",
    "CANCELLED",
];

/// `cfmeet:<KIND>:<lowercase uuid>` → (kind, uuid).
pub fn parse_case_id(case_id: &str) -> Option<(&'static str, uuid::Uuid)> {
    let (kind, raw) = case_id.strip_prefix("cfmeet:")?.split_once(':')?;
    let kind = match kind {
        PARTNER_GO_LIVE => PARTNER_GO_LIVE,
        APP_REVIEW => APP_REVIEW,
        _ => return None,
    };
    let id = uuid::Uuid::parse_str(raw).ok()?;
    (id.hyphenated().to_string() == raw).then_some((kind, id))
}

fn one_of(v: &Value, k: &str, allowed: &[&str]) -> T<Value> {
    let s = t::string(v, k)?;
    if allowed.contains(&s.as_str().unwrap_or_default()) {
        Ok(s)
    } else {
        Err(OwnerError::Contract)
    }
}

fn sha256_hex(v: &Value, k: &str) -> T<Value> {
    let s = t::string(v, k)?;
    let ok = s.as_str().is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if ok {
        Ok(s)
    } else {
        Err(OwnerError::Contract)
    }
}

fn evidence(e: &Value) -> T<Value> {
    Ok(obj(vec![
        (
            "kind",
            one_of(e, "kind", &["USE_CASE_SUMMARY", "APP_MANIFEST"])?,
        ),
        ("ref", t::string(e, "ref")?),
        ("sha256", sha256_hex(e, "sha256")?),
    ]))
}

/// One case, strictly re-extracted (`VerificationCase`).
pub fn case(c: &Value) -> T<Value> {
    let id = t::string(c, "case_id")?;
    let (kind, _) = parse_case_id(id.as_str().unwrap_or_default()).ok_or(OwnerError::Contract)?;
    if c.get("kind").and_then(Value::as_str) != Some(kind)
        || c.get("product").and_then(Value::as_str) != Some("cfmeet")
        || c.get("approvals_required").and_then(Value::as_u64) != Some(2)
    {
        return Err(OwnerError::Contract);
    }
    let items = c
        .get("evidence")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if items.len() > 4 {
        return Err(OwnerError::Contract);
    }
    let version = t::int(c, "version")?;
    if version.as_i64().is_none_or(|v| v < 1) {
        return Err(OwnerError::Contract);
    }
    Ok(obj(vec![
        ("case_id", id),
        ("product", json!("cfmeet")),
        ("kind", json!(kind)),
        ("subject_ref", t::uuid(c, "subject_ref")?),
        ("state", one_of(c, "state", &STATES)?),
        ("version", version),
        ("assignee_ref", t::opt_string(c, "assignee_ref")?),
        ("submitted_at", t::time(c, "submitted_at")?),
        ("updated_at", t::time(c, "updated_at")?),
        (
            "evidence",
            Value::Array(items.iter().map(evidence).collect::<T<Vec<_>>>()?),
        ),
        ("approvals_required", json!(2)),
        ("partner_tenant_id", t::uuid(c, "partner_tenant_id")?),
    ]))
}

fn capability_list(v: &Value, k: &str) -> T<Value> {
    let items = v
        .get(k)
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if items.len() > 16
        || !items.iter().all(|i| {
            i.as_str().is_some_and(|s| {
                (2..=64).contains(&s.len())
                    && s.as_bytes()[0].is_ascii_uppercase()
                    && s.bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            })
        })
    {
        return Err(OwnerError::Contract);
    }
    Ok(Value::Array(items.clone()))
}

/// A case with its decision details (`VerificationCaseDetail`).
pub fn case_detail(c: &Value) -> T<Value> {
    let mut v = case(c)?;
    let d = c.get("details").ok_or(OwnerError::Contract)?;
    v["details"] = if v["kind"] == PARTNER_GO_LIVE {
        obj(vec![
            (
                "requested_capabilities",
                capability_list(d, "requested_capabilities")?,
            ),
            (
                "approved_capabilities",
                capability_list(d, "approved_capabilities")?,
            ),
        ])
    } else {
        obj(vec![
            ("app_version", t::int(d, "app_version")?),
            ("published_version", t::opt_int(d, "published_version")?),
        ])
    };
    Ok(v)
}

fn page(v: &Value, item: impl Fn(&Value) -> T<Value>) -> T<Value> {
    let items = v
        .get("items")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if items.len() > 100 {
        return Err(OwnerError::Contract);
    }
    let out = items.iter().map(item).collect::<T<Vec<_>>>()?;
    let next = match v.get("next_cursor") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) if s.len() <= 128 => Value::String(s.clone()),
        _ => return Err(OwnerError::Contract),
    };
    Ok(json!({"items": out, "next_cursor": next}))
}

fn history_item(h: &Value) -> T<Value> {
    let state = |k: &str| -> T<Value> {
        match h.get(k) {
            Some(Value::Null) => Ok(Value::Null),
            Some(_) => one_of(h, k, &STATES),
            None => Err(OwnerError::Contract),
        }
    };
    Ok(obj(vec![
        ("sequence", t::int(h, "sequence")?),
        ("from_state", state("from_state")?),
        ("to_state", state("to_state")?),
        (
            "actor_kind",
            one_of(h, "actor_kind", &["STAFF", "PARTNER", "SYSTEM"])?,
        ),
        ("actor_ref", t::opt_string(h, "actor_ref")?),
        ("reason_code", t::opt_string(h, "reason_code")?),
        ("occurred_at", t::time(h, "occurred_at")?),
    ]))
}

fn case_param(auth: &Authorized) -> Result<(&'static str, uuid::Uuid, String), ErrorCode> {
    let raw = auth.params.get("case_id").ok_or(ErrorCode::Internal)?;
    let (kind, id) = parse_case_id(raw).ok_or(ErrorCode::ResourceNotFound)?;
    Ok((kind, id, raw.clone()))
}

/// The owner's decision route for a command on a case of `kind`, and the fields of the
/// command body forwarded to it. `None`: the command does not apply to that kind.
fn decision(
    capability: &str,
    kind: &str,
    id: uuid::Uuid,
) -> Option<(String, &'static [&'static str])> {
    let pa = |cmd: &str| format!("{BASE}/production-access-requests/{id}/{cmd}");
    let app = |cmd: &str| format!("{BASE}/apps/{id}/{cmd}");
    Some(match (capability, kind) {
        ("verification.case.claim", PARTNER_GO_LIVE) => (pa("review"), &[]),
        ("verification.case.claim", APP_REVIEW) => (app("review"), &[]),
        ("verification.partner-go-live.approve", PARTNER_GO_LIVE) => {
            (pa("approve"), &["approved_capabilities"])
        }
        ("verification.partner-go-live.reject", PARTNER_GO_LIVE) => (pa("reject"), &[]),
        ("verification.partner-go-live.suspend", PARTNER_GO_LIVE) => (pa("suspend"), &[]),
        ("verification.app-review.approve", APP_REVIEW) => (app("approve"), &[]),
        ("verification.app-review.reject", APP_REVIEW) => (app("reject"), &[]),
        ("verification.app-review.publish", APP_REVIEW) => (app("publish"), &[]),
        _ => return None,
    })
}

/// The owner answers a decision with the case (`VerificationCaseDetail`).
fn applied(v: &Value) -> Option<Applied> {
    let c = case_detail(v).ok()?;
    Some(Applied {
        source_status: c["state"].as_str()?.to_string(),
        source_version: Some(c["version"].as_i64()?.to_string()),
        view: c,
    })
}

async fn command(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let (kind, id, _) = case_param(auth)?;
    let (path, extra) =
        decision(auth.capability.id, kind, id).ok_or(ErrorCode::ResourceNotFound)?;
    let (base, body) = commands::parse_body(&auth.body, extra)?;
    // HQ sends the case version as an integer (the request schemas say so).
    let version: i64 = match body.get("expected_version") {
        Some(Value::Number(n)) => n
            .as_i64()
            .filter(|v| *v >= 1)
            .ok_or(ErrorCode::CommandBodyInvalid)?,
        _ => return Err(ErrorCode::CommandBodyInvalid),
    };
    let reason_required = matches!(
        auth.capability.id,
        "verification.partner-go-live.reject" | "verification.app-review.reject"
    );
    if reason_required && base.reason_code.is_none() {
        return Err(ErrorCode::CommandBodyInvalid);
    }
    let mut owner_body = json!({"expectedVersion": version});
    if let Some(r) = &base.reason_code {
        owner_body["reasonCode"] = json!(r);
    }
    if let Some(caps) = body.get("approved_capabilities") {
        let ok = caps.as_array().is_some_and(|a| {
            (1..=16).contains(&a.len())
                && a.iter().all(|c| {
                    c.as_str().is_some_and(|s| {
                        (2..=64).contains(&s.len())
                            && s.as_bytes()[0].is_ascii_uppercase()
                            && s.bytes()
                                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    })
                })
        });
        if !ok {
            return Err(ErrorCode::CommandBodyInvalid);
        }
        owner_body["capabilities"] = caps.clone();
    }
    commands::execute(
        bridge,
        auth,
        CommandSpec {
            owner: O,
            owner_path: path,
            owner_body,
            target: format!("developer-platform:{kind}:{id}"),
            expected_version: base.expected_version,
            operation_id: base.operation_id,
        },
        applied,
    )
    .await
}

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    if !auth.capability.id.starts_with("verification.") || !auth.capability.served {
        return None;
    }
    let operator = format!("hq:{}", auth.staff.claims.sub);
    let q = &auth.query;
    let paging = || {
        let mut query: Vec<(&str, String)> = Vec::new();
        query.extend(q.limit.map(|l| ("limit", l.to_string())));
        query.extend(q.cursor.clone().map(|c| ("cursor", c)));
        query
    };
    let answer = |r: T<Value>| {
        r.map(|result| Answer {
            result,
            freshness: "LIVE",
        })
        .map_err(ErrorCode::from)
    };
    Some(match auth.capability.id {
        "verification.cases.list" => {
            match bridge
                .owners
                .read(
                    O,
                    &format!("{BASE}/verification/cases"),
                    &paging(),
                    &operator,
                )
                .await
            {
                Ok(v) => answer(page(&v, case)),
                Err(e) => Err(e.into()),
            }
        }
        "verification.cases.read" => match case_param(auth) {
            Err(e) => Err(e),
            Ok((_, _, raw)) => match bridge
                .owners
                .read(
                    O,
                    &format!("{BASE}/verification/cases/{raw}"),
                    &[],
                    &operator,
                )
                .await
            {
                Ok(v) => answer(case_detail(&v)),
                Err(e) => Err(e.into()),
            },
        },
        "verification.case-history.list" => match case_param(auth) {
            Err(e) => Err(e),
            Ok((_, _, raw)) => match bridge
                .owners
                .read(
                    O,
                    &format!("{BASE}/verification/cases/{raw}/history"),
                    &paging(),
                    &operator,
                )
                .await
            {
                Ok(v) => answer(page(&v, history_item).map(|mut p| {
                    p["case_id"] = json!(raw);
                    p
                })),
                Err(e) => Err(e.into()),
            },
        },
        _ => command(bridge, auth).await,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_ids_are_strict_and_commands_match_their_kind() {
        let id = uuid::Uuid::new_v4();
        assert_eq!(
            parse_case_id(&format!("cfmeet:APP_REVIEW:{id}")),
            Some((APP_REVIEW, id))
        );
        assert_eq!(parse_case_id(&format!("cfmeet:APP_PUBLISH:{id}")), None);
        assert_eq!(
            parse_case_id(&format!(
                "cfmeet:APP_REVIEW:{}",
                id.to_string().to_uppercase()
            )),
            None
        );
        assert!(decision("verification.partner-go-live.approve", APP_REVIEW, id).is_none());
        assert!(decision("verification.app-review.publish", PARTNER_GO_LIVE, id).is_none());
        assert_eq!(
            decision("verification.case.claim", APP_REVIEW, id)
                .unwrap()
                .0,
            format!("/internal/v1/operator/apps/{id}/review")
        );
    }
}
