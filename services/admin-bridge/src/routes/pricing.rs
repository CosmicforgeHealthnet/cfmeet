//! Pricing, plan and finance reads (design §6.11 / §6.12 / §8), served from the Developer
//! Platform's pricing operator routes (`pricing.administer` + `X-CF-Operator: hq:<sub>`).
//!
//! Translation keeps identifiers, codes, amounts in minor units, quantities, digests,
//! HQ staff references (`hq:<sub>`) and times. `pricing.charges.simulate` is a read that
//! takes a body (a usage vector or a plan): it is checked strictly here and sent once to
//! the owner without an `Idempotency-Key`; nothing is charged and nothing changes.

use serde_json::{json, Map, Value};

use super::partners::day;
use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::DeveloperPlatform;
const BASE: &str = "/internal/v1/operator/pricing";
const FINANCE: &str = "/internal/v1/operator/finance";
/// The owner's report window: at most 31 days.
const MAX_REPORT_DAYS: i64 = 31;
/// The usage types a partner rate prices (simulation input keys).
pub const USAGE_TYPES: &[&str] = &[
    "MONTHLY_ACTIVE_USERS",
    "MEETING_MINUTES",
    "IN_APP_CALL_MINUTES",
    "MESSAGES",
    "RECORDING_MINUTES",
    "STORAGE",
    "API_CALLS",
];
/// Largest quantity the owner accepts.
const MAX_QUANTITY: u64 = 1_000_000_000_000_000;

fn rfc(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn uuid_param(auth: &Authorized, name: &str) -> Result<String, ErrorCode> {
    let raw = auth.params.get(name).ok_or(ErrorCode::Internal)?;
    uuid::Uuid::parse_str(raw)
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| ErrorCode::ResourceNotFound)
}

fn opt_bool(v: &Value, k: &str) -> T<Value> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::Bool(b)) => Ok(Value::Bool(*b)),
        _ => Err(OwnerError::Contract),
    }
}

fn nonneg(v: &Value, k: &str) -> T<Value> {
    match t::int(v, k)? {
        n if n.as_i64().is_some_and(|x| x >= 0) => Ok(n),
        _ => Err(OwnerError::Contract),
    }
}

fn opt_nonneg(v: &Value, k: &str) -> T<Value> {
    match t::opt_int(v, k)? {
        Value::Null => Ok(Value::Null),
        n if n.as_i64().is_some_and(|x| x >= 0) => Ok(n),
        _ => Err(OwnerError::Contract),
    }
}

fn array<'a>(v: &'a Value, k: &str, max: usize) -> T<&'a Vec<Value>> {
    match v.get(k).and_then(Value::as_array) {
        Some(a) if a.len() <= max => Ok(a),
        _ => Err(OwnerError::Contract),
    }
}

/// Plan limits (every key optional: an override sets a subset), or null.
fn limits(v: &Value, k: &str) -> T<Value> {
    let l = match v.get(k) {
        None | Some(Value::Null) => return Ok(Value::Null),
        Some(l) if l.is_object() => l,
        _ => return Err(OwnerError::Contract),
    };
    Ok(obj(vec![
        ("meeting_minutes", opt_nonneg(l, "meetingMinutes")?),
        ("participants", opt_nonneg(l, "participants")?),
        ("recording", opt_bool(l, "recording")?),
        ("storage_bytes", opt_nonneg(l, "storageBytes")?),
        ("attachment_bytes", opt_nonneg(l, "attachmentBytes")?),
        ("webinars", opt_bool(l, "webinars")?),
    ]))
}

fn price_book(b: &Value) -> T<Value> {
    Ok(obj(vec![
        ("price_book_id", t::uuid(b, "priceBookId")?),
        ("product_line", t::string(b, "productLine")?),
        ("currency", t::string(b, "currency")?),
        ("market_code", t::string(b, "marketCode")?),
        (
            "effective_version_id",
            t::opt_uuid(b, "effectiveVersionId")?,
        ),
        ("frozen", t::boolean(b, "frozen")?),
        ("updated_at", t::time(b, "updatedAt")?),
        ("version", t::int(b, "version")?),
    ]))
}

