//! Calling reads (design §6.4), served from Calling's tenant-scoped operator reads
//! (`calling.administer` + `X-CF-Operator: hq:<sub>`).
//!
//! Every read is scoped to the request's `tenant` (a Platform Access tenant id): another
//! tenant's data is never reached (design §16). A tenant that is not a UUID names nothing:
//! `RESOURCE_NOT_FOUND` without an owner call. Translation keeps ids, codes, states,
//! counts, durations, sizes and times only; owner cursors are checked here.

use serde_json::{json, Value};

use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::Calling;
const BASE: &str = "/internal/v1/operator";

fn rfc(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Calling's cursors: `<micros>_<32 hex>`.
fn cursor_ok(c: &str) -> bool {
    c.split_once('_').is_some_and(|(m, id)| {
        !m.is_empty()
            && m.len() <= 20
            && m.bytes().all(|b| b.is_ascii_digit())
            && id.len() == 32
            && id.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn tenant(auth: &Authorized) -> Result<String, ErrorCode> {
    uuid::Uuid::parse_str(&auth.query.tenant)
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

fn opt_nonneg(v: &Value, k: &str) -> T<Value> {
    match t::opt_int(v, k)? {
        Value::Null => Ok(Value::Null),
        n if n.as_i64().is_some_and(|x| x >= 0) => Ok(n),
        _ => Err(OwnerError::Contract),
    }
}

fn strings(v: &Value, k: &str, max: usize) -> T<Value> {
    let arr = v
        .get(k)
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if arr.len() > max {
        return Err(OwnerError::Contract);
    }
    arr.iter()
        .map(|x| {
            x.as_str()
                .map(|s| Value::String(s.to_string()))
                .ok_or(OwnerError::Contract)
        })
        .collect::<T<Vec<_>>>()
        .map(Value::Array)
}

pub fn call(c: &Value) -> T<Value> {
    Ok(obj(vec![
        ("call_id", t::uuid(c, "callId")?),
        ("application_id", t::uuid(c, "applicationId")?),
        ("environment_id", t::uuid(c, "environmentId")?),
        ("call_kind", t::string(c, "callKind")?),
        ("state", t::string(c, "state")?),
        ("origin", t::string(c, "origin")?),
        ("participant_count", t::int(c, "participantCount")?),
        ("terminal_reason", t::opt_string(c, "terminalReason")?),
        ("failure_category", t::opt_string(c, "failureCategory")?),
        ("requested_at", t::opt_time(c, "requestedAt")?),
        ("answered_at", t::opt_time(c, "answeredAt")?),
        ("connected_at", t::opt_time(c, "connectedAt")?),
        ("ended_at", t::opt_time(c, "endedAt")?),
        ("created_at", t::time(c, "createdAt")?),
        ("version", t::int(c, "version")?),
    ]))
}

fn participant(p: &Value) -> T<Value> {
    Ok(obj(vec![
        ("principal_id", t::uuid(p, "principalId")?),
        ("principal_kind", t::string(p, "principalKind")?),
        ("role", t::string(p, "role")?),
        ("is_host", t::boolean(p, "isHost")?),
        ("joined_at", t::time(p, "joinedAt")?),
        ("left_at", t::opt_time(p, "leftAt")?),
    ]))
}

fn call_detail(c: &Value) -> T<Value> {
    let mut v = call(c)?;
    let parts = c
        .get("participants")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if parts.len() > 500 {
        return Err(OwnerError::Contract);
    }
    v["participants"] = Value::Array(parts.iter().map(participant).collect::<T<Vec<_>>>()?);
    Ok(v)
}

fn abuse_report(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("abuse_report_id", t::uuid(r, "abuseReportId")?),
        ("application_id", t::uuid(r, "applicationId")?),
        ("environment_id", t::uuid(r, "environmentId")?),
        ("call_id", t::uuid(r, "callId")?),
        (
            "reporting_principal_id",
            t::uuid(r, "reportingPrincipalId")?,
        ),
        (
            "reported_principal_id",
            t::opt_uuid(r, "reportedPrincipalId")?,
        ),
        ("category", t::string(r, "category")?),
        ("comment_code", t::opt_string(r, "commentCode")?),
        ("status", t::string(r, "status")?),
        ("created_at", t::time(r, "createdAt")?),
        ("reviewed_at", t::opt_time(r, "reviewedAt")?),
        ("version", t::int(r, "version")?),
    ]))
}

fn block_rule(b: &Value) -> T<Value> {
    Ok(obj(vec![
        ("block_rule_id", t::uuid(b, "blockRuleId")?),
        ("application_id", t::uuid(b, "applicationId")?),
        ("environment_id", t::uuid(b, "environmentId")?),
        ("blocker_kind", t::string(b, "blockerKind")?),
        (
            "blocker_principal_id",
            t::opt_uuid(b, "blockerPrincipalId")?,
        ),
        ("blocked_kind", t::string(b, "blockedKind")?),
        (
            "blocked_principal_id",
            t::opt_uuid(b, "blockedPrincipalId")?,
        ),
        (
            "has_contact_reference",
            t::boolean(b, "hasContactReference")?,
        ),
        ("reason_code", t::opt_string(b, "reasonCode")?),
        ("status", t::string(b, "status")?),
        ("created_at", t::time(b, "createdAt")?),
        ("released_at", t::opt_time(b, "releasedAt")?),
        ("version", t::int(b, "version")?),
    ]))
}

fn limit_fields(l: &Value) -> T<Vec<(&'static str, Value)>> {
    Ok(vec![
        ("requests_per_minute", t::opt_int(l, "requestsPerMinute")?),
        (
            "max_concurrent_communications",
            t::opt_int(l, "maxConcurrentCommunications")?,
        ),
        (
            "telecom_spend_daily_limit_minor",
            t::opt_int(l, "telecomSpendDailyLimitMinor")?,
        ),
        (
            "telecom_spend_monthly_limit_minor",
            t::opt_int(l, "telecomSpendMonthlyLimitMinor")?,
        ),
        ("updated_at", t::time(l, "updatedAt")?),
    ])
}

fn limits(v: &Value) -> T<Value> {
    let tenant_policy = match v.get("tenant") {
        None | Some(Value::Null) => Value::Null,
        Some(l) => obj(limit_fields(l)?),
    };
    let apps = v
        .get("applications")
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if apps.len() > 100 {
        return Err(OwnerError::Contract);
    }
    let applications = apps
        .iter()
        .map(|a| {
            let mut f = vec![("application_id", t::uuid(a, "applicationId")?)];
            f.extend(limit_fields(a)?);
            Ok(obj(f))
        })
        .collect::<T<Vec<_>>>()?;
    Ok(obj(vec![
        ("tenant_id", t::uuid(v, "tenantId")?),
        ("currency", t::string(v, "currency")?),
        ("tenant", tenant_policy),
        ("applications", Value::Array(applications)),
        ("truncated", t::boolean(v, "truncated")?),
    ]))
}

fn routing_policy(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("environment_id", t::uuid(r, "environmentId")?),
        ("application_id", t::uuid(r, "applicationId")?),
        ("revision", t::int(r, "revision")?),
        ("updated_at", t::time(r, "updatedAt")?),
        (
            "allowed_route_classes",
            strings(r, "allowedRouteClasses", 32)?,
        ),
        ("app_first", opt_bool(r, "appFirst")?),
        (
            "public_calling_enabled",
            opt_bool(r, "publicCallingEnabled")?,
        ),
        (
            "callback_relay_enabled",
            opt_bool(r, "callbackRelayEnabled")?,
        ),
        (
            "require_verified_contacts",
            opt_bool(r, "requireVerifiedContacts")?,
        ),
        (
            "max_failover_attempts",
            opt_nonneg(r, "maxFailoverAttempts")?,
        ),
        (
            "compliance_required_countries",
            strings(r, "complianceRequiredCountries", 300)?,
        ),
    ]))
}

