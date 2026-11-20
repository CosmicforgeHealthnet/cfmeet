//! Organizations reads (design §6.6), served from the Organizations Service.
//!
//! Existing operator routes (`organization.administer`): list / search, per-organization
//! audit, service bindings. New operator reads (`organization.operator-read`,
//! `/internal/v1/operator/*`): detail, members, teams, policies, cross-organization audit.
//! Every call carries `X-CF-Operator: hq:<sub>`. Profile contact details (address,
//! public email, registration number) are never relayed.

use serde_json::{json, Value};

use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::Organizations;

fn operator(auth: &Authorized) -> String {
    format!("hq:{}", auth.staff.claims.sub)
}

fn uuid_cursor(auth: &Authorized) -> Result<Option<String>, ErrorCode> {
    match &auth.query.cursor {
        None => Ok(None),
        Some(c) => uuid::Uuid::parse_str(c)
            .map(|u| Some(u.to_string()))
            .map_err(|_| ErrorCode::QueryInvalid),
    }
}

fn int_cursor(auth: &Authorized) -> Result<Option<String>, ErrorCode> {
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

fn org_id(auth: &Authorized) -> Result<String, ErrorCode> {
    auth.params
        .get("organization_id")
        .cloned()
        .ok_or(ErrorCode::Internal)
}

pub fn summary(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("organization_id", t::uuid(o, "id")?),
        ("name", t::string(o, "name")?),
        ("slug", t::string(o, "slug")?),
        ("lifecycle_status", t::string(o, "lifecycleStatus")?),
        ("provisioning_state", t::string(o, "provisioningState")?),
        ("member_count", t::opt_int(o, "memberCount")?),
        ("created_at", t::time(o, "createdAt")?),
        ("updated_at", t::time(o, "updatedAt")?),
        ("version", t::int(o, "version")?),
    ]))
}

fn detail(o: &Value) -> T<Value> {
    let mut v = summary(o)?;
    let profile = o.get("profile").cloned().unwrap_or(Value::Null);
    let p = |k: &str| -> T<Value> {
        if profile.is_null() {
            Ok(Value::Null)
        } else {
            t::opt_string(&profile, k)
        }
    };
    let verification = profile
        .get("legalRegistration")
        .and_then(|l| l.get("verification"))
        .cloned()
        .unwrap_or(Value::Null);
    let verification = match verification {
        Value::Null | Value::String(_) => verification,
        _ => return Err(OwnerError::Contract),
    };
    v["creation_source"] = t::string(o, "creationSource")?;
    v["archived_at"] = t::opt_time(o, "archivedAt")?;
    v["organization_type"] = p("organizationType")?;
    v["industry"] = p("industry")?;
    v["country_code"] = p("countryCode")?;
    v["legal_verification"] = verification;
    Ok(v)
}

pub fn audit_item(e: &Value) -> T<Value> {
    let actor = e.get("actor").ok_or(OwnerError::Contract)?;
    Ok(obj(vec![
        ("event_id", t::uuid(e, "id")?),
        ("sequence", t::int(e, "seq")?),
        ("organization_id", t::opt_uuid(e, "organizationId")?),
        ("action", t::string(e, "action")?),
        (
            "actor",
            obj(vec![
                ("kind", t::string(actor, "kind")?),
                ("user_id", t::opt_uuid(actor, "userId")?),
                ("service", t::string(actor, "serviceId")?),
            ]),
        ),
        ("target_type", t::string(e, "targetType")?),
        ("target_id", t::opt_uuid(e, "targetId")?),
        ("occurred_at", t::time(e, "occurredAt")?),
    ]))
}

pub fn member_item(m: &Value) -> T<Value> {
    Ok(obj(vec![
        ("membership_id", t::uuid(m, "id")?),
        ("user_id", t::uuid(m, "userId")?),
        ("role", t::string(m, "role")?),
        ("status", t::string(m, "status")?),
        ("joined_via", t::opt_string(m, "joinedVia")?),
        ("created_at", t::time(m, "createdAt")?),
        ("updated_at", t::time(m, "updatedAt")?),
        ("removed_at", t::opt_time(m, "removedAt")?),
        ("version", t::int(m, "version")?),
    ]))
}

pub fn team_item(m: &Value) -> T<Value> {
    Ok(obj(vec![
        ("team_id", t::uuid(m, "id")?),
        ("name", t::string(m, "name")?),
        ("parent_team_id", t::opt_uuid(m, "parentTeamId")?),
        ("status", t::string(m, "status")?),
        ("member_count", t::opt_int(m, "memberCount")?),
        ("created_at", t::time(m, "createdAt")?),
        ("updated_at", t::time(m, "updatedAt")?),
        ("archived_at", t::opt_time(m, "archivedAt")?),
        ("version", t::int(m, "version")?),
    ]))
}