fn version_fields(v: &Value) -> T<Vec<(&'static str, Value)>> {
    Ok(vec![
        ("version_id", t::uuid(v, "versionId")?),
        ("price_book_id", t::uuid(v, "priceBookId")?),
        ("seq", t::int(v, "seq")?),
        ("version_state", t::string(v, "state")?),
        ("rates_digest", t::string(v, "ratesDigest")?),
        ("submitted_at", t::opt_time(v, "submittedAt")?),
        ("approved_at", t::opt_time(v, "approvedAt")?),
        ("approval_expires_at", t::opt_time(v, "approvalExpiresAt")?),
        ("rejected_reason", t::opt_string(v, "rejectedReason")?),
        ("decided_at", t::opt_time(v, "decidedAt")?),
        ("created_at", t::time(v, "createdAt")?),
        ("updated_at", t::time(v, "updatedAt")?),
        ("version", t::int(v, "version")?),
    ])
}

fn price_version(v: &Value) -> T<Value> {
    Ok(obj(version_fields(v)?))
}

fn partner_rate(r: &Value) -> T<Value> {
    let tiers = match r.get("tiers") {
        None | Some(Value::Null) => Value::Null,
        Some(_) => Value::Array(
            array(r, "tiers", 16)?
                .iter()
                .map(|x| {
                    Ok(obj(vec![
                        ("up_to", opt_nonneg(x, "upTo")?),
                        ("unit_amount_minor", nonneg(x, "unitAmountMinor")?),
                    ]))
                })
                .collect::<T<Vec<_>>>()?,
        ),
    };
    Ok(obj(vec![
        ("usage_type", t::string(r, "usageType")?),
        ("unit_amount_minor", nonneg(r, "unitAmountMinor")?),
        ("included_quantity", opt_nonneg(r, "includedQuantity")?),
        (
            "minimum_monthly_minor",
            opt_nonneg(r, "minimumMonthlyMinor")?,
        ),
        ("tiers", tiers),
    ]))
}

fn consumer_plan(p: &Value) -> T<Value> {
    Ok(obj(vec![
        ("plan_code", t::string(p, "code")?),
        ("label", t::string(p, "label")?),
        ("monthly_minor", nonneg(p, "monthlyMinor")?),
        ("yearly_minor", nonneg(p, "yearlyMinor")?),
        ("limits", limits(p, "limits")?),
    ]))
}

/// One version with its content: partner rates or CF Meet user plans by product line.
fn version_detail(v: &Value) -> T<Value> {
    let mut f = version_fields(v)?;
    let line = t::string(v, "productLine")?;
    let rates = v.get("rates").ok_or(OwnerError::Contract)?;
    let (partner, consumer) = match line.as_str() {
        Some("PARTNER_PLATFORM") => (
            Value::Array(
                array(rates, "rates", 7)?
                    .iter()
                    .map(partner_rate)
                    .collect::<T<Vec<_>>>()?,
            ),
            Value::Null,
        ),
        Some("CONSUMER") => (
            Value::Null,
            Value::Array(
                array(rates, "plans", 20)?
                    .iter()
                    .map(consumer_plan)
                    .collect::<T<Vec<_>>>()?,
            ),
        ),
        _ => return Err(OwnerError::Contract),
    };
    f.extend([
        ("product_line", line),
        ("market_code", t::string(v, "marketCode")?),
        ("partner_rates", partner),
        ("consumer_plans", consumer),
        ("submitted_digest", t::opt_string(v, "submittedDigest")?),
        ("drafted_by", t::string(v, "draftedBy")?),
        ("submitted_by", t::opt_string(v, "submittedBy")?),
        ("approved_by", t::opt_string(v, "approvedBy")?),
        ("approver_ref", t::opt_string(v, "approverRef")?),
        ("effective", t::boolean(v, "effective")?),
    ]);
    Ok(obj(f))
}