fn enterprise_policy(e: &Value) -> T<Value> {
    Ok(obj(vec![
        ("policy_id", t::uuid(e, "policyId")?),
        ("environment_id", t::uuid(e, "environmentId")?),
        ("application_id", t::uuid(e, "applicationId")?),
        ("name", t::string(e, "name")?),
        ("is_default", t::boolean(e, "isDefault")?),
        ("status", t::string(e, "status")?),
        (
            "internal_calling_enabled",
            t::boolean(e, "internalCallingEnabled")?,
        ),
        (
            "external_public_calling_allowed",
            t::boolean(e, "externalPublicCallingAllowed")?,
        ),
        (
            "public_destination_countries",
            strings(e, "publicDestinationCountries", 300)?,
        ),
        ("forwarding_allowed", t::boolean(e, "forwardingAllowed")?),
        (
            "public_forwarding_allowed",
            t::boolean(e, "publicForwardingAllowed")?,
        ),
        ("voicemail_allowed", t::boolean(e, "voicemailAllowed")?),
        ("transfer_allowed", t::boolean(e, "transferAllowed")?),
        ("admin_capabilities", strings(e, "adminCapabilities", 32)?),
        ("version", t::int(e, "version")?),
        ("updated_at", t::time(e, "updatedAt")?),
        ("disabled_at", t::opt_time(e, "disabledAt")?),
    ]))
}

