//! Partner and app reads (design §6.7 / §6.9), served from the Developer Platform's
//! operator routes (`developer-platform.administer` + `X-CF-Operator: hq:<sub>`).
//!
//! Translation keeps identifiers, names, states, counts and timestamps only: the
//! workspace support address, invitee addresses, credential material, webhook URL paths,
//! manifest URLs, test accounts and command summaries are never relayed.

use serde_json::{json, Value};

use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::DeveloperPlatform;
const BASE: &str = "/internal/v1/operator";

fn uuid_param(auth: &Authorized, name: &str) -> Result<String, ErrorCode> {
    let raw = auth.params.get(name).ok_or(ErrorCode::Internal)?;
    // The owner keys workspaces and apps by UUID: any other id names nothing.
    uuid::Uuid::parse_str(raw)
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| ErrorCode::ResourceNotFound)
}

fn rfc(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A calendar day `YYYY-MM-DD`, strictly.
pub fn day(v: &Value, k: &str) -> T<Value> {
    let s = v
        .get(k)
        .and_then(Value::as_str)
        .ok_or(OwnerError::Contract)?;
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map(|d| Value::String(d.format("%Y-%m-%d").to_string()))
        .map_err(|_| OwnerError::Contract)
}

fn listing(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "next_cursor": next})
}

fn workspace(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("workspace_id", t::uuid(o, "id")?),
        ("display_name", t::string(o, "displayName")?),
        ("state", t::string(o, "state")?),
        ("state_reason", t::opt_string(o, "stateReason")?),
        ("created_at", t::time(o, "createdAt")?),
        ("updated_at", t::time(o, "updatedAt")?),
        ("closed_at", t::opt_time(o, "closedAt")?),
        ("version", t::int(o, "version")?),
    ]))
}

fn workspace_detail(o: &Value) -> T<Value> {
    let mut v = workspace(o)?;
    v["active_members"] = t::int(o, "activeMembers")?;
    v["projects"] = t::int(o, "projects")?;
    Ok(v)
}

fn activity(e: &Value) -> T<Value> {
    Ok(obj(vec![
        ("event_id", t::uuid(e, "id")?),
        ("scope", t::string(e, "scope")?),
        ("workspace_id", t::opt_uuid(e, "organizationId")?),
        ("operator", t::string(e, "operator")?),
        ("action", t::string(e, "action")?),
        ("target_type", t::opt_string(e, "targetType")?),
        ("target_id", t::opt_string(e, "targetId")?),
        ("outcome", t::string(e, "outcome")?),
        ("occurred_at", t::time(e, "occurredAt")?),
    ]))
}

fn project(p: &Value) -> T<Value> {
    Ok(obj(vec![
        ("project_id", t::uuid(p, "projectId")?),
        ("display_name", t::string(p, "displayName")?),
        ("status", t::string(p, "status")?),
        ("status_reason", t::opt_string(p, "statusReason")?),
        ("environments", t::int(p, "environments")?),
        ("created_at", t::time(p, "createdAt")?),
        ("updated_at", t::time(p, "updatedAt")?),
        ("archived_at", t::opt_time(p, "archivedAt")?),
        ("version", t::int(p, "version")?),
    ]))
}

fn environment(e: &Value) -> T<Value> {
    Ok(obj(vec![
        ("environment_id", t::uuid(e, "environmentId")?),
        ("project_id", t::uuid(e, "projectId")?),
        ("kind", t::string(e, "kind")?),
        ("display_name", t::string(e, "displayName")?),
        ("status", t::string(e, "status")?),
        (
            "production_access_state",
            t::string(e, "productionAccessState")?,
        ),
        ("created_at", t::time(e, "createdAt")?),
        ("updated_at", t::time(e, "updatedAt")?),
        ("version", t::int(e, "version")?),
    ]))
}

fn member(m: &Value) -> T<Value> {
    Ok(obj(vec![
        ("membership_id", t::uuid(m, "membershipId")?),
        ("user_id", t::uuid(m, "userId")?),
        ("role", t::string(m, "role")?),
        ("status", t::string(m, "status")?),
        ("joined_via", t::string(m, "joinedVia")?),
        ("created_at", t::time(m, "createdAt")?),
        ("updated_at", t::time(m, "updatedAt")?),
        ("removed_at", t::opt_time(m, "removedAt")?),
        ("version", t::int(m, "version")?),
    ]))
}