fn activation(a: &Value) -> T<Value> {
    Ok(obj(vec![
        ("activation_id", t::uuid(a, "activationId")?),
        ("price_book_id", t::uuid(a, "priceBookId")?),
        ("version_id", t::uuid(a, "versionId")?),
        ("kind", t::string(a, "kind")?),
        ("effective_from", t::time(a, "effectiveFrom")?),
        ("operator_ref", t::string(a, "operatorRef")?),
        ("approver_ref", t::opt_string(a, "approverRef")?),
        ("recorded_at", t::time(a, "recordedAt")?),
    ]))
}

fn freeze(f: &Value) -> T<Value> {
    Ok(obj(vec![
        ("market_code", t::string(f, "marketCode")?),
        ("freeze_state", t::string(f, "state")?),
        ("reason_code", t::string(f, "reasonCode")?),
        ("changed_by", t::string(f, "changedBy")?),
        ("frozen_at", t::opt_time(f, "frozenAt")?),
        ("updated_at", t::time(f, "updatedAt")?),
        ("version", t::int(f, "version")?),
    ]))
}

fn plan(p: &Value) -> T<Value> {
    Ok(obj(vec![
        ("price_book_id", t::uuid(p, "priceBookId")?),
        ("currency", t::string(p, "currency")?),
        ("market_code", t::string(p, "marketCode")?),
        ("version_id", t::opt_uuid(p, "versionId")?),
        ("plan_code", t::string(p, "planCode")?),
        ("label", t::string(p, "label")?),
        ("monthly_minor", nonneg(p, "monthlyMinor")?),
        ("yearly_minor", nonneg(p, "yearlyMinor")?),
        ("limits", limits(p, "limits")?),
    ]))
}

fn assignment(a: &Value) -> T<Value> {
    Ok(obj(vec![
        ("assignment_id", t::uuid(a, "assignmentId")?),
        ("holder_kind", t::string(a, "holderKind")?),
        ("holder_id", t::uuid(a, "holderId")?),
        ("plan_code", t::string(a, "planCode")?),
        ("price_book_id", t::uuid(a, "priceBookId")?),
        ("reason_code", t::string(a, "reasonCode")?),
        ("assigned_by", t::string(a, "assignedBy")?),
        ("effective_from", t::time(a, "effectiveFrom")?),
        ("current", t::boolean(a, "current")?),
    ]))
}

fn plan_override(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("override_id", t::uuid(o, "overrideId")?),
        ("holder_kind", t::string(o, "holderKind")?),
        ("holder_id", t::uuid(o, "holderId")?),
        ("override_state", t::string(o, "state")?),
        ("limits", limits(o, "limits")?),
        ("reason_code", t::string(o, "reasonCode")?),
        ("changed_by", t::string(o, "changedBy")?),
        ("updated_at", t::time(o, "updatedAt")?),
        ("version", t::int(o, "version")?),
    ]))
}

fn usage_row(u: &Value) -> T<Value> {
    Ok(obj(vec![
        ("workspace_id", t::uuid(u, "organizationId")?),
        ("plan_code", t::opt_string(u, "planCode")?),
        ("usage_type", t::string(u, "usageType")?),
        ("unit", t::string(u, "unit")?),
        ("quantity", t::int(u, "quantity")?),
        ("fact_count", t::int(u, "factCount")?),
    ]))
}

fn charge_line(l: &Value) -> T<Value> {
    Ok(obj(vec![
        ("usage_type", t::string(l, "usageType")?),
        ("quantity", nonneg(l, "quantity")?),
        ("included_quantity", nonneg(l, "includedQuantity")?),
        ("billable_quantity", nonneg(l, "billableQuantity")?),
        ("amount_minor", nonneg(l, "amountMinor")?),
    ]))
}

