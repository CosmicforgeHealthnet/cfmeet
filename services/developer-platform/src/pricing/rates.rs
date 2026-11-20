//! Price version content (design §8): validation, the canonical digest and the charge
//! arithmetic. Pure functions, no I/O. Every amount is an integer in MINOR units of the
//! price book's currency; nothing here converts currencies or charges anyone.
//!
//! * `PARTNER_PLATFORM`: `{"rates": [{"usageType", "unitAmountMinor", "includedQuantity"?,
//!   "minimumMonthlyMinor"?, "tiers"?: [{"upTo": n | null, "unitAmountMinor"}]}]}` — one
//!   rate per usage type; tiers are graduated over the quantity beyond the included
//!   amount, ascending, the last one open (`upTo: null`); the minimum monthly charge
//!   applies per rate.
//! * `CONSUMER`: `{"plans": [{"code", "label", "monthlyMinor", "yearlyMinor", "limits":
//!   {"meetingMinutes", "participants", "recording", "storageBytes", "attachmentBytes",
//!   "webinars"}}]}` — CF Meet user plans; `FREE` is required and costs 0.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::http::error::{ApiError, ErrorCode};

pub const PARTNER_PLATFORM: &str = "PARTNER_PLATFORM";
pub const CONSUMER: &str = "CONSUMER";

/// The usage types a partner rate prices.
pub const USAGE_TYPES: &[&str] = &[
    "MONTHLY_ACTIVE_USERS",
    "MEETING_MINUTES",
    "IN_APP_CALL_MINUTES",
    "MESSAGES",
    "RECORDING_MINUTES",
    "STORAGE",
    "API_CALLS",
];

/// The usage ledger type each rate usage type is estimated from (monthly active users
/// are not metered by the ledger and are never estimated).
pub fn ledger_type(usage_type: &str) -> Option<&'static str> {
    Some(match usage_type {
        "MEETING_MINUTES" => "MEETING_PARTICIPANT_MINUTE",
        "IN_APP_CALL_MINUTES" => "CALL_MINUTE_APP",
        "MESSAGES" => "MESSAGE_SENT",
        "RECORDING_MINUTES" => "RECORDING_MINUTE",
        "STORAGE" => "STORAGE_BYTE_TIME",
        "API_CALLS" => "API_OPERATION",
        _ => return None,
    })
}

/// The limits a CF Meet user plan controls: (key, boolean?).
pub const PLAN_LIMITS: &[(&str, bool)] = &[
    ("meetingMinutes", false),
    ("participants", false),
    ("recording", true),
    ("storageBytes", false),
    ("attachmentBytes", false),
    ("webinars", true),
];

/// Largest amount or quantity accepted anywhere (fits every sum in an i64 with room).
const MAX_AMOUNT: u64 = 1_000_000_000_000_000;
const MAX_TIERS: usize = 16;
const MAX_PLANS: usize = 20;

fn invalid() -> ApiError {
    ApiError(ErrorCode::ValidationError)
}

fn amount(v: Option<&Value>, required: bool) -> Result<u64, ApiError> {
    match v {
        None if !required => Ok(0),
        Some(v) => v.as_u64().filter(|n| *n <= MAX_AMOUNT).ok_or_else(invalid),
        None => Err(invalid()),
    }
}

fn only(obj: &Map<String, Value>, keys: &[&str]) -> Result<(), ApiError> {
    if obj.keys().all(|k| keys.contains(&k.as_str())) {
        Ok(())
    } else {
        Err(invalid())
    }
}

/// A plan code: `^[A-Z][A-Z_]{1,23}$` (it is also the resulting state of an assignment).
pub fn plan_code(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=24).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b.iter().all(|c| c.is_ascii_uppercase() || *c == b'_')
}