fn invitation(i: &Value) -> T<Value> {
    Ok(obj(vec![
        ("invitation_id", t::uuid(i, "invitationId")?),
        ("role", t::string(i, "role")?),
        ("status", t::string(i, "status")?),
        ("created_at", t::time(i, "createdAt")?),
        ("expires_at", t::time(i, "expiresAt")?),
        ("decided_at", t::opt_time(i, "decidedAt")?),
    ]))
}

fn push_key(p: &Value) -> T<Value> {
    Ok(obj(vec![
        ("push_key_id", t::uuid(p, "pushProviderId")?),
        ("project_id", t::uuid(p, "projectId")?),
        ("environment_id", t::uuid(p, "environmentId")?),
        ("environment_kind", t::string(p, "environmentKind")?),
        ("provider", t::string(p, "provider")?),
        ("key_id", t::opt_string(p, "apnsKeyId")?),
        ("team_id", t::opt_string(p, "apnsTeamId")?),
        ("bundle_id", t::opt_string(p, "apnsBundleId")?),
        ("apns_host", t::opt_string(p, "apnsHost")?),
        ("fcm_project", t::opt_string(p, "fcmProjectId")?),
        ("fingerprint", t::string(p, "fingerprint")?),
        ("verification", t::string(p, "verification")?),
        ("last_verified_at", t::opt_time(p, "lastVerifiedAt")?),
        ("last_error_code", t::opt_string(p, "lastErrorCode")?),
        ("created_at", t::time(p, "createdAt")?),
        ("updated_at", t::time(p, "updatedAt")?),
        ("state", t::string(p, "state")?),
        ("disabled_at", t::opt_time(p, "disabledAt")?),
        ("version", t::int(p, "version")?),
    ]))
}

fn usage(u: &Value) -> T<Value> {
    Ok(obj(vec![
        ("project_id", t::uuid(u, "projectId")?),
        ("environment_id", t::uuid(u, "environmentId")?),
        ("environment_kind", t::string(u, "environmentKind")?),
        ("usage_type", t::string(u, "usageType")?),
        ("unit", t::string(u, "unit")?),
        ("quantity", t::int(u, "quantity")?),
        ("fact_count", t::int(u, "factCount")?),
    ]))
}

fn incident(i: &Value) -> T<Value> {
    Ok(obj(vec![
        ("incident_record_id", t::uuid(i, "incidentRecordId")?),
        ("workspace_id", t::uuid(i, "organizationId")?),
        ("incident_ref", t::opt_string(i, "incidentRef")?),
        ("occurred_at", t::opt_time(i, "occurredAt")?),
        ("recorded_at", t::time(i, "recordedAt")?),
    ]))
}

fn string_list(v: &Value, k: &str) -> T<Value> {
    let items = v
        .get(k)
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    items
        .iter()
        .map(|x| {
            x.as_str()
                .map(|s| Value::String(s.to_string()))
                .ok_or(OwnerError::Contract)
        })
        .collect::<T<Vec<_>>>()
        .map(Value::Array)
}

fn webhook(w: &Value) -> T<Value> {
    Ok(obj(vec![
        ("subscription_id", t::uuid(w, "subscriptionId")?),
        ("project_id", t::uuid(w, "projectId")?),
        ("environment_id", t::uuid(w, "environmentId")?),
        ("environment_kind", t::string(w, "environmentKind")?),
        ("endpoint_host", t::opt_string(w, "endpointHost")?),
        ("event_types", string_list(w, "eventTypes")?),
        ("status", t::string(w, "status")?),
        ("consecutive_failures", t::int(w, "consecutiveFailures")?),
        ("last_success_at", t::opt_time(w, "lastSuccessAt")?),
        ("last_failure_at", t::opt_time(w, "lastFailureAt")?),
        ("disabled_reason", t::opt_string(w, "disabledReason")?),
        ("origin", t::string(w, "origin")?),
        ("created_at", t::time(w, "createdAt")?),
        ("updated_at", t::time(w, "updatedAt")?),
    ]))
}

fn delivery(d: &Value) -> T<Value> {
    Ok(obj(vec![
        ("delivery_id", t::uuid(d, "deliveryId")?),
        ("subscription_id", t::uuid(d, "subscriptionId")?),
        ("event_id", t::string(d, "eventId")?),
        ("environment_id", t::uuid(d, "environmentId")?),
        ("status", t::string(d, "status")?),
        ("attempt_count", t::int(d, "attemptCount")?),
        ("last_http_status", t::opt_int(d, "lastHttpStatus")?),
        ("failure_category", t::opt_string(d, "failureCategory")?),
        ("created_at", t::time(d, "createdAt")?),
        ("completed_at", t::opt_time(d, "completedAt")?),
    ]))
}