fn lines(v: &Value) -> T<Value> {
    match v.get("lines") {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(_) => Ok(Value::Array(
            array(v, "lines", 16)?
                .iter()
                .map(charge_line)
                .collect::<T<Vec<_>>>()?,
        )),
    }
}

fn workspace_charges(c: &Value) -> T<Value> {
    let l = lines(c)?;
    if l.is_null() {
        return Err(OwnerError::Contract);
    }
    Ok(obj(vec![
        ("workspace_id", t::uuid(c, "organizationId")?),
        ("lines", l),
        ("total_minor", nonneg(c, "totalMinor")?),
    ]))
}

fn spread(s: &Value) -> T<Value> {
    Ok(obj(vec![
        ("plan_code", t::string(s, "planCode")?),
        ("holder_kind", t::string(s, "holderKind")?),
        ("holders", nonneg(s, "holders")?),
        ("assigned_in_window", nonneg(s, "assignedInWindow")?),
    ]))
}

fn estimate(v: &Value) -> T<()> {
    if v.get("estimate") == Some(&Value::Bool(true)) {
        Ok(())
    } else {
        Err(OwnerError::Contract)
    }
}

fn simulation(v: &Value) -> T<Value> {
    estimate(v)?;
    Ok(obj(vec![
        ("price_book_id", t::uuid(v, "priceBookId")?),
        ("version_id", t::uuid(v, "versionId")?),
        ("version_state", t::string(v, "versionState")?),
        ("currency", t::string(v, "currency")?),
        ("estimate", Value::Bool(true)),
        ("total_minor", nonneg(v, "totalMinor")?),
        ("lines", lines(v)?),
        ("plan_code", t::opt_string(v, "planCode")?),
        ("cycle", t::opt_string(v, "cycle")?),
    ]))
}

fn page(v: &Value, item: fn(&Value) -> T<Value>) -> T<Value> {
    t::page(v, item).map(|(items, next)| json!({"items": items, "next_cursor": next}))
}

/// A report page: the items plus the window's UTC days `[from, to]`.
fn report(v: &Value, item: fn(&Value) -> T<Value>) -> T<Value> {
    let mut out = page(v, item)?;
    out["from"] = day(v, "from")?;
    out["to"] = day(v, "to")?;
    Ok(out)
}

fn plan_code_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=24).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b.iter().all(|c| c.is_ascii_uppercase() || *c == b'_')
}

/// HQ's simulation body (snake_case, closed) → the owner's (camelCase).
pub fn simulation_body(raw: &[u8]) -> Result<Value, ErrorCode> {
    let bad = ErrorCode::CommandBodyInvalid;
    let v: Value = serde_json::from_slice(raw).map_err(|_| bad)?;
    let o = v.as_object().ok_or(bad)?;
    let allowed = ["price_book_id", "version_id", "usage", "plan_code", "cycle"];
    if o.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(bad);
    }
    let uuid = |k: &str| -> Result<Option<Value>, ErrorCode> {
        match o.get(k) {
            None => Ok(None),
            Some(Value::String(s)) => uuid::Uuid::parse_str(s)
                .ok()
                .filter(|u| u.hyphenated().to_string() == *s)
                .map(|_| Some(Value::String(s.clone())))
                .ok_or(bad),
            Some(_) => Err(bad),
        }
    };
    let mut out = Map::new();
    out.insert("priceBookId".into(), uuid("price_book_id")?.ok_or(bad)?);
    if let Some(v) = uuid("version_id")? {
        out.insert("versionId".into(), v);
    }
    if let Some(u) = o.get("usage") {
        let u = u.as_object().ok_or(bad)?;
        if u.len() > USAGE_TYPES.len()
            || u.iter().any(|(k, q)| {
                !USAGE_TYPES.contains(&k.as_str()) || q.as_u64().is_none_or(|n| n > MAX_QUANTITY)
            })
        {
            return Err(bad);
        }
        out.insert("usage".into(), Value::Object(u.clone()));
    }
    if let Some(p) = o.get("plan_code") {
        let p = p.as_str().filter(|s| plan_code_ok(s)).ok_or(bad)?;
        out.insert("planCode".into(), Value::from(p));
    }
    if let Some(c) = o.get("cycle") {
        let c = c
            .as_str()
            .filter(|s| matches!(*s, "MONTHLY" | "YEARLY"))
            .ok_or(bad)?;
        out.insert("cycle".into(), Value::from(c));
    }
    // A usage vector prices a partner lineage; a plan a CF Meet user lineage.
    if out.contains_key("usage") == out.contains_key("planCode")
        || (out.contains_key("cycle") && !out.contains_key("planCode"))
    {
        return Err(bad);
    }
    Ok(Value::Object(out))
}

