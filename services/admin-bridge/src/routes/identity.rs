//! Identity reads (design §6.8) and the partner reads whose data lives in Platform Access
//! (keys, service accounts, allowed websites, partner users; §6.7), served from Platform
//! Access's operator reads (`platform-identity.administer` + `X-CF-Operator: hq:<sub>`).
//!
//! A partner workspace id IS its Platform Access tenant id (the Developer Platform keys
//! workspaces by the canonical tenant id), so the partner reads call the tenant routes.
//!
//! Translation keeps identifiers, names, kinds, states, scopes and timestamps only: never a
//! credential secret, verifier or label, never a principal's external reference (which may
//! be an email address), never OAuth redirect URIs or test accounts,
//! never the audit detail. Cursors are checked here; a malformed one never reaches the owner.

use serde_json::{json, Value};

use super::translate::{self as t, obj, T};
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::Owner;
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

const O: Owner = Owner::PlatformAccess;
const BASE: &str = "/internal/v1/operator";
const PARTNER_USER_KIND: &str = "EXTERNAL_APPLICATION_PRINCIPAL";

fn uuid_param(auth: &Authorized, name: &str) -> Result<String, ErrorCode> {
    let raw = auth.params.get(name).ok_or(ErrorCode::Internal)?;
    // Platform Access keys tenants and apps by UUID: any other id names nothing.
    uuid::Uuid::parse_str(raw)
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| ErrorCode::ResourceNotFound)
}

fn rfc(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Cursor forms the owner issues, checked before any call.
#[derive(Clone, Copy)]
enum CursorForm {
    Uuid,
    /// `<applicationId>:<scope>` (scope grants).
    ScopeGrant,
    /// `<micros>_<32 hex>` (audit).
    Audit,
}

fn cursor_ok(form: CursorForm, c: &str) -> bool {
    match form {
        CursorForm::Uuid => uuid::Uuid::parse_str(c).is_ok(),
        CursorForm::ScopeGrant => c.split_once(':').is_some_and(|(a, s)| {
            uuid::Uuid::parse_str(a).is_ok() && !s.is_empty() && s.len() <= 100
        }),
        CursorForm::Audit => c.split_once('_').is_some_and(|(m, id)| {
            !m.is_empty()
                && m.len() <= 20
                && m.bytes().all(|b| b.is_ascii_digit())
                && id.len() == 32
                && id.bytes().all(|b| b.is_ascii_hexdigit())
        }),
    }
}

fn string_array(v: &Value, k: &str) -> T<Value> {
    let arr = v
        .get(k)
        .and_then(Value::as_array)
        .ok_or(OwnerError::Contract)?;
    arr.iter()
        .map(|x| {
            x.as_str()
                .map(|s| Value::String(s.to_string()))
                .ok_or(OwnerError::Contract)
        })
        .collect::<T<Vec<_>>>()
        .map(Value::Array)
}

fn opt_string_array(v: &Value, k: &str) -> T<Value> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(_) => string_array(v, k),
    }
}

pub fn tenant(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("tenant_id", t::uuid(o, "id")?),
        ("slug", t::string(o, "slug")?),
        ("display_name", t::string(o, "displayName")?),
        ("classification", t::string(o, "classification")?),
        ("status", t::string(o, "status")?),
        ("billing_currency", t::string(o, "billingCurrency")?),
        ("revision", t::int(o, "revision")?),
        ("created_at", t::time(o, "createdAt")?),
        ("updated_at", t::time(o, "updatedAt")?),
    ]))
}

pub fn application(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("application_id", t::uuid(o, "id")?),
        ("slug", t::string(o, "slug")?),
        ("display_name", t::string(o, "displayName")?),
        ("status", t::string(o, "status")?),
        ("revision", t::int(o, "revision")?),
        ("created_at", t::time(o, "createdAt")?),
        ("updated_at", t::time(o, "updatedAt")?),
    ]))
}

pub fn environment(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("environment_id", t::uuid(o, "id")?),
        ("application_id", t::uuid(o, "applicationId")?),
        ("kind", t::string(o, "kind")?),
        ("status", t::string(o, "status")?),
        ("live_telecom_allowed", t::boolean(o, "liveTelecomAllowed")?),
        ("revision", t::int(o, "revision")?),
        ("created_at", t::time(o, "createdAt")?),
        ("updated_at", t::time(o, "updatedAt")?),
    ]))
}

pub fn service_account(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("service_account_id", t::uuid(o, "id")?),
        ("application_id", t::uuid(o, "applicationId")?),
        ("environment_id", t::uuid(o, "environmentId")?),
        ("name", t::string(o, "name")?),
        ("status", t::string(o, "status")?),
        ("revoked_at", t::opt_time(o, "revokedAt")?),
        ("revision", t::int(o, "revision")?),
        ("created_at", t::time(o, "createdAt")?),
        ("updated_at", t::time(o, "updatedAt")?),
    ]))
}