pub fn app_summary(a: &Value) -> T<Value> {
    Ok(obj(vec![
        ("app_id", t::uuid(a, "appId")?),
        ("workspace_id", t::uuid(a, "organizationId")?),
        ("name", t::opt_string(a, "name")?),
        ("distribution", t::opt_string(a, "distribution")?),
        ("status", t::string(a, "status")?),
        ("current_version", t::int(a, "currentVersion")?),
        ("published_version", t::opt_int(a, "publishedVersion")?),
        ("published_at", t::opt_time(a, "publishedAt")?),
        ("created_at", t::time(a, "createdAt")?),
        ("updated_at", t::time(a, "updatedAt")?),
    ]))
}

/// The owner's operator view of one app -> `AppDetail` (name and distribution from the
/// current manifest; no manifest URLs, test accounts or secret metadata).
fn app_detail(a: &Value) -> T<Value> {
    let manifest = a.get("manifest").cloned().unwrap_or(Value::Null);
    let from_manifest = |k: &str| -> T<Value> {
        if manifest.is_null() {
            Ok(Value::Null)
        } else {
            t::opt_string(&manifest, k)
        }
    };
    let review = match a.get("review") {
        None | Some(Value::Null) => Value::Null,
        Some(r) => obj(vec![
            ("reason_code", t::opt_string(r, "reasonCode")?),
            ("decided_at", t::time(r, "decidedAt")?),
        ]),
    };
    let history = match a.get("history") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(h)) => h
            .iter()
            .rev()
            .take(100)
            .map(|e| {
                Ok(obj(vec![
                    ("from_status", t::opt_string(e, "fromStatus")?),
                    ("to_status", t::string(e, "toStatus")?),
                    ("version", t::int(e, "version")?),
                    ("actor_kind", t::string(e, "actorKind")?),
                    ("reason_code", t::opt_string(e, "reasonCode")?),
                    ("occurred_at", t::time(e, "occurredAt")?),
                ]))
            })
            .collect::<T<Vec<_>>>()?,
        Some(_) => return Err(OwnerError::Contract),
    };
    Ok(obj(vec![
        ("app_id", t::uuid(a, "appId")?),
        ("workspace_id", t::uuid(a, "organizationId")?),
        ("name", from_manifest("name")?),
        ("distribution", from_manifest("distribution")?),
        ("status", t::string(a, "status")?),
        ("current_version", t::int(a, "currentVersion")?),
        ("published_version", t::opt_int(a, "publishedVersion")?),
        ("published_at", t::opt_time(a, "publishedAt")?),
        ("created_at", t::time(a, "createdAt")?),
        ("updated_at", t::time(a, "updatedAt")?),
        ("submitted_version", t::opt_int(a, "submittedVersion")?),
        ("reviewed_version", t::opt_int(a, "reviewedVersion")?),
        ("listed_in_directory", t::boolean(a, "listedInDirectory")?),
        ("developer_name", t::opt_string(a, "developerName")?),
        ("review", review),
        ("history", Value::Array(history)),
    ]))
}

fn app_version(v: &Value) -> T<Value> {
    Ok(obj(vec![
        ("version", t::int(v, "version")?),
        ("name", t::opt_string(v, "name")?),
        ("distribution", t::opt_string(v, "distribution")?),
        ("created_at", t::time(v, "createdAt")?),
    ]))
}

fn app_install(i: &Value) -> T<Value> {
    Ok(obj(vec![
        ("grant_id", t::uuid(i, "grantId")?),
        ("target_kind", t::string(i, "targetKind")?),
        ("organization_id", t::opt_uuid(i, "organizationId")?),
        ("principal_id", t::opt_uuid(i, "principalId")?),
        ("scope_count", t::int(i, "scopeCount")?),
        ("status", t::string(i, "status")?),
        ("revoked_reason", t::opt_string(i, "revokedReason")?),
        ("installed_at", t::time(i, "installedAt")?),
        ("updated_at", t::time(i, "updatedAt")?),
    ]))
}

type Item = fn(&Value) -> T<Value>;

/// What one capability reads: owner path (relative to the operator base), the page item
/// translation (`None`: a single object, translated by `single`) and the window names.
enum Read {
    Page {
        path: String,
        item: Item,
        window: bool,
        search: Option<&'static str>,
    },
    Single {
        path: String,
        translate: Item,
    },
}

