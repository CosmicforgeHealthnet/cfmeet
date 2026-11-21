//! Closed result schemas of the pricing, plan and finance reads (A6, Developer Platform)
//! and the simulation request. Identifiers, codes, minor-unit amounts, quantities,
//! digests, HQ staff references and times only. No property name is on HQ's
//! personal-data list.

use serde_json::{json, Map, Value};

use super::model::CURSOR_PATTERN;

pub fn insert(m: &mut Map<String, Value>) {
    let uuid = json!({"type": "string", "format": "uuid"});
    let nullable_uuid = json!({"type": ["string", "null"], "format": "uuid"});
    let ts = json!({"type": "string", "format": "date-time"});
    let nullable_ts = json!({"type": ["string", "null"], "format": "date-time"});
    let code = json!({"type": "string", "pattern": "^[A-Z][A-Z0-9_]{0,63}$"});
    let nullable_code = json!({"type": ["string", "null"], "pattern": "^[A-Z][A-Z0-9_]{0,63}$"});
    let amount = json!({"type": "integer", "minimum": 0});
    let nullable_amount = json!({"type": ["integer", "null"], "minimum": 0});
    let int = json!({"type": "integer"});
    let staff = json!({"type": "string", "pattern": "^hq:[A-Za-z0-9._:@-]{1,128}$", "description": "The HQ staff member (hq:<sub>)."});
    let nullable_staff =
        json!({"type": ["string", "null"], "pattern": "^hq:[A-Za-z0-9._:@-]{1,128}$"});
    let digest = json!({"type": "string", "pattern": "^[0-9a-f]{64}$"});
    let nullable_digest = json!({"type": ["string", "null"], "pattern": "^[0-9a-f]{64}$"});
    let day =
        json!({"type": "string", "format": "date", "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"});
    let holder_kind = json!({"enum": ["USER", "ORGANIZATION"]});
    let version_state =
        json!({"enum": ["DRAFT", "SUBMITTED", "APPROVED", "REJECTED", "SUPERSEDED"]});
    let product_line = json!({"enum": ["PARTNER_PLATFORM", "CONSUMER"]});
    let usage_type = json!({"type": "string", "minLength": 1, "maxLength": 64});
    let page = |item: &str| {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["items", "next_cursor"],
            "properties": {
                "items": {"type": "array", "maxItems": 100, "items": {"$ref": format!("#/components/schemas/{item}")}},
                "next_cursor": {"type": ["string", "null"], "pattern": CURSOR_PATTERN}
            }
        })
    };
    let object = |description: &str, props: Value| {
        let required: Vec<String> = props.as_object().expect("object").keys().cloned().collect();
        json!({"description": description, "type": "object", "additionalProperties": false, "required": required, "properties": props})
    };
    let limits = json!({
        "type": ["object", "null"],
        "description": "Plan limits (an override sets a subset; an absent limit is null).",
        "additionalProperties": false,
        "required": ["meeting_minutes", "participants", "recording", "storage_bytes", "attachment_bytes", "webinars"],
        "properties": {
            "meeting_minutes": nullable_amount, "participants": nullable_amount,
            "recording": {"type": ["boolean", "null"]}, "storage_bytes": nullable_amount,
            "attachment_bytes": nullable_amount, "webinars": {"type": ["boolean", "null"]}
        }
    });
    let charge_line = object(
        "One priced usage type: quantities and the amount in minor units.",
        json!({
            "usage_type": usage_type, "quantity": amount, "included_quantity": amount,
            "billable_quantity": amount, "amount_minor": amount
        }),
    );

    m.insert(
        "PricingPriceBook".into(),
        object(
            "One price book (a lineage of versions for a product line and market).",
            json!({
                "price_book_id": uuid, "product_line": product_line, "currency": {"type": "string", "pattern": "^[A-Z]{3}$"},
                "market_code": code, "effective_version_id": nullable_uuid, "frozen": {"type": "boolean"},
                "updated_at": ts, "version": int
            }),
        ),
    );
    m.insert("PricingPriceBookPage".into(), page("PricingPriceBook"));

    let version = json!({
        "version_id": uuid, "price_book_id": uuid, "seq": int, "version_state": version_state,
        "rates_digest": digest, "submitted_at": nullable_ts, "approved_at": nullable_ts,
        "approval_expires_at": nullable_ts, "rejected_reason": nullable_code, "decided_at": nullable_ts,
        "created_at": ts, "updated_at": ts, "version": int
    });
    m.insert(
        "PricingVersion".into(),
        object(
            "One price version (its state and content digest; no rates).",
            version.clone(),
        ),
    );
    m.insert("PricingVersionPage".into(), page("PricingVersion"));
    let tier = object(
        "A graduated tier over the quantity beyond the included amount (up_to null: open).",
        json!({"up_to": nullable_amount, "unit_amount_minor": amount}),
    );
    let partner_rate = object(
        "The rate of one usage type.",
        json!({
            "usage_type": usage_type, "unit_amount_minor": amount, "included_quantity": nullable_amount,
            "minimum_monthly_minor": nullable_amount,
            "tiers": {"type": ["array", "null"], "maxItems": 16, "items": tier}
        }),
    );
    let consumer_plan = object(
        "One CF Meet user plan.",
        json!({
            "plan_code": code, "label": {"type": "string", "minLength": 1, "maxLength": 40},
            "monthly_minor": amount, "yearly_minor": amount, "limits": limits
        }),
    );
    let mut detail = version.as_object().cloned().unwrap_or_default();
    for (k, v) in [
        ("product_line", product_line.clone()),
        ("market_code", code.clone()),
        (
            "partner_rates",
            json!({"type": ["array", "null"], "maxItems": 7, "items": partner_rate, "description": "PARTNER_PLATFORM content (null for CONSUMER)."}),
        ),
        (
            "consumer_plans",
            json!({"type": ["array", "null"], "maxItems": 20, "items": consumer_plan, "description": "CONSUMER content (null for PARTNER_PLATFORM)."}),
        ),
        ("submitted_digest", nullable_digest),
        ("drafted_by", staff.clone()),
        ("submitted_by", nullable_staff.clone()),
        ("approved_by", nullable_staff.clone()),
        ("approver_ref", nullable_staff.clone()),
        ("effective", json!({"type": "boolean"})),
    ] {
        detail.insert(k.into(), v);
    }
    m.insert(
        "PricingVersionDetail".into(),
        object(
            "One price version with its content and who drafted, submitted and approved it.",
            Value::Object(detail),
        ),
    );

    m.insert(
        "PricingActivation".into(),
        object(
            "One append-only activation of a price version.",
            json!({
                "activation_id": uuid, "price_book_id": uuid, "version_id": uuid,
                "kind": {"enum": ["SCHEDULED", "ACTIVATED", "ROLLED_BACK"]}, "effective_from": ts,
                "operator_ref": staff, "approver_ref": nullable_staff, "recorded_at": ts
            }),
        ),
    );
    m.insert("PricingActivationPage".into(), page("PricingActivation"));

    m.insert(
        "PricingFreeze".into(),
        object(
            "One market's price freeze.",
            json!({
                "market_code": code, "freeze_state": {"enum": ["FROZEN", "UNFROZEN"]}, "reason_code": code,
                "changed_by": staff, "frozen_at": nullable_ts, "updated_at": ts, "version": int
            }),
        ),
    );
    m.insert("PricingFreezePage".into(), page("PricingFreeze"));

    m.insert(
        "PricingPlan".into(),
        object(
            "One CF Meet user plan of a CONSUMER lineage's effective version (FREE always).",
            json!({
                "price_book_id": uuid, "currency": {"type": "string", "pattern": "^[A-Z]{3}$"}, "market_code": code,
                "version_id": nullable_uuid, "plan_code": code,
                "label": {"type": "string", "minLength": 1, "maxLength": 40},
                "monthly_minor": amount, "yearly_minor": amount, "limits": limits
            }),
        ),
    );
    let mut plans = page("PricingPlan");
    plans["description"] = json!("Every plan in one page (next_cursor is always null).");
    m.insert("PricingPlanPage".into(), plans);

    m.insert(
        "PricingPlanAssignment".into(),
        object(
            "One plan assignment of a user or organization (append-only history).",
            json!({
                "assignment_id": uuid, "holder_kind": holder_kind, "holder_id": uuid, "plan_code": code,
                "price_book_id": uuid, "reason_code": code, "assigned_by": staff, "effective_from": ts,
                "current": {"type": "boolean"}
            }),
        ),
    );
    m.insert(
        "PricingPlanAssignmentPage".into(),
        page("PricingPlanAssignment"),
    );

    m.insert(
        "PricingOverride".into(),
        object(
            "One holder's limit override.",
            json!({
                "override_id": uuid, "holder_kind": holder_kind, "holder_id": uuid,
                "override_state": {"enum": ["ACTIVE", "REVOKED"]}, "limits": limits, "reason_code": code,
                "changed_by": staff, "updated_at": ts, "version": int
            }),
        ),
    );
    m.insert("PricingOverridePage".into(), page("PricingOverride"));

    m.insert(
        "PricingChargeSimulationRequest".into(),
        json!({
            "description": "What a usage vector (PARTNER_PLATFORM: usage) or a plan (CONSUMER: plan_code, cycle) would cost under a version (default: the lineage's effective one). A read: nothing is charged and nothing changes.",
            "type": "object",
            "additionalProperties": false,
            "required": ["price_book_id"],
            "properties": {
                "price_book_id": uuid,
                "version_id": uuid,
                "usage": {
                    "type": "object",
                    "maxProperties": 7,
                    "propertyNames": {"enum": ["MONTHLY_ACTIVE_USERS", "MEETING_MINUTES", "IN_APP_CALL_MINUTES", "MESSAGES", "RECORDING_MINUTES", "STORAGE", "API_CALLS"]},
                    "additionalProperties": {"type": "integer", "minimum": 0, "maximum": 1_000_000_000_000_000_i64}
                },
                "plan_code": {"type": "string", "pattern": "^[A-Z][A-Z_]{1,23}$"},
                "cycle": {"enum": ["MONTHLY", "YEARLY"]}
            },
            "oneOf": [{"required": ["usage"]}, {"required": ["plan_code"]}]
        }),
    );
    m.insert(
        "PricingChargeSimulation".into(),
        object(
            "An estimate in minor units of the book's currency (never a charge).",
            json!({
                "price_book_id": uuid, "version_id": uuid, "version_state": version_state,
                "currency": {"type": "string", "pattern": "^[A-Z]{3}$"}, "estimate": {"const": true},
                "total_minor": amount,
                "lines": {"type": ["array", "null"], "maxItems": 16, "items": charge_line.clone()},
                "plan_code": nullable_code, "cycle": {"enum": ["MONTHLY", "YEARLY", null]}
            }),
        ),
    );

    let report = |item: &str, description: &str| {
        let mut r = page(item);
        r["description"] = json!(description);
        r["required"] = json!(["items", "next_cursor", "from", "to"]);
        r["properties"]["from"] = day.clone();
        r["properties"]["to"] = day.clone();
        r
    };
    m.insert(
        "FinanceUsageRow".into(),
        object(
            "Metered usage of one workspace and usage type over the window.",
            json!({
                "workspace_id": uuid, "plan_code": nullable_code, "usage_type": usage_type,
                "unit": {"type": "string", "minLength": 1, "maxLength": 32}, "quantity": int, "fact_count": int
            }),
        ),
    );
    m.insert(
        "FinanceUsageReport".into(),
        report(
            "FinanceUsageRow",
            "Usage per workspace over the window's UTC days [from, to] (at most 31 days).",
        ),
    );
    m.insert(
        "FinanceWorkspaceCharges".into(),
        object(
            "The estimated charges of one workspace.",
            json!({
                "workspace_id": uuid,
                "lines": {"type": "array", "maxItems": 16, "items": charge_line},
                "total_minor": amount
            }),
        ),
    );
    let mut charges = report(
        "FinanceWorkspaceCharges",
        "Estimated charges (an estimate, one currency, minor units) under the PARTNER_PLATFORM price book of the request's market (price_book_id at price_book_version; no such book: RESOURCE_NOT_FOUND, never another market's book), over the window's UTC days [from, to].",
    );
    charges["required"] = json!([
        "items",
        "next_cursor",
        "from",
        "to",
        "price_book_id",
        "price_book_version",
        "version_id",
        "currency",
        "estimate"
    ]);
    charges["properties"]["price_book_id"] = uuid.clone();
    charges["properties"]["price_book_version"] = json!({"type": "integer", "minimum": 1, "description": "The chosen price book's version when the bridge chose it."});
    charges["properties"]["version_id"] = nullable_uuid.clone();
    charges["properties"]["currency"] = json!({"type": "string", "pattern": "^[A-Z]{3}$"});
    charges["properties"]["estimate"] = json!({"const": true});
    m.insert("FinanceEstimatedChargesReport".into(), charges);
    m.insert(
        "FinancePlanSpread".into(),
        object(
            "Holders on one plan, and those assigned to it in the window.",
            json!({"plan_code": code, "holder_kind": holder_kind, "holders": amount, "assigned_in_window": amount}),
        ),
    );
    m.insert(
        "FinancePlanSpreadReport".into(),
        json!({
            "description": "The plan spread as of the window's end (one bounded list).",
            "type": "object",
            "additionalProperties": false,
            "required": ["items", "as_of"],
            "properties": {
                "items": {"type": "array", "maxItems": 500, "items": {"$ref": "#/components/schemas/FinancePlanSpread"}},
                "as_of": ts
            }
        }),
    );
}