fn spend_policy(s: &Value) -> T<Value> {
    Ok(obj(vec![
        ("environment_id", t::uuid(s, "environmentId")?),
        ("application_id", t::uuid(s, "applicationId")?),
        ("currency", t::string(s, "currency")?),
        ("enabled", t::boolean(s, "enabled")?),
        ("leg_allowance_minor", t::opt_int(s, "legAllowanceMinor")?),
        ("max_leg_seconds", t::opt_int(s, "maxLegSeconds")?),
        ("daily_limit_minor", t::opt_int(s, "dailyLimitMinor")?),
        ("monthly_limit_minor", t::opt_int(s, "monthlyLimitMinor")?),
        ("max_concurrent_legs", t::opt_int(s, "maxConcurrentLegs")?),
        (
            "max_originations_per_minute",
            t::opt_int(s, "maxOriginationsPerMinute")?,
        ),
        ("revision", t::int(s, "revision")?),
        ("updated_at", t::time(s, "updatedAt")?),
    ]))
}

fn recording(r: &Value) -> T<Value> {
    Ok(obj(vec![
        ("recording_id", t::uuid(r, "recordingId")?),
        ("call_id", t::uuid(r, "callId")?),
        ("environment_id", t::uuid(r, "environmentId")?),
        ("state", t::string(r, "state")?),
        ("capture_source", t::string(r, "captureSource")?),
        ("duration_ms", opt_nonneg(r, "durationMs")?),
        ("size_bytes", opt_nonneg(r, "sizeBytes")?),
        ("retention_until", t::opt_time(r, "retentionUntil")?),
        ("created_at", t::time(r, "createdAt")?),
        ("deleted_at", t::opt_time(r, "deletedAt")?),
    ]))
}

fn voicemail(v: &Value) -> T<Value> {
    Ok(obj(vec![
        ("message_id", t::uuid(v, "messageId")?),
        ("call_id", t::opt_uuid(v, "callId")?),
        ("mailbox_id", t::uuid(v, "mailboxId")?),
        ("environment_id", t::uuid(v, "environmentId")?),
        ("state", t::string(v, "state")?),
        ("read_state", t::string(v, "readState")?),
        ("duration_ms", opt_nonneg(v, "durationMs")?),
        ("size_bytes", opt_nonneg(v, "sizeBytes")?),
        ("retention_until", t::opt_time(v, "retentionUntil")?),
        ("received_at", t::opt_time(v, "receivedAt")?),
        ("deleted_at", t::opt_time(v, "deletedAt")?),
    ]))
}

fn retention_hold(h: &Value) -> T<Value> {
    Ok(obj(vec![
        ("hold_id", t::uuid(h, "holdId")?),
        ("scope", t::string(h, "scope")?),
        ("data_class", t::string(h, "dataClass")?),
        ("resource_id", t::opt_uuid(h, "resourceId")?),
        ("environment_id", t::opt_uuid(h, "environmentId")?),
        ("reason_code", t::string(h, "reasonCode")?),
        ("active", t::boolean(h, "active")?),
        ("created_at", t::time(h, "createdAt")?),
        ("released_at", t::opt_time(h, "releasedAt")?),
    ]))
}

fn incident(i: &Value) -> T<Value> {
    Ok(obj(vec![
        ("incident_id", t::uuid(i, "incidentId")?),
        ("scope", t::string(i, "scope")?),
        ("category", t::string(i, "category")?),
        ("severity", t::string(i, "severity")?),
        ("status", t::string(i, "status")?),
        ("call_id", t::opt_uuid(i, "callId")?),
        ("resource_type", t::opt_string(i, "resourceType")?),
        ("resource_id", t::opt_uuid(i, "resourceId")?),
        ("occurrences", t::int(i, "occurrences")?),
        ("first_seen_at", t::time(i, "firstSeenAt")?),
        ("last_seen_at", t::time(i, "lastSeenAt")?),
        ("resolved_at", t::opt_time(i, "resolvedAt")?),
    ]))
}

fn webhook_key(k: &Value) -> T<Value> {
    Ok(obj(vec![
        ("key_id", t::opt_string(k, "keyId")?),
        ("subscriptions", t::int(k, "subscriptions")?),
        ("active_subscriptions", t::int(k, "activeSubscriptions")?),
        ("last_updated_at", t::opt_time(k, "lastUpdatedAt")?),
    ]))
}