/// Validates the content of a version of `product_line`. `complete`: the version is
/// being submitted (a partner book prices at least one usage type; a consumer book has
/// `FREE` at 0).
pub fn validate(product_line: &str, rates: &Value, complete: bool) -> Result<(), ApiError> {
    let obj = rates.as_object().ok_or_else(invalid)?;
    match product_line {
        PARTNER_PLATFORM => {
            only(obj, &["rates"])?;
            let list = obj
                .get("rates")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?;
            if list.len() > USAGE_TYPES.len() || (complete && list.is_empty()) {
                return Err(invalid());
            }
            let mut seen = Vec::new();
            for r in list {
                let r = r.as_object().ok_or_else(invalid)?;
                only(
                    r,
                    &[
                        "usageType",
                        "unitAmountMinor",
                        "includedQuantity",
                        "minimumMonthlyMinor",
                        "tiers",
                    ],
                )?;
                let t = r
                    .get("usageType")
                    .and_then(Value::as_str)
                    .filter(|t| USAGE_TYPES.contains(t))
                    .ok_or_else(invalid)?;
                if seen.contains(&t) {
                    return Err(invalid());
                }
                seen.push(t);
                amount(r.get("unitAmountMinor"), true)?;
                amount(r.get("includedQuantity"), false)?;
                amount(r.get("minimumMonthlyMinor"), false)?;
                if let Some(tiers) = r.get("tiers") {
                    let tiers = tiers.as_array().ok_or_else(invalid)?;
                    if tiers.is_empty() || tiers.len() > MAX_TIERS {
                        return Err(invalid());
                    }
                    let mut last = 0u64;
                    for (i, tier) in tiers.iter().enumerate() {
                        let tier = tier.as_object().ok_or_else(invalid)?;
                        only(tier, &["upTo", "unitAmountMinor"])?;
                        amount(tier.get("unitAmountMinor"), true)?;
                        let open = i + 1 == tiers.len();
                        match tier.get("upTo") {
                            Some(Value::Null) | None if open => {}
                            Some(v) if !open => {
                                let up = amount(Some(v), true)?;
                                if up <= last {
                                    return Err(invalid());
                                }
                                last = up;
                            }
                            _ => return Err(invalid()),
                        }
                    }
                }
            }
            Ok(())
        }
        CONSUMER => {
            only(obj, &["plans"])?;
            let list = obj
                .get("plans")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?;
            if list.len() > MAX_PLANS {
                return Err(invalid());
            }
            let mut seen = Vec::new();
            for p in list {
                let p = p.as_object().ok_or_else(invalid)?;
                only(
                    p,
                    &["code", "label", "monthlyMinor", "yearlyMinor", "limits"],
                )?;
                let code = p
                    .get("code")
                    .and_then(Value::as_str)
                    .filter(|c| plan_code(c))
                    .ok_or_else(invalid)?;
                if seen.contains(&code) {
                    return Err(invalid());
                }
                seen.push(code);
                let label = p.get("label").and_then(Value::as_str).ok_or_else(invalid)?;
                if label.trim().is_empty()
                    || label.chars().count() > 40
                    || label.chars().any(char::is_control)
                {
                    return Err(invalid());
                }
                let monthly = amount(p.get("monthlyMinor"), true)?;
                let yearly = amount(p.get("yearlyMinor"), true)?;
                if code == "FREE" && (monthly != 0 || yearly != 0) {
                    return Err(invalid());
                }
                let limits = p
                    .get("limits")
                    .and_then(Value::as_object)
                    .ok_or_else(invalid)?;
                only(
                    limits,
                    &PLAN_LIMITS.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
                )?;
                for (k, boolean) in PLAN_LIMITS {
                    match (limits.get(*k), boolean) {
                        (Some(Value::Bool(_)), true) => {}
                        (Some(v), false) => {
                            amount(Some(v), true)?;
                        }
                        _ => return Err(invalid()),
                    }
                }
            }
            if complete && !seen.contains(&"FREE") {
                return Err(invalid());
            }
            Ok(())
        }
        _ => Err(invalid()),
    }
}

/// Validates override limits (a subset of the plan limits).
pub fn validate_limits(limits: &Value) -> Result<(), ApiError> {
    let obj = limits.as_object().ok_or_else(invalid)?;
    if obj.is_empty() {
        return Err(invalid());
    }
    for (k, v) in obj {
        match PLAN_LIMITS.iter().find(|(name, _)| name == k) {
            Some((_, true)) if v.is_boolean() => {}
            Some((_, false)) => {
                amount(Some(v), true)?;
            }
            _ => return Err(invalid()),
        }
    }
    Ok(())
}