pub fn scope_grant(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("application_id", t::uuid(o, "applicationId")?),
        ("scope", t::string(o, "scope")?),
        (
            "allowed_environment_kinds",
            opt_string_array(o, "allowedEnvironmentKinds")?,
        ),
        ("grant_basis", t::string(o, "grantBasis")?),
        ("granted_at", t::time(o, "grantedAt")?),
    ]))
}

pub fn key(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("credential_id", t::uuid(o, "id")?),
        ("public_id", t::string(o, "publicId")?),
        ("last4", t::string(o, "lastFour")?),
        ("application_id", t::uuid(o, "applicationId")?),
        ("environment_id", t::uuid(o, "environmentId")?),
        ("service_account_id", t::uuid(o, "serviceAccountId")?),
        ("status", t::string(o, "status")?),
        ("scopes", string_array(o, "scopes")?),
        ("not_before", t::time(o, "notBefore")?),
        ("expires_at", t::time(o, "expiresAt")?),
        ("revoked_at", t::opt_time(o, "revokedAt")?),
        ("superseded_at", t::opt_time(o, "supersededAt")?),
        ("suspended_at", t::opt_time(o, "suspendedAt")?),
        ("suspension_reason", t::opt_string(o, "suspensionReason")?),
        ("last_used_at", t::opt_time(o, "lastUsedAt")?),
        ("revision", t::int(o, "revision")?),
        ("created_at", t::time(o, "createdAt")?),
    ]))
}

pub fn principal(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("principal_id", t::uuid(o, "id")?),
        ("tenant_id", t::uuid(o, "tenantId")?),
        ("application_id", t::uuid(o, "applicationId")?),
        ("environment_id", t::uuid(o, "environmentId")?),
        ("kind", t::string(o, "kind")?),
        (
            "linked_service_account_id",
            t::opt_uuid(o, "linkedServiceAccountId")?,
        ),
        ("status", t::string(o, "status")?),
        ("tombstoned_at", t::opt_time(o, "tombstonedAt")?),
        ("revision", t::int(o, "revision")?),
        ("created_at", t::time(o, "createdAt")?),
        ("updated_at", t::time(o, "updatedAt")?),
    ]))
}

pub fn websites(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("environment_id", t::uuid(o, "environmentId")?),
        ("application_id", t::uuid(o, "applicationId")?),
        ("environment_kind", t::string(o, "environmentKind")?),
        ("origins", string_array(o, "origins")?),
        ("revision", t::opt_int(o, "revision")?),
        ("updated_at", t::opt_time(o, "updatedAt")?),
    ]))
}

pub fn oauth_app(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("application_id", t::uuid(o, "applicationId")?),
        ("tenant_id", t::uuid(o, "tenantId")?),
        ("name", t::string(o, "name")?),
        ("distribution", t::string(o, "distribution")?),
        ("status", t::string(o, "status")?),
        ("user_scopes", string_array(o, "userScopes")?),
        ("bot_scopes", string_array(o, "botScopes")?),
        ("redirect_uri_count", t::int(o, "redirectUriCount")?),
        ("revision", t::int(o, "revision")?),
        ("created_at", t::time(o, "createdAt")?),
        ("updated_at", t::time(o, "updatedAt")?),
    ]))
}

pub fn app_grant(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("grant_id", t::uuid(o, "id")?),
        ("target", t::string(o, "target")?),
        ("account_id", t::opt_uuid(o, "accountId")?),
        ("organization_id", t::opt_uuid(o, "organizationId")?),
        (
            "installed_by_account_id",
            t::uuid(o, "installedByAccountId")?,
        ),
        ("environment_id", t::uuid(o, "environmentId")?),
        ("principal_id", t::uuid(o, "principalId")?),
        ("scopes", string_array(o, "scopes")?),
        ("status", t::string(o, "status")?),
        ("revoked_at", t::opt_time(o, "revokedAt")?),
        ("revoke_reason", t::opt_string(o, "revokeReason")?),
        ("created_at", t::time(o, "createdAt")?),
        ("updated_at", t::time(o, "updatedAt")?),
    ]))
}

pub fn audit_entry(o: &Value) -> T<Value> {
    Ok(obj(vec![
        ("event_id", t::uuid(o, "id")?),
        ("actor", t::string(o, "actor")?),
        ("correlation_id", t::string(o, "correlationId")?),
        ("action", t::string(o, "action")?),
        ("target_kind", t::string(o, "targetKind")?),
        ("target_id", t::opt_uuid(o, "targetId")?),
        ("tenant_id", t::opt_uuid(o, "tenantId")?),
        ("application_id", t::opt_uuid(o, "applicationId")?),
        ("environment_id", t::opt_uuid(o, "environmentId")?),
        ("occurred_at", t::time(o, "occurredAt")?),
    ]))
}

