//! Wire snapshots of identity rows (contract `cfmeet.platform-access.identity` v1).
//!
//! Snapshots are what consumers project from: the authenticated authority chain, the
//! change feed and resolved principals all carry them. They contain CF Meet identifiers,
//! statuses, classifications and timestamps — never a credential secret or verifier
//! (`verifier`, `verifier_algorithm` and `verifier_key_version` have no field here, and
//! the database feed trigger removes them before a change is even recorded).
//!
//! Database rows (`to_jsonb(row)`, snake_case) are converted with [`from_row_json`]:
//! keys become camelCase, then the typed snapshot validates the shape, so a row that does
//! not match the contract fails loudly instead of leaking an unexpected column.

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::domain::{
    ApplicationStatus, CredentialStatus, EnvironmentKind, EnvironmentStatus, PrincipalKind,
    PrincipalStatus, ServiceAccountStatus, TenantClassification, TenantStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TenantSnapshot {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub classification: TenantClassification,
    pub status: TenantStatus,
    pub billing_currency: String,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationSnapshot {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub status: ApplicationStatus,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSnapshot {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub kind: EnvironmentKind,
    pub status: EnvironmentStatus,
    pub live_telecom_allowed: bool,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceAccountSnapshot {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: ServiceAccountStatus,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A credential without any verifier material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialSnapshot {
    pub id: Uuid,
    pub public_id: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
    pub label: Option<String>,
    pub last_four: String,
    pub status: CredentialStatus,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub rotated_from_credential_id: Option<Uuid>,
    pub superseded_by_credential_id: Option<Uuid>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub supersession_valid_until: Option<DateTime<Utc>>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrincipalSnapshot {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: PrincipalKind,
    pub external_reference: String,
    pub linked_service_account_id: Option<Uuid>,
    pub status: PrincipalStatus,
    pub tombstoned_at: Option<DateTime<Utc>>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationScopeSnapshot {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub scope: String,
    pub allowed_environment_kinds: Option<Vec<EnvironmentKind>>,
    pub grant_basis: String,
    pub granted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialScopeSnapshot {
    pub credential_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub scope: String,
    pub granted_at: DateTime<Utc>,
}

/// `snake_case` → `camelCase`.
fn camel(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut upper = false;
    for c in key.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// PostgreSQL `to_jsonb(timestamptz)` renders `+00:00` offsets and up to microseconds;
/// chrono accepts both. Keys are converted to camelCase; nothing else changes.
pub fn camel_object(row: Value) -> Value {
    match row {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (camel(&k), v))
                .collect::<Map<_, _>>(),
        ),
        other => other,
    }
}

/// Converts a database row object (`to_jsonb(row)`) into its typed snapshot.
pub fn from_row_json<T: DeserializeOwned>(row: Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(camel_object(row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_convert_to_camel_case_snapshots() {
        let row = json!({
            "id": "6f1c2b9e-3a4d-4c5e-8f70-112233445566",
            "slug": "acme",
            "display_name": "Acme",
            "classification": "EXTERNAL",
            "status": "ACTIVE",
            "billing_currency": "USD",
            "revision": 3,
            "created_at": "2026-10-16T05:00:00.123456+00:00",
            "updated_at": "2026-10-16T05:00:00+00:00"
        });
        let t: TenantSnapshot = from_row_json(row).unwrap();
        assert_eq!(t.revision, 3);
        let wire = serde_json::to_value(&t).unwrap();
        assert_eq!(wire["displayName"], "Acme");
        assert!(wire.get("display_name").is_none());
    }

    #[test]
    fn unexpected_columns_are_refused() {
        let row = json!({
            "id": "6f1c2b9e-3a4d-4c5e-8f70-112233445566",
            "public_id": "0123456789abcdef01234567",
            "tenant_id": "6f1c2b9e-3a4d-4c5e-8f70-112233445566",
            "application_id": "6f1c2b9e-3a4d-4c5e-8f70-112233445566",
            "environment_id": "6f1c2b9e-3a4d-4c5e-8f70-112233445566",
            "service_account_id": "6f1c2b9e-3a4d-4c5e-8f70-112233445566",
            "label": null,
            "last_four": "abcd",
            "status": "ACTIVE",
            "not_before": "2026-10-16T05:00:00+00:00",
            "expires_at": "2027-10-16T05:00:00+00:00",
            "revoked_at": null,
            "rotated_from_credential_id": null,
            "superseded_by_credential_id": null,
            "superseded_at": null,
            "supersession_valid_until": null,
            "revision": 1,
            "created_at": "2026-10-16T05:00:00+00:00",
            "updated_at": "2026-10-16T05:00:00+00:00",
            "verifier": "\\x00"
        });
        assert!(from_row_json::<CredentialSnapshot>(row).is_err());
    }
}