pub fn binding_item(b: &Value) -> T<Value> {
    Ok(obj(vec![
        ("service", t::string(b, "service")?),
        ("source", t::string(b, "source")?),
        ("version", t::int(b, "version")?),
        ("verified_at", t::opt_time(b, "verifiedAt")?),
        (
            "credential_expires_at",
            t::opt_time(b, "credentialExpiresAt")?,
        ),
        ("created_at", t::time(b, "createdAt")?),
        ("updated_at", t::time(b, "updatedAt")?),
    ]))
}

/// Services an organization can be bound to (Organizations `bindings::SERVICES`).
pub const BINDING_SERVICES: &[&str] = &["calling", "messaging"];

fn section<'a>(doc: &'a Value, key: &str) -> T<&'a Value> {
    doc.get(key)
        .filter(|v| v.is_object())
        .ok_or(OwnerError::Contract)
}

/// The organization's policy document, field by field. A section the bridge does not know
/// is dropped (reviewed before it is relayed); a known field that is missing or mistyped
/// fails the answer.
pub fn policies(v: &Value) -> T<Value> {
    let doc = v
        .get("policy")
        .filter(|p| p.is_object())
        .ok_or(OwnerError::Contract)?;
    let md = section(doc, "memberDirectory")?;
    let guests = section(doc, "guests")?;
    let meetings = section(doc, "meetings")?;
    let features = section(doc, "features")?;
    let retention = section(doc, "retention")?;
    let delegated = section(doc, "delegatedAdministration")?;
    let apps = match doc.get("apps") {
        None | Some(Value::Null) => Value::Null,
        Some(a) if a.is_object() => {
            let allow = match a.get("allowList") {
                None | Some(Value::Null) => Value::Null,
                Some(Value::Array(ids)) if ids.len() <= 200 => Value::Array(
                    ids.iter()
                        .map(|id| {
                            id.as_str()
                                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                                .map(|u| Value::String(u.to_string()))
                                .ok_or(OwnerError::Contract)
                        })
                        .collect::<T<Vec<_>>>()?,
                ),
                _ => return Err(OwnerError::Contract),
            };
            obj(vec![
                ("install_tier", t::string(a, "installTier")?),
                ("allow_list", allow),
                (
                    "user_installs_allowed",
                    t::boolean(a, "userInstallsAllowed")?,
                ),
            ])
        }
        _ => return Err(OwnerError::Contract),
    };
    Ok(obj(vec![
        ("organization_id", t::uuid(v, "organizationId")?),
        ("revision", t::int(v, "revision")?),
        ("updated_at", t::opt_time(v, "updatedAt")?),
        (
            "policy",
            obj(vec![
                (
                    "member_directory",
                    obj(vec![
                        ("visibility", t::string(md, "visibility")?),
                        ("searchable", t::boolean(md, "searchable")?),
                    ]),
                ),
                (
                    "guests",
                    obj(vec![(
                        "invitations_allowed",
                        t::boolean(guests, "invitationsAllowed")?,
                    )]),
                ),
                (
                    "meetings",
                    obj(vec![(
                        "host_creation",
                        t::string(meetings, "hostCreation")?,
                    )]),
                ),
                (
                    "features",
                    obj(vec![
                        ("meetings", t::boolean(features, "meetings")?),
                        ("messaging", t::boolean(features, "messaging")?),
                        ("calling", t::boolean(features, "calling")?),
                    ]),
                ),
                (
                    "retention",
                    obj(vec![("hint_days", t::opt_int(retention, "hintDays")?)]),
                ),
                (
                    "delegated_administration",
                    obj(vec![
                        (
                            "managers_may_invite",
                            t::boolean(delegated, "managersMayInvite")?,
                        ),
                        (
                            "managers_may_create_teams",
                            t::boolean(delegated, "managersMayCreateTeams")?,
                        ),
                        (
                            "admins_may_manage_policies",
                            t::boolean(delegated, "adminsMayManagePolicies")?,
                        ),
                    ]),
                ),
                ("apps", apps),
            ]),
        ),
    ]))
}