fn audit_entry(a: &Value) -> T<Value> {
    Ok(obj(vec![
        ("event_id", t::uuid(a, "eventId")?),
        ("actor", t::string(a, "actor")?),
        ("action", t::string(a, "action")?),
        ("target_kind", t::string(a, "targetKind")?),
        ("target_id", t::opt_uuid(a, "targetId")?),
        ("application_id", t::opt_uuid(a, "applicationId")?),
        ("environment_id", t::opt_uuid(a, "environmentId")?),
        ("occurred_at", t::time(a, "occurredAt")?),
    ]))
}

/// A bounded configuration list `{<key>: [...], truncated}`.
fn bounded(v: &Value, key: &str, item: fn(&Value) -> T<Value>) -> T<Value> {
    let arr = v
        .get(key)
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    if arr.len() > 100 {
        return Err(OwnerError::Contract);
    }
    let items = arr.iter().map(item).collect::<T<Vec<_>>>()?;
    let mut out = serde_json::Map::new();
    out.insert(key.to_string(), Value::Array(items));
    out.insert("truncated".into(), t::boolean(v, "truncated")?);
    Ok(Value::Object(out))
}

enum Shape {
    Page(fn(&Value) -> T<Value>),
    One(fn(&Value) -> T<Value>),
}

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    let (path, windowed, shape) = match auth.capability.id {
        "calling.calls.list" => (format!("{BASE}/calls"), true, Shape::Page(call)),
        "calling.calls.read" => {
            let id = match auth.params.get("call_id").map(|c| uuid::Uuid::parse_str(c)) {
                Some(Ok(u)) => u.hyphenated().to_string(),
                Some(Err(_)) => return Some(Err(ErrorCode::ResourceNotFound)),
                None => return Some(Err(ErrorCode::Internal)),
            };
            (format!("{BASE}/calls/{id}"), false, Shape::One(call_detail))
        }
        "calling.live.list" => (format!("{BASE}/live-calls"), false, Shape::Page(call)),
        "calling.abuse-reports.list" => (
            format!("{BASE}/abuse-reports"),
            false,
            Shape::Page(abuse_report),
        ),
        "calling.blocks.list" => (format!("{BASE}/blocks"), false, Shape::Page(block_rule)),
        "calling.limits.read" => (format!("{BASE}/limits"), false, Shape::One(limits)),
        "calling.routing.read" => (
            format!("{BASE}/routing-policies"),
            false,
            Shape::One(|v| bounded(v, "environments", routing_policy)),
        ),
        "calling.enterprise-features.read" => (
            format!("{BASE}/enterprise-policies"),
            false,
            Shape::One(|v| bounded(v, "policies", enterprise_policy)),
        ),
        "calling.spend-limits.read" => (
            format!("{BASE}/spend-policies"),
            false,
            Shape::One(|v| bounded(v, "environments", spend_policy)),
        ),
        "calling.recordings.list" => (format!("{BASE}/recordings"), true, Shape::Page(recording)),
        "calling.voicemail.list" => (format!("{BASE}/voicemail"), true, Shape::Page(voicemail)),
        "calling.retention-holds.list" => (
            format!("{BASE}/retention-holds"),
            false,
            Shape::Page(retention_hold),
        ),
        "calling.incidents.list" => (format!("{BASE}/incidents"), true, Shape::Page(incident)),
        "calling.webhook-keys.list" => (
            format!("{BASE}/webhook-keys"),
            false,
            Shape::Page(webhook_key),
        ),
        "calling.audit.list" => (format!("{BASE}/audit"), true, Shape::Page(audit_entry)),
        _ => return None,
    };
    let tenant = match tenant(auth) {
        Ok(t) => t,
        Err(e) => return Some(Err(e)),
    };
    let q = &auth.query;
    if q.cursor.as_deref().is_some_and(|c| !cursor_ok(c)) {
        return Some(Err(ErrorCode::QueryInvalid));
    }
    let mut query: Vec<(&'static str, String)> = vec![("tenantId", tenant)];
    if let Shape::Page(_) = shape {
        query.extend(q.limit.map(|l| ("limit", l.to_string())));
        query.extend(q.cursor.clone().map(|c| ("cursor", c)));
    }
    if windowed {
        query.extend(q.start.map(|s| ("since", rfc(s))));
        query.extend(q.end.map(|e| ("until", rfc(e))));
    }
    let operator = format!("hq:{}", auth.staff.claims.sub);
    let result = match bridge.owners.read(O, &path, &query, &operator).await {
        Ok(v) => v,
        Err(e) => return Some(Err(e.into())),
    };
    let translated = match shape {
        Shape::Page(item) => {
            t::page(&result, item).map(|(items, next)| json!({"items": items, "next_cursor": next}))
        }
        Shape::One(f) => f(&result),
    };
    Some(
        translated
            .map(|result| Answer {
                result,
                freshness: "LIVE",
            })
            .map_err(ErrorCode::from),
    )
}