/// JSON with object keys sorted at every level (the digest's input).
pub fn canonical(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&m[k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

/// The SHA-256 (hex) of the canonical content.
pub fn digest(v: &Value) -> String {
    hex::encode(Sha256::digest(canonical(v).as_bytes()))
}

/// The empty content of a new lineage.
pub fn empty(product_line: &str) -> Value {
    if product_line == CONSUMER {
        json!({ "plans": [] })
    } else {
        json!({ "rates": [] })
    }
}

/// The charge of one rate for `quantity` (minor units): graduated tiers over the
/// quantity beyond the included amount (or the flat unit amount), at least the minimum
/// monthly charge.
pub fn rate_charge(rate: &Value, quantity: u64) -> Result<(u64, u64, u64), ApiError> {
    let included = rate["includedQuantity"].as_u64().unwrap_or(0);
    let billable = quantity.saturating_sub(included);
    let mut total: u128 = 0;
    match rate.get("tiers").and_then(Value::as_array) {
        Some(tiers) => {
            let mut floor = 0u64;
            for tier in tiers {
                if billable <= floor {
                    break;
                }
                let up = tier["upTo"].as_u64().unwrap_or(u64::MAX);
                let units = billable.min(up) - floor;
                total += units as u128 * tier["unitAmountMinor"].as_u64().unwrap_or(0) as u128;
                floor = up;
            }
        }
        None => {
            total = billable as u128 * rate["unitAmountMinor"].as_u64().unwrap_or(0) as u128;
        }
    }
    let minimum = rate["minimumMonthlyMinor"].as_u64().unwrap_or(0) as u128;
    let total = total.max(minimum);
    let total = u64::try_from(total)
        .ok()
        .filter(|t| *t <= MAX_AMOUNT * 1000)
        .ok_or_else(invalid)?;
    Ok((included.min(quantity), billable, total))
}

/// The estimated charges of a partner rate card for a usage vector `{usageType: n}`.
pub fn simulate_partner(rates: &Value, usage: &Map<String, Value>) -> Result<Value, ApiError> {
    for (k, v) in usage {
        if !USAGE_TYPES.contains(&k.as_str()) {
            return Err(invalid());
        }
        amount(Some(v), true)?;
    }
    let mut lines = Vec::new();
    let mut total: u64 = 0;
    for rate in rates["rates"].as_array().into_iter().flatten() {
        let t = rate["usageType"].as_str().unwrap_or_default();
        let q = usage.get(t).and_then(Value::as_u64).unwrap_or(0);
        let (included, billable, charge) = rate_charge(rate, q)?;
        total = total.checked_add(charge).ok_or_else(invalid)?;
        lines.push(json!({
            "usageType": t, "quantity": q, "includedQuantity": included,
            "billableQuantity": billable, "amountMinor": charge,
        }));
    }
    Ok(json!({ "lines": lines, "totalMinor": total }))
}

/// The plan of `code` in a consumer version (FREE when the version has none: everyone
/// starts on Free).
pub fn plan<'a>(rates: &'a Value, code: &str) -> Option<&'a Value> {
    rates["plans"]
        .as_array()?
        .iter()
        .find(|p| p["code"] == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Value {
        json!({ "rates": [
            { "usageType": "MEETING_MINUTES", "unitAmountMinor": 2, "includedQuantity": 1000,
              "minimumMonthlyMinor": 500 },
            { "usageType": "MESSAGES", "unitAmountMinor": 0,
              "tiers": [ { "upTo": 100, "unitAmountMinor": 3 }, { "upTo": null, "unitAmountMinor": 1 } ] },
        ]})
    }

    #[test]
    fn partner_cards_are_validated_strictly() {
        assert!(validate(PARTNER_PLATFORM, &card(), true).is_ok());
        assert!(validate(PARTNER_PLATFORM, &json!({ "rates": [] }), false).is_ok());
        assert!(validate(PARTNER_PLATFORM, &json!({ "rates": [] }), true).is_err());
        for bad in [
            json!({ "rates": [{ "usageType": "SMS", "unitAmountMinor": 1 }] }),
            json!({ "rates": [{ "usageType": "MESSAGES", "unitAmountMinor": -1 }] }),
            json!({ "rates": [{ "usageType": "MESSAGES", "unitAmountMinor": 1.5 }] }),
            json!({ "rates": [{ "usageType": "MESSAGES", "unitAmountMinor": 1 },
                               { "usageType": "MESSAGES", "unitAmountMinor": 2 }] }),
            json!({ "rates": [{ "usageType": "MESSAGES", "unitAmountMinor": 1, "currency": "EUR" }] }),
            json!({ "rates": [{ "usageType": "MESSAGES", "unitAmountMinor": 1,
                "tiers": [{ "upTo": 10, "unitAmountMinor": 1 }] }] }),
            json!({ "rates": [{ "usageType": "MESSAGES", "unitAmountMinor": 1,
                "tiers": [{ "upTo": 10, "unitAmountMinor": 1 }, { "upTo": 5, "unitAmountMinor": 1 },
                          { "upTo": null, "unitAmountMinor": 1 }] }] }),
            json!({ "plans": [] }),
        ] {
            assert!(validate(PARTNER_PLATFORM, &bad, false).is_err(), "{bad}");
        }
    }

    #[test]
    fn consumer_plans_need_a_free_plan_at_zero() {
        let limits = json!({ "meetingMinutes": 40, "participants": 100, "recording": false,
            "storageBytes": 0, "attachmentBytes": 10_000_000, "webinars": false });
        let free = json!({ "code": "FREE", "label": "Free", "monthlyMinor": 0, "yearlyMinor": 0,
            "limits": limits });
        let pro = json!({ "code": "PRO", "label": "Pro", "monthlyMinor": 1200, "yearlyMinor": 12000,
            "limits": limits });
        assert!(validate(CONSUMER, &json!({ "plans": [free, pro] }), true).is_ok());
        assert!(validate(CONSUMER, &json!({ "plans": [pro] }), true).is_err());
        assert!(validate(CONSUMER, &json!({ "plans": [pro] }), false).is_ok());
        let mut paid_free = free.clone();
        paid_free["monthlyMinor"] = json!(1);
        assert!(validate(CONSUMER, &json!({ "plans": [paid_free] }), false).is_err());
        let mut no_limit = pro.clone();
        no_limit["limits"]
            .as_object_mut()
            .unwrap()
            .remove("webinars");
        assert!(validate(CONSUMER, &json!({ "plans": [no_limit] }), false).is_err());
        assert!(validate_limits(&json!({ "participants": 500 })).is_ok());
        assert!(validate_limits(&json!({ "participants": true })).is_err());
        assert!(validate_limits(&json!({})).is_err());
    }

    #[test]
    fn charges_use_included_amounts_graduated_tiers_and_minimums() {
        let c = card();
        let usage = json!({ "MEETING_MINUTES": 1100, "MESSAGES": 150 });
        let out = simulate_partner(&c, usage.as_object().unwrap()).unwrap();
        // Meetings: 100 billable × 2 = 200, below the 500 minimum.
        assert_eq!(out["lines"][0]["amountMinor"], 500);
        assert_eq!(out["lines"][0]["billableQuantity"], 100);
        // Messages: 100 × 3 + 50 × 1.
        assert_eq!(out["lines"][1]["amountMinor"], 350);
        assert_eq!(out["totalMinor"], 850);
        assert!(simulate_partner(&c, json!({ "SMS": 1 }).as_object().unwrap()).is_err());
    }

    #[test]
    fn the_digest_ignores_key_order() {
        let a = json!({ "rates": [{ "usageType": "MESSAGES", "unitAmountMinor": 1 }] });
        let b = json!({ "rates": [{ "unitAmountMinor": 1, "usageType": "MESSAGES" }] });
        assert_eq!(digest(&a), digest(&b));
        assert_ne!(digest(&a), digest(&card()));
        assert_eq!(digest(&a).len(), 64);
    }
}
