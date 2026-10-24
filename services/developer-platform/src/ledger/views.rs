//! Customer reads of the unified usage ledger (Section 55 §11.5, Section 15 Usage /
//! Limits families).
//!
//! Every read is environment-scoped (a machine credential only ever reads its OWN
//! environment; humans need `UsageRead` / `LimitsRead` / `UsageExport` in the
//! environment's organization). Sandbox and production usage are never merged: an
//! environment is one kind. Views never expose provider rates, raw provider ids,
//! protected contacts or content: facts carry safe refs and public dimensions only
//! (cost evidence stays internal).

use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use sea_orm::ConnectionTrait;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{
    control::{
        authorize, entitlements,
        pagination::{Page, PageQuery},
        stmt, Caller, Principal,
    },
    domain::rbac::Action,
    http::error::{ApiError, ErrorCode},
    state::AppState,
};

/// `(tenant, application, kind)` of an addressable environment.
pub async fn environment<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> Result<(Uuid, Uuid, String), ApiError> {
    let r = db
        .query_one(stmt(
            "SELECT tenant_id, application_id, kind FROM devplat_environment_profiles WHERE environment_id = $1",
            vec![environment_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok((
        r.try_get("", "tenant_id")?,
        r.try_get("", "application_id")?,
        r.try_get("", "kind")?,
    ))
}

/// Authorizes an environment-scoped usage read.
pub async fn authorize_environment(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    action: Action,
) -> Result<(Uuid, Uuid, String), ApiError> {
    let (tenant, app, kind) = match environment(&state.db, environment_id).await {
        Ok(v) => v,
        // A machine credential's own environment may be known by reference only.
        Err(ApiError(ErrorCode::NotFound)) => match &caller.principal {
            Principal::Machine(m) if m.context.environment_id == environment_id => (
                m.context.tenant_id,
                m.context.application_id,
                m.context.environment_kind.clone(),
            ),
            _ => return Err(ApiError(ErrorCode::NotFound)),
        },
        Err(e) => return Err(e),
    };
    if let Principal::Machine(m) = &caller.principal {
        if m.context.environment_id != environment_id {
            return Err(ApiError(ErrorCode::NotFound));
        }
    }
    authorize(state, caller, tenant, action).await?;
    Ok((tenant, app, kind))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub usage_type: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub granularity: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// `[from, to]` days: default the current UTC month; at most 400 days.
pub fn period(q: &UsageQuery) -> Result<(NaiveDate, NaiveDate), ApiError> {
    let day = |s: &str| {
        crate::control::usage::parse_day(s).or_else(|_| {
            DateTime::parse_from_rfc3339(s)
                .map(|t| t.with_timezone(&Utc).date_naive())
                .map_err(|_| ApiError(ErrorCode::ValidationError))
        })
    };
    let today = Utc::now().date_naive();
    let from = match &q.from {
        Some(f) => day(f)?,
        None => NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today),
    };
    let to = match &q.to {
        Some(t) => day(t)?,
        None => today,
    };
    if to < from || (to - from).num_days() > 400 {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    Ok((from, to))
}

fn usage_type(q: &UsageQuery) -> Result<Option<String>, ApiError> {
    match &q.usage_type {
        None => Ok(None),
        Some(t) if cf_usage_facts::definition(t).is_some() => Ok(Some(t.clone())),
        Some(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

/// Minutes of a millisecond quantity (customer convenience; the quantity is canonical).
fn derived(unit: &str, quantity: i64) -> Value {
    match unit {
        "MILLISECOND" => {
            json!({ "minutes": (quantity as f64 / 60_000.0 * 1000.0).round() / 1000.0 })
        }
        "BYTE_SECOND" => {
            json!({ "gibibyteHours": (quantity as f64 / 1_073_741_824.0 / 3600.0 * 1_000_000.0).round() / 1_000_000.0 })
        }
        _ => json!({}),
    }
}

async fn rollup_rows<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    ty: Option<&str>,
) -> Result<Vec<(NaiveDate, String, String, i64, i64)>, ApiError> {
    let rows = db
        .query_all(stmt(
            "SELECT usage_day, usage_type, unit, quantity, fact_count FROM devplat_usage_rollups_daily \
             WHERE environment_id = $1 AND usage_day BETWEEN $2 AND $3 AND ($4::text IS NULL OR usage_type = $4) \
             ORDER BY usage_day, usage_type",
            vec![environment_id.into(), from.into(), to.into(), ty.map(str::to_string).into()],
        ))
        .await?;
    rows.iter()
        .map(|r| {
            Ok((
                r.try_get("", "usage_day")?,
                r.try_get("", "usage_type")?,
                r.try_get("", "unit")?,
                r.try_get("", "quantity")?,
                r.try_get("", "fact_count")?,
            ))
        })
        .collect()
}

/// `GET /v1/environments/{id}/usage` — daily rollups and period totals.
pub async fn summary(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    q: UsageQuery,
) -> Result<Value, ApiError> {
    let (tenant, app, kind) =
        authorize_environment(state, caller, environment_id, Action::UsageRead).await?;
    let (from, to) = period(&q)?;
    let ty = usage_type(&q)?;
    let rows = rollup_rows(&state.db, environment_id, from, to, ty.as_deref()).await?;
    let mut totals: Map<String, Value> = Map::new();
    let mut days = Vec::new();
    for (day, t, unit, quantity, count) in &rows {
        days.push(
            json!({ "day": day.to_string(), "usageType": t, "unit": unit, "quantity": quantity,
                          "factCount": count, "derived": derived(unit, *quantity) }),
        );
        let e = totals.entry(t.clone()).or_insert_with(
            || json!({ "usageType": t, "unit": unit, "quantity": 0, "factCount": 0 }),
        );
        e["quantity"] = json!(e["quantity"].as_i64().unwrap_or(0) + quantity);
        e["factCount"] = json!(e["factCount"].as_i64().unwrap_or(0) + count);
    }
    let totals: Vec<Value> = totals
        .into_values()
        .map(|mut t| {
            let d = derived(
                t["unit"].as_str().unwrap_or_default(),
                t["quantity"].as_i64().unwrap_or(0),
            );
            t["derived"] = d;
            t
        })
        .collect();
    Ok(json!({
        "organizationId": tenant, "projectId": app, "environmentId": environment_id,
        "environmentKind": kind, "sandbox": kind != "PRODUCTION",
        "from": from.to_string(), "to": to.to_string(),
        "usageTypes": cf_usage_facts::usage_types().collect::<Vec<_>>(),
        "days": days, "totals": totals,
    }))
}

const FACT_VIEW: &str = "seq, jsonb_build_object('id', id, 'usageType', usage_type, 'unit', unit, \
    'quantity', quantity, 'resourceRef', resource_ref, 'producer', producer, 'sourceFactId', source_fact_id, \
    'occurredAt', occurred_at, 'usageDay', usage_day, 'periodStart', period_start, 'periodEnd', period_end, \
    'environmentId', environment_id, 'environmentKind', environment_kind, \
    'sandbox', environment_kind <> 'PRODUCTION', 'dimensions', dimensions, 'recordedAt', ingested_at) AS v";

fn public(mut v: Value) -> Value {
    let ty = v["usageType"].as_str().unwrap_or_default().to_string();
    v["dimensions"] = cf_usage_facts::UsageFact::public_dimensions(&ty, &v["dimensions"]);
    v
}

async fn fact_page<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    ty: Option<&str>,
    after: Option<i64>,
    limit: usize,
) -> Result<(Vec<Value>, Option<String>), ApiError> {
    let rows = db
        .query_all(stmt(
            &format!(
                "SELECT {FACT_VIEW} FROM devplat_usage_facts WHERE environment_id = $1 \
                   AND occurred_at >= $2 AND occurred_at < $3 AND ($4::text IS NULL OR usage_type = $4) \
                   AND seq > $5 ORDER BY seq LIMIT $6"
            ),
            vec![
                environment_id.into(),
                from.into(),
                to.into(),
                ty.map(str::to_string).into(),
                after.unwrap_or(0).into(),
                (limit as i64 + 1).into(),
            ],
        ))
        .await?;
    let mut items = Vec::new();
    let mut last = None;
    for r in rows.iter().take(limit) {
        last = Some(r.try_get::<i64>("", "seq")?);
        items.push(public(r.try_get::<Value>("", "v")?));
    }
    let next = (rows.len() > limit)
        .then(|| last.map(Page::seq_cursor))
        .flatten();
    Ok((items, next))
}

fn day_bounds(from: NaiveDate, to: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    (
        from.and_hms_opt(0, 0, 0).expect("midnight").and_utc(),
        (to + Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .expect("midnight")
            .and_utc(),
    )
}

fn page_of(q: &UsageQuery) -> Result<Page, ApiError> {
    Page::from_query(&PageQuery {
        limit: q.limit,
        cursor: q.cursor.clone(),
    })
}

/// `GET /v1/environments/{id}/usage/facts` — immutable facts (keyset pages).
pub async fn facts(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    q: UsageQuery,
) -> Result<Value, ApiError> {
    authorize_environment(state, caller, environment_id, Action::UsageRead).await?;
    let (from, to) = period(&q)?;
    let ty = usage_type(&q)?;
    let page = page_of(&q)?;
    if page.has_cursor() && page.cursor_seq().is_none() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let (start, end) = day_bounds(from, to);
    let (items, next) = fact_page(
        &state.db,
        environment_id,
        start,
        end,
        ty.as_deref(),
        page.cursor_seq(),
        page.limit,
    )
    .await?;
    Ok(json!({ "items": items, "nextCursor": next }))
}

fn csv_field(v: &str) -> String {
    // Values are bounded codes / ids / numbers; quote defensively and neutralize
    // spreadsheet formula prefixes.
    let v = if v.starts_with(['=', '+', '-', '@']) {
        format!("'{v}")
    } else {
        v.to_string()
    };
    format!("\"{}\"", v.replace('"', "\"\""))
}

/// `GET /v1/environments/{id}/usage/export?format=csv|json&granularity=daily|facts`.
pub async fn export(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    q: UsageQuery,
) -> Result<(String, String), ApiError> {
    let (_, _, kind) =
        authorize_environment(state, caller, environment_id, Action::UsageExport).await?;
    let (from, to) = period(&q)?;
    let ty = usage_type(&q)?;
    let format = q.format.as_deref().unwrap_or("csv");
    let granularity = q.granularity.as_deref().unwrap_or("daily");
    if !matches!(format, "csv" | "json") || !matches!(granularity, "daily" | "facts") {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    if granularity == "daily" {
        let rows = rollup_rows(&state.db, environment_id, from, to, ty.as_deref()).await?;
        if format == "json" {
            let items: Vec<Value> = rows
                .iter()
                .map(|(d, t, u, qn, c)| json!({ "day": d.to_string(), "usageType": t, "unit": u, "quantity": qn, "factCount": c }))
                .collect();
            return Ok((
                "application/json".into(),
                json!({ "environmentId": environment_id, "environmentKind": kind,
                "from": from.to_string(), "to": to.to_string(), "items": items })
                .to_string(),
            ));
        }
        let mut out = String::from(
            "environment_id,environment_kind,day,usage_type,unit,quantity,fact_count\n",
        );
        for (d, t, u, qn, c) in rows {
            out.push_str(&format!(
                "{},{},{},{},{},{qn},{c}\n",
                csv_field(&environment_id.to_string()),
                csv_field(&kind),
                csv_field(&d.to_string()),
                csv_field(&t),
                csv_field(&u)
            ));
        }
        return Ok(("text/csv; charset=utf-8".into(), out));
    }
    // Fact-level export: bounded to 10 000 facts per request (page with `from` / `to`).
    let (start, end) = day_bounds(from, to);
    let mut all = Vec::new();
    let mut after = None;
    loop {
        let (items, next) = fact_page(
            &state.db,
            environment_id,
            start,
            end,
            ty.as_deref(),
            after,
            100,
        )
        .await?;
        all.extend(items);
        match next {
            Some(c) if all.len() < 10_000 => {
                after = Page::from_query(&PageQuery {
                    limit: None,
                    cursor: Some(c),
                })?
                .cursor_seq();
            }
            _ => break,
        }
    }
    if format == "json" {
        return Ok((
            "application/json".into(),
            json!({ "environmentId": environment_id, "items": all }).to_string(),
        ));
    }
    let mut out = String::from(
        "fact_id,occurred_at,usage_type,unit,quantity,resource_ref,environment_kind,producer\n",
    );
    for f in all {
        let s = |k: &str| {
            f[k].as_str()
                .map(str::to_string)
                .unwrap_or_else(|| f[k].to_string())
        };
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            csv_field(&s("id")),
            csv_field(&s("occurredAt")),
            csv_field(&s("usageType")),
            csv_field(&s("unit")),
            s("quantity"),
            csv_field(&s("resourceRef")),
            csv_field(&s("environmentKind")),
            csv_field(&s("producer"))
        ));
    }
    Ok(("text/csv; charset=utf-8".into(), out))
}

/// Today's (UTC) usage of the metered limits.
async fn today_usage<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> Result<(i64, i64), ApiError> {
    let today = Utc::now().date_naive();
    let api: i64 = db
        .query_one(stmt(
            "SELECT COALESCE((SELECT request_count FROM devplat_api_usage_daily WHERE environment_id = $1 AND usage_day = $2), 0)::bigint AS n",
            vec![environment_id.into(), today.into()],
        ))
        .await?
        .map(|r| r.try_get("", "n"))
        .transpose()?
        .unwrap_or(0);
    let sms: i64 = db
        .query_one(stmt(
            "SELECT COALESCE((SELECT fact_count FROM devplat_usage_rollups_daily WHERE environment_id = $1 \
               AND usage_day = $2 AND usage_type = 'SMS_SEGMENT'), 0)::bigint AS n",
            vec![environment_id.into(), today.into()],
        ))
        .await?
        .map(|r| r.try_get("", "n"))
        .transpose()?
        .unwrap_or(0);
    Ok((api, sms))
}

/// Who enforces each domain entitlement.
fn enforced_by(key: &str) -> &'static str {
    match key {
        "concurrentCalls" => "cfmeet.calling",
        "meetingParticipants" => "cfmeet.meeting-service",
        "activeNumbers" => "cfmeet.numbering",
        "smsPerDay" => "cfmeet.sms-gateway",
        "messagingAttachmentBytes" => "cfmeet.messaging",
        _ => "cfmeet.api-gateway",
    }
}

fn snake(key: &str) -> String {
    let mut out = String::new();
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// Warning level of `current / ceiling` (80 %: WARNING, 100 %: EXHAUSTED).
pub fn warning(limit: &str, current: i64, ceiling: i64) -> Option<Value> {
    if ceiling <= 0 {
        return Some(
            json!({ "limit": limit, "level": "EXHAUSTED", "current": current, "ceiling": ceiling, "ratio": 1.0 }),
        );
    }
    let ratio = current as f64 / ceiling as f64;
    let level = if ratio >= 1.0 {
        "EXHAUSTED"
    } else if ratio >= 0.8 {
        "WARNING"
    } else {
        return None;
    };
    Some(
        json!({ "limit": limit, "level": level, "current": current, "ceiling": ceiling,
                 "ratio": (ratio * 1000.0).round() / 1000.0 }),
    )
}

/// The limits view of an environment (optionally for one caller credential).
pub async fn limits_view(
    state: &AppState,
    environment_id: Uuid,
    kind: Option<&str>,
    service_account: Option<Uuid>,
    credential: Option<Uuid>,
) -> Result<Value, ApiError> {
    let snap = entitlements::snapshot(&state.db, environment_id, kind).await?;
    let eff = if service_account.is_some() || credential.is_some() {
        entitlements::effective_for(&state.db, environment_id, kind, service_account, credential)
            .await?
    } else {
        snap.clone()
    };
    let (api_today, sms_today) = today_usage(&state.db, environment_id).await?;
    let now = Utc::now();
    let tomorrow = (now.date_naive() + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .expect("midnight")
        .and_utc();
    let rpm = eff["apiRequestsPerMinute"].as_i64().unwrap_or(0);
    let rpd = eff["apiRequestsPerDay"].as_i64().unwrap_or(0);
    let mut limits = vec![
        json!({ "limit": "API_REQUESTS_PER_MINUTE", "window": "MINUTE", "ceiling": rpm,
                "limitingLevel": eff["limitingLevel"]["apiRequestsPerMinute"], "current": null,
                "remaining": null, "enforcedBy": "cfmeet.api-gateway" }),
        json!({ "limit": "API_REQUESTS_PER_DAY", "window": "DAY", "ceiling": rpd,
                "limitingLevel": eff["limitingLevel"]["apiRequestsPerDay"], "current": api_today,
                "remaining": (rpd - api_today).max(0), "resetsAt": tomorrow.to_rfc3339(),
                "enforcedBy": "cfmeet.api-gateway" }),
    ];
    let mut warnings: Vec<Value> = warning("API_REQUESTS_PER_DAY", api_today, rpd)
        .into_iter()
        .collect();
    if let Some(domain) = eff["domainEntitlements"].as_object() {
        for (k, v) in domain {
            let ceiling = v.as_i64().unwrap_or(0);
            let name = snake(k);
            let (current, remaining, window) = if k == "smsPerDay" {
                if let Some(w) = warning(&name, sms_today, ceiling) {
                    warnings.push(w);
                }
                (json!(sms_today), json!((ceiling - sms_today).max(0)), "DAY")
            } else {
                // Current state is the owning service's (concurrency / inventory).
                (Value::Null, Value::Null, "CURRENT")
            };
            limits.push(json!({ "limit": name, "window": window, "ceiling": ceiling,
                                "current": current, "remaining": remaining,
                                "enforcedBy": enforced_by(k) }));
        }
    }
    Ok(json!({
        "environmentId": environment_id,
        "organizationId": eff["organizationId"], "projectId": eff["projectId"],
        "environmentKind": eff["environmentKind"], "organizationState": eff["organizationState"],
        "generatedAt": now.to_rfc3339(),
        "snapshotVersion": snap["snapshotVersion"], "snapshotDigest": snap["snapshotDigest"],
        "plan": eff["plan"], "projectPlan": eff["projectPlan"],
        "limits": limits, "warnings": warnings,
    }))
}

/// `GET /v1/environments/{id}/limits`.
pub async fn limits(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
) -> Result<Value, ApiError> {
    let (_, _, kind) =
        authorize_environment(state, caller, environment_id, Action::LimitsRead).await?;
    let (sa, cred) = match &caller.principal {
        Principal::Machine(m) => (Some(m.context.service_account_id), Some(m.credential_id)),
        Principal::Human(_) => (None, None),
    };
    limits_view(state, environment_id, Some(&kind), sa, cred).await
}

/// `GET /v1/environments/{id}/usage/warnings`.
pub async fn warnings(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
) -> Result<Value, ApiError> {
    let v = limits(state, caller, environment_id).await?;
    Ok(
        json!({ "environmentId": environment_id, "generatedAt": v["generatedAt"], "items": v["warnings"] }),
    )
}

// ---------------------------------------------------------------------------------------
// Machine compatibility: `/v1/usage` and `/v1/limits` (Calling's public paths), served by
// the Developer Platform once the API Gateway's `API_GATEWAY_CENTRAL_USAGE_ENABLED`
// re-points them (rollback: unset the flag; Calling's routes stay intact).
//
// Part VI §19.4: the paths keep their PUBLISHED contract (`cfmeet.calling.http-api`
// `listUsage` / `getLimits`, `platform/usage/v1.schema.json` `UsageList` / `Limits`), so
// existing clients and the generated SDKs keep decoding them after the switch:
//
// * `/v1/usage` projects the unified ledger's Calling-produced call-minute facts back to
//   the contract's records — `CALL_MINUTE_APP` → `CALL_CONNECTED_TIME`,
//   `CALL_MINUTE_PUBLIC_NETWORK` → `LEG_CONNECTED_TIME` (same record id, call id, leg id,
//   quantity, route class, destination country, billable seconds, cost evidence and
//   recording time as Calling's own view; same keyset order). Calling detail the ledger
//   does not meter (participant / session connected time, voicemail and recording
//   artifacts) stays at Calling's preserved service-specific path `/v1/calling/usage`.
// * `/v1/limits` reports `REQUEST_RATE` (`MINUTE`) from the effective plan entitlement the
//   gateway enforces; concurrent calls, telecom spend and the PRINCIPAL axis are Calling's
//   domain limits and stay at `/v1/calling/limits` (`principalId` is refused here).
// ---------------------------------------------------------------------------------------

fn machine(caller: &Caller) -> Result<&crate::control::MachineCaller, ApiError> {
    match &caller.principal {
        Principal::Machine(m) => Ok(m),
        Principal::Human(_) => Err(ApiError(ErrorCode::AuthenticationRequired)),
    }
}

/// `(Calling usage type, resource class, ledger usage type)` of the call facts the
/// unified ledger meters (Calling's exporter mapping, reversed).
pub const COMPAT_USAGE_TYPES: &[(&str, &str, &str)] = &[
    ("CALL_CONNECTED_TIME", "APP_MEDIA", "CALL_MINUTE_APP"),
    (
        "LEG_CONNECTED_TIME",
        "PUBLIC_NETWORK",
        "CALL_MINUTE_PUBLIC_NETWORK",
    ),
];
/// Every usage type / resource class the published contract accepts as a filter.
const CONTRACT_USAGE_TYPES: &[&str] = &[
    "CALL_CONNECTED_TIME",
    "PARTICIPANT_CONNECTED_TIME",
    "SESSION_CONNECTED_TIME",
    "LEG_CONNECTED_TIME",
    "VOICEMAIL_ARTIFACT",
    "RECORDING_ARTIFACT",
];
const CONTRACT_RESOURCE_CLASSES: &[&str] = &["APP_MEDIA", "PUBLIC_NETWORK", "STORAGE"];
/// Calling's source fact id prefix of a `call_usage_records` row.
const CALL_USAGE_SOURCE: &str = "call_usage:";

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompatUsageQuery {
    #[serde(default)]
    pub from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub usage_type: Option<String>,
    #[serde(default)]
    pub resource_class: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompatLimitsQuery {
    #[serde(default)]
    pub principal_id: Option<String>,
}

/// The ledger usage types a compatibility query selects (validated like Calling does:
/// an unknown type / class is a VALIDATION_ERROR; both filters intersect).
fn compat_types(q: &CompatUsageQuery) -> Result<Vec<&'static str>, ApiError> {
    if q.usage_type
        .as_deref()
        .is_some_and(|t| !CONTRACT_USAGE_TYPES.contains(&t))
        || q.resource_class
            .as_deref()
            .is_some_and(|c| !CONTRACT_RESOURCE_CLASSES.contains(&c))
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    Ok(COMPAT_USAGE_TYPES
        .iter()
        .filter(|(t, c, _)| {
            q.usage_type.as_deref().is_none_or(|u| u == *t)
                && q.resource_class.as_deref().is_none_or(|r| r == *c)
        })
        .map(|(_, _, ledger)| *ledger)
        .collect())
}

fn millis(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// One ledger fact as the contract's `UsageRecord` (`None`: not representable).
pub fn compat_record(
    source_fact_id: &str,
    usage_type: &str,
    quantity: i64,
    resource_ref: &str,
    occurred_at: DateTime<Utc>,
    dimensions: &Value,
) -> Option<Value> {
    let id = Uuid::parse_str(source_fact_id.strip_prefix(CALL_USAGE_SOURCE)?).ok()?;
    let (calling_type, class, _) = COMPAT_USAGE_TYPES
        .iter()
        .find(|(_, _, ledger)| *ledger == usage_type)?;
    let call_id = Uuid::parse_str(dimensions.get("callId")?.as_str()?).ok()?;
    let mut out = json!({
        "id": id, "callId": call_id, "usageType": calling_type, "resourceClass": class,
        "quantity": quantity.max(0), "unit": "MILLISECONDS", "recordedAt": millis(occurred_at),
    });
    if *calling_type == "LEG_CONNECTED_TIME" {
        let leg = Uuid::parse_str(resource_ref.strip_prefix("call_leg:")?).ok()?;
        out["legId"] = json!(leg);
        for key in ["routeClass", "destinationCountry", "billableSeconds"] {
            if let Some(v) = dimensions.get(key).filter(|v| !v.is_null()) {
                out[key] = v.clone();
            }
        }
        // Cost EVIDENCE exactly as Calling reports it to the same environment: KNOWN /
        // ESTIMATED carry the amount and currency, UNKNOWN never does.
        let status = dimensions.get("costEvidence")?.as_str()?;
        let mut cost = json!({ "status": status });
        if status != "UNKNOWN" {
            cost["amountMinor"] = dimensions.get("costMinor")?.clone();
            cost["currency"] = dimensions.get("costCurrency")?.clone();
        }
        out["cost"] = cost;
    }
    Some(out)
}

/// `GET /v1/usage` — the credential's environment's metered call facts in the published
/// `UsageList` shape (`{data, nextCursor}`), oldest first by recording time.
pub async fn compat_usage(
    state: &AppState,
    caller: &Caller,
    q: CompatUsageQuery,
) -> Result<Value, ApiError> {
    let m = machine(caller)?;
    let env = m.context.environment_id;
    authorize_environment(state, caller, env, Action::UsageRead).await?;
    if let (Some(f), Some(t)) = (q.from, q.to) {
        if f >= t {
            return Err(ApiError(ErrorCode::ValidationError));
        }
    }
    let types = compat_types(&q)?;
    let page = Page::from_query(&PageQuery {
        limit: q.limit,
        cursor: q.cursor.clone(),
    })?;
    let after = match (page.has_cursor(), page.cursor_key()) {
        (false, _) => None,
        (true, Some(k)) => Some(k),
        (true, None) => return Err(ApiError(ErrorCode::ValidationError)),
    };
    if types.is_empty() {
        return Ok(json!({ "data": [], "nextCursor": null }));
    }
    let types: Vec<String> = types.into_iter().map(str::to_string).collect();
    let (after_at, after_key) = match after {
        Some((at, id)) => (Some(at), Some(format!("{CALL_USAGE_SOURCE}{id}"))),
        None => (None, None),
    };
    let rows = state
        .db
        .query_all(stmt(
            "SELECT source_fact_id, usage_type, quantity, resource_ref, occurred_at, dimensions \
             FROM devplat_usage_facts \
             WHERE environment_id = $1 AND tenant_id = $2 AND application_id = $3 \
               AND producer = 'cfmeet.calling' AND usage_type = ANY($4) \
               AND source_fact_id LIKE 'call_usage:%' AND dimensions ? 'callId' \
               AND (usage_type <> 'CALL_MINUTE_PUBLIC_NETWORK' \
                    OR (resource_ref LIKE 'call_leg:%' AND dimensions ? 'costEvidence')) \
               AND ($5::timestamptz IS NULL OR occurred_at >= $5) \
               AND ($6::timestamptz IS NULL OR occurred_at < $6) \
               AND ($7::timestamptz IS NULL OR (occurred_at, source_fact_id) > ($7, $8::text)) \
             ORDER BY occurred_at, source_fact_id LIMIT $9",
            vec![
                env.into(),
                m.context.tenant_id.into(),
                m.context.application_id.into(),
                types.into(),
                q.from.into(),
                q.to.into(),
                after_at.into(),
                after_key.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .await?;
    let mut data = Vec::new();
    let mut last: Option<(DateTime<Utc>, Uuid)> = None;
    for r in rows.iter().take(page.limit) {
        let source: String = r.try_get("", "source_fact_id")?;
        let at: DateTime<Utc> = r.try_get("", "occurred_at")?;
        if let Some(id) = source
            .strip_prefix(CALL_USAGE_SOURCE)
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            last = Some((at, id));
        }
        match compat_record(
            &source,
            &r.try_get::<String>("", "usage_type")?,
            r.try_get("", "quantity")?,
            &r.try_get::<String>("", "resource_ref")?,
            at,
            &r.try_get::<Value>("", "dimensions")?,
        ) {
            Some(v) => data.push(v),
            None => {
                tracing::warn!(source_fact_id = %source, "ledger call fact not representable in the /v1/usage contract")
            }
        }
    }
    let next = (rows.len() > page.limit)
        .then(|| last.map(|(at, id)| Page::key_cursor(at, id)))
        .flatten();
    Ok(json!({ "data": data, "nextCursor": next }))
}

/// The contract's `LimitLevel.scope` of an entitlement resolution level.
fn compat_scope(level: &str) -> &'static str {
    match level {
        "CREDENTIAL" => "CREDENTIAL",
        "SERVICE_ACCOUNT" => "SERVICE_ACCOUNT",
        "ENVIRONMENT" => "ENVIRONMENT",
        "PROJECT_PLAN" => "APPLICATION",
        "ORGANIZATION_PLAN" | "OPERATOR_OVERRIDE" => "TENANT",
        _ => "PLATFORM_DEFAULT",
    }
}

/// `GET /v1/limits` — the published `Limits` shape: the request rate the gateway enforces
/// for this credential (effective plan entitlement; current = API operations the ledger
/// recorded in the current minute, which can trail the gateway's live counter by the
/// producer's export interval).
pub async fn compat_limits(
    state: &AppState,
    caller: &Caller,
    q: CompatLimitsQuery,
) -> Result<Value, ApiError> {
    let m = machine(caller)?;
    if q.principal_id.is_some() {
        // The PRINCIPAL axis is Calling's (`GET /v1/calling/limits?principalId=`).
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let env = m.context.environment_id;
    authorize_environment(state, caller, env, Action::LimitsRead).await?;
    let eff = entitlements::effective_for(
        &state.db,
        env,
        Some(&m.context.environment_kind),
        Some(m.context.service_account_id),
        Some(m.credential_id),
    )
    .await?;
    let now = Utc::now();
    let minute = now
        .date_naive()
        .and_hms_opt(now.time().hour(), now.time().minute(), 0)
        .expect("minute")
        .and_utc();
    let current: i64 = state
        .db
        .query_one(stmt(
            "SELECT COALESCE(SUM(quantity), 0)::bigint AS n FROM devplat_usage_facts \
             WHERE environment_id = $1 AND usage_type = 'API_OPERATION' AND occurred_at >= $2",
            vec![env.into(), minute.into()],
        ))
        .await?
        .map(|r| r.try_get("", "n"))
        .transpose()?
        .unwrap_or(0);
    let ceiling = eff["apiRequestsPerMinute"].as_i64().unwrap_or(0).max(0);
    let scope = compat_scope(
        eff["limitingLevel"]["apiRequestsPerMinute"]
            .as_str()
            .unwrap_or("PLATFORM_DEFAULT"),
    );
    let remaining = (ceiling - current).max(0);
    Ok(json!({
        "generatedAt": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "limits": [{
            "limit": "REQUEST_RATE", "window": "MINUTE",
            "resetsAt": millis(minute + Duration::minutes(1)),
            "scope": scope, "ceiling": ceiling, "current": current, "remaining": remaining,
            "levels": [{ "scope": scope, "ceiling": ceiling, "current": current, "remaining": remaining }],
        }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_trigger_at_80_and_100_percent() {
        assert!(warning("X", 79, 100).is_none());
        assert_eq!(warning("X", 80, 100).unwrap()["level"], "WARNING");
        assert_eq!(warning("X", 100, 100).unwrap()["level"], "EXHAUSTED");
        assert_eq!(warning("X", 0, 0).unwrap()["level"], "EXHAUSTED");
    }

    #[test]
    fn csv_cells_are_quoted_and_formula_safe() {
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_field("=cmd"), "\"'=cmd\"");
        assert_eq!(snake("smsPerDay"), "SMS_PER_DAY");
    }

    #[test]
    fn periods_default_to_the_month_and_are_bounded() {
        let (f, t) = period(&UsageQuery::default()).unwrap();
        assert_eq!(f.day(), 1);
        assert!(t >= f);
        let q = UsageQuery {
            from: Some("2026-01-01".into()),
            to: Some("2027-12-31".into()),
            ..Default::default()
        };
        assert!(period(&q).is_err());
    }
}