/// One planned owner page read.
struct Plan {
    path: String,
    query: Vec<(&'static str, String)>,
    cursor: CursorForm,
    item: fn(&Value) -> T<Value>,
}

fn plan(auth: &Authorized) -> Option<Result<Plan, ErrorCode>> {
    let tenant = |name: &str| uuid_param(auth, name);
    let q = &auth.query;
    let mk = |path: Result<String, ErrorCode>,
              extra: Vec<(&'static str, String)>,
              cursor: CursorForm,
              item: fn(&Value) -> T<Value>| {
        path.map(|path| Plan {
            path,
            query: extra,
            cursor,
            item,
        })
    };
    let search = || {
        q.q.clone()
            .map(|v| ("q", v))
            .into_iter()
            .collect::<Vec<_>>()
    };
    Some(match auth.capability.id {
        "identity.tenants.list" => mk(
            Ok(format!("{BASE}/tenants")),
            search(),
            CursorForm::Uuid,
            tenant_item,
        ),
        "identity.applications.list" => mk(
            tenant("tenant_id").map(|t| format!("{BASE}/tenants/{t}/applications")),
            vec![],
            CursorForm::Uuid,
            application,
        ),
        "identity.environments.list" => mk(
            tenant("tenant_id").map(|t| format!("{BASE}/tenants/{t}/environments")),
            vec![],
            CursorForm::Uuid,
            environment,
        ),
        "identity.service-accounts.list" => mk(
            tenant("tenant_id").map(|t| format!("{BASE}/tenants/{t}/service-accounts")),
            vec![],
            CursorForm::Uuid,
            service_account,
        ),
        "identity.scope-grants.list" => mk(
            tenant("tenant_id").map(|t| format!("{BASE}/tenants/{t}/scope-grants")),
            vec![],
            CursorForm::ScopeGrant,
            scope_grant,
        ),
        "identity.keys.list" => mk(
            tenant("tenant_id").map(|t| format!("{BASE}/tenants/{t}/credentials")),
            vec![],
            CursorForm::Uuid,
            key,
        ),
        "identity.records.list" => mk(
            Ok(format!("{BASE}/principals")),
            search(),
            CursorForm::Uuid,
            principal,
        ),
        "identity.oauth-apps.list" => mk(
            Ok(format!("{BASE}/oauth-apps")),
            search(),
            CursorForm::Uuid,
            oauth_app,
        ),
        "identity.app-grants.list" => mk(
            uuid_param(auth, "app_id").map(|a| format!("{BASE}/oauth-apps/{a}/grants")),
            vec![],
            CursorForm::Uuid,
            app_grant,
        ),
        "identity.partner-users.list" => {
            let mut extra = vec![("kind", PARTNER_USER_KIND.to_string())];
            extra.extend(search());
            mk(
                tenant("tenant_id").map(|t| format!("{BASE}/tenants/{t}/principals")),
                extra,
                CursorForm::Uuid,
                principal,
            )
        }
        "identity.audit.list" => {
            let mut extra = Vec::new();
            extra.extend(q.start.map(|s| ("since", rfc(s))));
            extra.extend(q.end.map(|e| ("until", rfc(e))));
            mk(
                Ok(format!("{BASE}/audit")),
                extra,
                CursorForm::Audit,
                audit_entry,
            )
        }
        // Partner reads keyed by workspace = Platform Access tenant.
        "partners.keys.list" => mk(
            tenant("workspace_id").map(|t| format!("{BASE}/tenants/{t}/credentials")),
            vec![],
            CursorForm::Uuid,
            key,
        ),
        "partners.service-accounts.list" => mk(
            tenant("workspace_id").map(|t| format!("{BASE}/tenants/{t}/service-accounts")),
            vec![],
            CursorForm::Uuid,
            service_account,
        ),
        "partners.websites.list" => mk(
            tenant("workspace_id").map(|t| format!("{BASE}/tenants/{t}/origins")),
            vec![],
            CursorForm::Uuid,
            websites,
        ),
        "partners.users.list" => {
            let mut extra = vec![("kind", PARTNER_USER_KIND.to_string())];
            extra.extend(search());
            mk(
                tenant("workspace_id").map(|t| format!("{BASE}/tenants/{t}/principals")),
                extra,
                CursorForm::Uuid,
                principal,
            )
        }
        _ => return None,
    })
}

fn tenant_item(o: &Value) -> T<Value> {
    tenant(o)
}

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    let plan = match plan(auth)? {
        Ok(p) => p,
        Err(e) => return Some(Err(e)),
    };
    let q = &auth.query;
    if q.cursor
        .as_deref()
        .is_some_and(|c| !cursor_ok(plan.cursor, c))
    {
        return Some(Err(ErrorCode::QueryInvalid));
    }
    let mut query = plan.query;
    query.extend(q.limit.map(|l| ("limit", l.to_string())));
    query.extend(q.cursor.clone().map(|c| ("cursor", c)));
    let operator = format!("hq:{}", auth.staff.claims.sub);
    Some(
        match bridge.owners.read(O, &plan.path, &query, &operator).await {
            Ok(v) => t::page(&v, plan.item)
                .map(|(items, next)| Answer {
                    result: json!({"items": items, "next_cursor": next}),
                    freshness: "LIVE",
                })
                .map_err(ErrorCode::from),
            Err(e) => Err(e.into()),
        },
    )
}