fn window(auth: &Authorized) -> Result<Vec<(&'static str, String)>, ErrorCode> {
    let q = &auth.query;
    let (start, end) = (
        q.start.ok_or(ErrorCode::QueryInvalid)?,
        q.end.ok_or(ErrorCode::QueryInvalid)?,
    );
    if end <= start || end - start > chrono::Duration::days(MAX_REPORT_DAYS) {
        return Err(ErrorCode::QueryInvalid);
    }
    Ok(vec![("since", rfc(start)), ("until", rfc(end))])
}

fn paging(auth: &Authorized) -> Vec<(&'static str, String)> {
    let q = &auth.query;
    let mut out = Vec::new();
    out.extend(q.limit.map(|l| ("limit", l.to_string())));
    out.extend(q.cursor.clone().map(|c| ("cursor", c)));
    out
}

/// The partner price book of the request's market: the lineage estimated charges are
/// computed with (organizations on the platform are priced by PARTNER_PLATFORM).
async fn partner_book(
    bridge: &Bridge,
    auth: &Authorized,
    operator: &str,
) -> Result<(String, Value), ErrorCode> {
    let mut cursor: Option<String> = None;
    for _ in 0..5 {
        let mut query = vec![
            ("productLine", "PARTNER_PLATFORM".to_string()),
            ("limit", "100".to_string()),
        ];
        query.extend(cursor.clone().map(|c| ("cursor", c)));
        let v = bridge
            .owners
            .read(O, &format!("{BASE}/price-books"), &query, operator)
            .await?;
        let (books, next) = t::page(&v, price_book)?;
        // Never another market's or product line's book.
        if let Some(b) = books.iter().find(|b| {
            b["product_line"] == "PARTNER_PLATFORM"
                && b["market_code"].as_str() == Some(auth.query.market.as_str())
        }) {
            let id = b["price_book_id"]
                .as_str()
                .map(str::to_string)
                .ok_or(ErrorCode::Internal)?;
            return Ok((id, b["version"].clone()));
        }
        match next {
            Value::String(c) => cursor = Some(c),
            _ => break,
        }
    }
    // No partner price book for this market: nothing to estimate with.
    Err(ErrorCode::ResourceNotFound)
}

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    let id = auth.capability.id;
    if !(id.starts_with("pricing.") || id.starts_with("finance.")) || !auth.capability.served {
        return None;
    }
    Some(serve(bridge, auth).await.map(|result| Answer {
        result,
        freshness: "LIVE",
    }))
}