fn listing(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "next_cursor": next})
}

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    let op = operator(auth);
    let q = &auth.query;
    let limit = q.limit.map(|l| ("limit", l.to_string()));
    let read = |path: String, query: Vec<(&'static str, String)>| {
        let op = op.clone();
        async move { bridge.owners.read(O, &path, &query, &op).await }
    };
    let answer = |r: T<Value>| -> Result<Answer, ErrorCode> {
        r.map(|result| Answer {
            result,
            freshness: "LIVE",
        })
        .map_err(ErrorCode::from)
    };
    Some(match auth.capability.id {
        "organizations.organizations.search" => {
            let cursor = match uuid_cursor(auth) {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };
            let mut query = vec![("q", q.q.clone().unwrap_or_default())];
            query.extend(limit.clone());
            query.extend(cursor.map(|c| ("cursor", c)));
            match read("/internal/v1/admin/organizations".into(), query).await {
                Ok(v) => answer(t::page(&v, summary).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
        "organizations.organizations.read" => {
            let id = match org_id(auth) {
                Ok(i) => i,
                Err(e) => return Some(Err(e)),
            };
            match read(format!("/internal/v1/operator/organizations/{id}"), vec![]).await {
                Ok(v) => answer(detail(&v)),
                Err(e) => Err(e.into()),
            }
        }
        "organizations.audit.list" => {
            let (id, before) = match (org_id(auth), int_cursor(auth)) {
                (Ok(i), Ok(b)) => (i, b),
                (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
            };
            let mut query: Vec<(&'static str, String)> = Vec::new();
            query.extend(before.map(|b| ("before", b)));
            query.extend(limit.clone());
            query.extend(q.start.map(|s| ("from", rfc(s))));
            query.extend(q.end.map(|e| ("to", rfc(e))));
            match read(
                format!("/internal/v1/admin/organizations/{id}/audit-events"),
                query,
            )
            .await
            {
                Ok(v) => answer(t::page(&v, audit_item).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
        "organizations.global-audit.list" => {
            let mut query: Vec<(&'static str, String)> = Vec::new();
            query.extend(q.start.map(|s| ("since", rfc(s))));
            query.extend(q.end.map(|e| ("until", rfc(e))));
            query.extend(limit.clone());
            query.extend(q.cursor.clone().map(|c| ("cursor", c)));
            match read("/internal/v1/operator/audit-events".into(), query).await {
                Ok(v) => answer(t::page(&v, audit_item).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
        "organizations.members.list" => {
            let (id, cursor) = match (org_id(auth), uuid_cursor(auth)) {
                (Ok(i), Ok(c)) => (i, c),
                (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
            };
            let mut query: Vec<(&'static str, String)> = Vec::new();
            query.extend(limit.clone());
            query.extend(cursor.map(|c| ("cursor", c)));
            match read(
                format!("/internal/v1/operator/organizations/{id}/members"),
                query,
            )
            .await
            {
                Ok(v) => answer(t::page(&v, member_item).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
        "organizations.policies.read" => {
            let id = match org_id(auth) {
                Ok(i) => i,
                Err(e) => return Some(Err(e)),
            };
            match read(
                format!("/internal/v1/operator/organizations/{id}/policies"),
                vec![],
            )
            .await
            {
                Ok(v) => answer(policies(&v)),
                Err(e) => Err(e.into()),
            }
        }
        "organizations.teams.list" => {
            let (id, cursor) = match (org_id(auth), uuid_cursor(auth)) {
                (Ok(i), Ok(c)) => (i, c),
                (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
            };
            let mut query: Vec<(&'static str, String)> = Vec::new();
            query.extend(limit.clone());
            query.extend(cursor.map(|c| ("cursor", c)));
            match read(
                format!("/internal/v1/operator/organizations/{id}/teams"),
                query,
            )
            .await
            {
                Ok(v) => answer(t::page(&v, team_item).map(|(i, n)| listing(i, n))),
                Err(e) => Err(e.into()),
            }
        }
        "organizations.bindings.list" => {
            let id = match org_id(auth) {
                Ok(i) => i,
                Err(e) => return Some(Err(e)),
            };
            // One owner read per bindable service; 404 means "not bound", not an error.
            let mut items = Vec::new();
            for service in BINDING_SERVICES {
                let path = format!("/internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/{service}");
                match read(path, vec![]).await {
                    Ok(v) => match binding_item(&v) {
                        Ok(b) => items.push(b),
                        Err(e) => return Some(Err(e.into())),
                    },
                    Err(OwnerError::NotFound) => {}
                    Err(e) => return Some(Err(e.into())),
                }
            }
            Ok(Answer {
                result: listing(items, Value::Null),
                freshness: "LIVE",
            })
        }
        _ => return None,
    })
}
