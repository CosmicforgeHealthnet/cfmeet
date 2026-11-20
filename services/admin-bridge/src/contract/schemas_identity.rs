//! Closed result schemas of the identity reads and the partner reads served from Platform
//! Access (A2.3). Identifiers, names, kinds, states, scopes and timestamps only: never a
//! credential secret, verifier or label, never a principal's external reference (a
//! partner's own user id, an email address or a telephone number), never OAuth redirect
//! URIs or test accounts, never the audit detail. No property name is on HQ's
//! personal-data list.

use serde_json::{json, Map, Value};

use super::model::CURSOR_PATTERN;

pub fn insert(m: &mut Map<String, Value>) {
    let uuid = json!({"type": "string", "format": "uuid"});
    let nullable_uuid = json!({"type": ["string", "null"], "format": "uuid"});
    let ts = json!({"type": "string", "format": "date-time"});
    let nullable_ts = json!({"type": ["string", "null"], "format": "date-time"});
    let code = json!({"type": "string", "pattern": "^[A-Z][A-Z0-9_]{0,63}$"});
    let revision = json!({"type": "integer", "minimum": 1});
    let name = |max: u32| json!({"type": "string", "minLength": 1, "maxLength": max});
    let scopes = json!({"type": "array", "maxItems": 200, "items": {"type": "string", "minLength": 1, "maxLength": 100}});
    let env_kind = json!({"enum": ["SANDBOX", "TEST", "PRODUCTION"]});
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
    let mut add = |item: &str, description: &str, props: Value| {
        m.insert(item.into(), object(description, props));
        m.insert(format!("{item}Page"), page(item));
    };

    add(
        "IdentityTenant",
        "One Platform Access tenant.",
        json!({
            "tenant_id": uuid, "slug": name(64), "display_name": name(200),
            "classification": {"enum": ["FIRST_PARTY", "EXTERNAL"]}, "status": code,
            "billing_currency": {"type": "string", "pattern": "^[A-Z]{3}$"}, "revision": revision,
            "created_at": ts, "updated_at": ts
        }),
    );
    add(
        "IdentityApplication",
        "One application of a tenant.",
        json!({
            "application_id": uuid, "slug": name(64), "display_name": name(200), "status": code,
            "revision": revision, "created_at": ts, "updated_at": ts
        }),
    );
    add(
        "IdentityEnvironment",
        "One environment of a tenant's application.",
        json!({
            "environment_id": uuid, "application_id": uuid, "kind": env_kind, "status": code,
            "live_telecom_allowed": {"type": "boolean"}, "revision": revision, "created_at": ts, "updated_at": ts
        }),
    );
    add(
        "IdentityServiceAccount",
        "One service account. Its free-text description is not relayed.",
        json!({
            "service_account_id": uuid, "application_id": uuid, "environment_id": uuid, "name": name(100),
            "status": code, "revoked_at": nullable_ts, "revision": revision, "created_at": ts, "updated_at": ts
        }),
    );
    add(
        "IdentityScopeGrant",
        "One scope granted to an application.",
        json!({
            "application_id": uuid, "scope": name(100),
            "allowed_environment_kinds": {"type": ["array", "null"], "maxItems": 3, "items": env_kind},
            "grant_basis": code, "granted_at": ts
        }),
    );
    add(
        "IdentityKey",
        "One API key: its public id and last four characters, scopes, state and times. Never the secret, the verifier, its algorithm or key version, or the free-text label.",
        json!({
            "credential_id": uuid, "public_id": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,32}$"},
            "last4": {"type": "string", "pattern": "^[A-Za-z0-9]{4}$"},
            "application_id": uuid, "environment_id": uuid, "service_account_id": uuid, "status": code,
            "scopes": scopes, "not_before": ts, "expires_at": ts, "revoked_at": nullable_ts,
            "superseded_at": nullable_ts, "suspended_at": nullable_ts,
            "suspension_reason": {"type": ["string", "null"], "maxLength": 64},
            "last_used_at": nullable_ts, "revision": revision, "created_at": ts
        }),
    );
    add(
        "IdentityPrincipal",
        "One principal (identity record; partner users are kind EXTERNAL_APPLICATION_PRINCIPAL). Its external reference is never relayed.",
        json!({
            "principal_id": uuid, "tenant_id": uuid, "application_id": uuid, "environment_id": uuid,
            "kind": {"enum": [
                "NATIVE_CF_USER", "EXTERNAL_APPLICATION_PRINCIPAL", "ENTERPRISE_MEMBER", "SERVICE_ACCOUNT",
                "TELEPHONE_ENDPOINT", "TEMPORARY_EXTERNAL_PARTICIPANT", "BOT_OR_SERVICE"
            ]},
            "linked_service_account_id": nullable_uuid, "status": code, "tombstoned_at": nullable_ts,
            "revision": revision, "created_at": ts, "updated_at": ts
        }),
    );
    add(
        "PartnerWebsites",
        "The allowed websites (origins) of one environment of a partner workspace; empty when none were set.",
        json!({
            "environment_id": uuid, "application_id": uuid, "environment_kind": env_kind,
            "origins": {"type": "array", "maxItems": 20, "items": {"type": "string", "minLength": 1, "maxLength": 253}},
            "revision": {"type": ["integer", "null"], "minimum": 1}, "updated_at": nullable_ts
        }),
    );
    add(
        "IdentityOAuthApp",
        "One CF Meet Apps OAuth client. The number of redirect URIs, never the URIs or the test accounts.",
        json!({
            "application_id": uuid, "tenant_id": uuid, "name": name(200),
            "distribution": {"enum": ["PRIVATE", "PUBLIC"]}, "status": code,
            "user_scopes": scopes, "bot_scopes": scopes,
            "redirect_uri_count": {"type": "integer", "minimum": 0, "maximum": 10},
            "revision": revision, "created_at": ts, "updated_at": ts
        }),
    );
    add(
        "IdentityAppGrant",
        "One grant (installation) of an OAuth app. Accounts and organizations are opaque CF Meet ids.",
        json!({
            "grant_id": uuid, "target": {"enum": ["USER", "ORGANIZATION"]}, "account_id": nullable_uuid,
            "organization_id": nullable_uuid, "installed_by_account_id": uuid, "environment_id": uuid,
            "principal_id": uuid, "scopes": scopes, "status": code, "revoked_at": nullable_ts,
            "revoke_reason": {"type": ["string", "null"], "maxLength": 32}, "created_at": ts, "updated_at": ts
        }),
    );
    add(
        "IdentityAuditEntry",
        "One Platform Access operator audit row, without its detail.",
        json!({
            "event_id": uuid, "actor": name(140), "correlation_id": name(64), "action": name(64),
            "target_kind": name(32), "target_id": nullable_uuid, "tenant_id": nullable_uuid,
            "application_id": nullable_uuid, "environment_id": nullable_uuid, "occurred_at": ts
        }),
    );
}