async fn serve(bridge: &Bridge, auth: &Authorized) -> Result<Value, ErrorCode> {
    let operator = format!("hq:{}", auth.staff.claims.sub);
    let read = |path: String, query: Vec<(&'static str, String)>| {
        let operator = operator.clone();
        async move { bridge.owners.read(O, &path, &query, &operator).await }
    };
    let out: T<Value> = match auth.capability.id {
        "pricing.price-books.list" => page(
            &read(format!("{BASE}/price-books"), paging(auth)).await?,
            price_book,
        ),
        "pricing.versions.list" => {
            let book = uuid_param(auth, "price_book_id")?;
            let path = format!("{BASE}/price-books/{book}/versions");
            page(&read(path, paging(auth)).await?, price_version)
        }
        "pricing.versions.read" => {
            let version = uuid_param(auth, "version_id")?;
            version_detail(&read(format!("{BASE}/versions/{version}"), vec![]).await?)
        }
        "pricing.activations.list" => {
            let book = uuid_param(auth, "price_book_id")?;
            let path = format!("{BASE}/price-books/{book}/activations");
            page(&read(path, paging(auth)).await?, activation)
        }
        "pricing.freezes.list" => page(
            &read(format!("{BASE}/freezes"), paging(auth)).await?,
            freeze,
        ),
        "pricing.plans.list" => {
            // One bounded page (lineages × at most 20 plans): the owner takes no cursor.
            if auth.query.cursor.is_some() {
                return Err(ErrorCode::QueryInvalid);
            }
            let query = auth
                .query
                .limit
                .map(|l| ("limit", l.to_string()))
                .into_iter()
                .collect();
            page(&read(format!("{BASE}/plans"), query).await?, plan)
        }
        "pricing.plan-assignments.list" => {
            // `q`: a holder id (UUID) or a plan code.
            let q = auth.query.q.as_deref().ok_or(ErrorCode::QueryInvalid)?;
            let mut query = match uuid::Uuid::parse_str(q) {
                Ok(u) => vec![("holderId", u.hyphenated().to_string())],
                Err(_) if plan_code_ok(q) => vec![("planCode", q.to_string())],
                Err(_) => return Err(ErrorCode::QueryInvalid),
            };
            query.extend(paging(auth));
            page(
                &read(format!("{BASE}/plan-assignments"), query).await?,
                assignment,
            )
        }
        "pricing.overrides.list" => page(
            &read(format!("{BASE}/overrides"), paging(auth)).await?,
            plan_override,
        ),
        "pricing.charges.simulate" => {
            let body = simulation_body(&auth.body)?;
            let path = format!("{BASE}/simulations");
            simulation(&bridge.owners.query(O, &path, &body, &operator).await?)
        }
        "finance.usage.report" => {
            let mut query = window(auth)?;
            query.extend(paging(auth));
            report(&read(format!("{FINANCE}/usage"), query).await?, usage_row)
        }
        "finance.estimated-charges.report" => {
            let mut query = window(auth)?;
            let (book, book_version) = partner_book(bridge, auth, &operator).await?;
            query.insert(0, ("priceBookId", book.clone()));
            query.extend(paging(auth));
            let v = read(format!("{FINANCE}/estimated-charges"), query).await?;
            estimate(&v)?;
            report(&v, workspace_charges).and_then(|mut out| {
                // The estimate must be of the book the bridge chose.
                let answered = t::uuid(&v, "priceBookId")?;
                if answered.as_str() != Some(book.as_str()) {
                    return Err(OwnerError::Contract);
                }
                out["price_book_id"] = answered;
                out["price_book_version"] = book_version;
                out["version_id"] = t::opt_uuid(&v, "versionId")?;
                out["currency"] = t::string(&v, "currency")?;
                out["estimate"] = Value::Bool(true);
                Ok(out)
            })
        }
        "finance.plan-spread.report" => {
            let v = read(format!("{FINANCE}/plan-spread"), window(auth)?).await?;
            let items = array(&v, "items", 500)?
                .iter()
                .map(spread)
                .collect::<T<Vec<_>>>()?;
            Ok(json!({"items": items, "as_of": t::time(&v, "asOf")?}))
        }
        _ => return Err(ErrorCode::CapabilityDisabled),
    };
    out.map_err(ErrorCode::from)
}