fn plan(auth: &Authorized) -> Option<Result<Read, ErrorCode>> {
    let ws = || uuid_param(auth, "workspace_id");
    let app = || uuid_param(auth, "app_id");
    let page = |path: String, item: Item| Read::Page {
        path,
        item,
        window: false,
        search: None,
    };
    Some((|| -> Result<Read, ErrorCode> {
        Ok(match auth.capability.id {
            "partners.workspaces.search" => Read::Page {
                path: format!("{BASE}/organizations"),
                item: workspace,
                window: false,
                search: Some("query"),
            },
            "partners.workspaces.read" => Read::Single {
                path: format!("{BASE}/organizations/{}", ws()?),
                translate: workspace_detail,
            },
            "partners.workspace-activity.list" => Read::Page {
                path: format!("{BASE}/activity"),
                item: activity,
                window: true,
                search: None,
            },
            "partners.projects.list" => {
                page(format!("{BASE}/organizations/{}/projects", ws()?), project)
            }
            "partners.environments.list" => page(
                format!("{BASE}/organizations/{}/environments", ws()?),
                environment,
            ),
            "partners.members.list" => {
                page(format!("{BASE}/organizations/{}/members", ws()?), member)
            }
            "partners.invitations.list" => page(
                format!("{BASE}/organizations/{}/invitations", ws()?),
                invitation,
            ),
            "partners.push-keys.list" => page(
                format!("{BASE}/organizations/{}/push-providers", ws()?),
                push_key,
            ),
            "partners.webhooks.list" => {
                page(format!("{BASE}/organizations/{}/webhooks", ws()?), webhook)
            }
            "partners.usage.read" => Read::Page {
                path: format!("{BASE}/organizations/{}/usage", ws()?),
                item: usage,
                window: true,
                search: None,
            },
            "partners.incidents.list" => Read::Page {
                path: format!("{BASE}/incidents"),
                item: incident,
                window: true,
                search: None,
            },
            "partners.webhook-deliveries.list" => Read::Page {
                path: format!("{BASE}/organizations/{}/webhook-deliveries", ws()?),
                item: delivery,
                window: true,
                search: None,
            },
            "apps.apps.search" => Read::Page {
                path: format!("{BASE}/app-search"),
                item: app_summary,
                window: false,
                search: Some("q"),
            },
            "apps.apps.read" => Read::Single {
                path: format!("{BASE}/apps/{}", app()?),
                translate: app_detail,
            },
            "apps.versions.list" => page(format!("{BASE}/apps/{}/versions", app()?), app_version),
            "apps.installs.list" => {
                page(format!("{BASE}/apps/{}/installations", app()?), app_install)
            }
            _ => return Err(ErrorCode::CapabilityNotFound),
        })
    })())
    .filter(|r| !matches!(r, Err(ErrorCode::CapabilityNotFound)))
}

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    let read = match plan(auth)? {
        Ok(r) => r,
        Err(e) => return Some(Err(e)),
    };
    let operator = format!("hq:{}", auth.staff.claims.sub);
    let q = &auth.query;
    let answer = |r: T<Value>| {
        r.map(|result| Answer {
            result,
            freshness: "LIVE",
        })
        .map_err(ErrorCode::from)
    };
    Some(match read {
        Read::Single { path, translate } => {
            match bridge.owners.read(O, &path, &[], &operator).await {
                Ok(v) => answer(translate(&v)),
                Err(e) => Err(e.into()),
            }
        }
        Read::Page {
            path,
            item,
            window,
            search,
        } => {
            let mut query: Vec<(&str, String)> = Vec::new();
            if auth.capability.id == "partners.workspace-activity.list" {
                match uuid_param(auth, "workspace_id") {
                    Ok(ws) => query.push(("organizationId", ws)),
                    Err(e) => return Some(Err(e)),
                }
            }
            if let (Some(name), Some(text)) = (search, q.q.clone()) {
                query.push((name, text));
            }
            if window {
                query.extend(q.start.map(|s| ("since", rfc(s))));
                query.extend(q.end.map(|e| ("until", rfc(e))));
            }
            query.extend(q.limit.map(|l| ("limit", l.to_string())));
            query.extend(q.cursor.clone().map(|c| ("cursor", c)));
            match bridge.owners.read(O, &path, &query, &operator).await {
                Ok(v) if auth.capability.id == "partners.usage.read" => {
                    answer(t::page(&v, item).and_then(|(i, n)| {
                        let mut out = listing(i, n);
                        out["from"] = day(&v, "from")?;
                        out["to"] = day(&v, "to")?;
                        Ok(out)
                    }))
                }
                Ok(v) => answer(t::page(&v, item).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
    })
}
