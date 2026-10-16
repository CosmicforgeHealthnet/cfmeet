//! Controlled identity hand-off from the previous authority (Part IV §B2).
//!
//! Until Part IV, Calling owned the identity hierarchy. The migration is an export /
//! import of the persisted rows — identifiers, statuses, validity windows, rotation
//! links, scopes, principals and the keyed HMAC verifiers — never a raw secret (none is
//! stored anywhere). Steps:
//!
//! 1. `platform_admin identity export` (Calling, its own database) writes an
//!    [`IdentityBundle`] file: the rows plus a SHA-256 digest of the canonical body.
//! 2. `platform-access-admin identity import --bundle FILE` (this module) verifies the
//!    digest, validates every row against the contract shape, inserts everything in ONE
//!    transaction (all or nothing; an existing id or key aborts the import — there is no
//!    merge) and records an [`ImportReceipt`]: bundle digest, per-entity counts and a
//!    digest of the imported verifiers. Re-importing the same bundle returns the same
//!    receipt (idempotent).
//! 3. Calling verifies the receipt through the authenticated internal API (its own
//!    digests must match) before it stops holding verifier material.
//!
//! [`export_bundle`] writes the same format from this database (recovery / rollback:
//! Calling can restore verifiers from it during the rollback window).
//!
//! Bundles contain verifiers (not secrets, but sensitive: with the pepper they allow
//! offline guessing) — files are created 0600 and never logged.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    operator::{self, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
    PlatformError, PlatformResult, PlatformService,
};
use crate::domain::{
    ApplicationStatus, CredentialStatus, EnvironmentKind, EnvironmentStatus, PrincipalKind,
    PrincipalStatus, ServiceAccountStatus, TenantClassification, TenantStatus,
};

pub const BUNDLE_VERSION: u64 = 1;
/// Largest bundle file accepted.
pub const MAX_BUNDLE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleTenant {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub classification: TenantClassification,
    pub status: TenantStatus,
    pub billing_currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleApplication {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub status: ApplicationStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleEnvironment {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub kind: EnvironmentKind,
    pub status: EnvironmentStatus,
    pub live_telecom_allowed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleServiceAccount {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: ServiceAccountStatus,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleApplicationScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub scope: String,
    /// Absent in bundles exported by Calling (no restriction existed there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_environment_kinds: Option<Vec<EnvironmentKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_basis: Option<String>,
    pub granted_at: DateTime<Utc>,
}

/// A credential row WITH its keyed verifier (hex). Never a secret.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleCredential {
    pub id: Uuid,
    pub public_id: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
    pub label: Option<String>,
    pub verifier: String,
    pub verifier_algorithm: String,
    pub verifier_key_version: i32,
    pub last_four: String,
    pub status: CredentialStatus,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub rotated_from_credential_id: Option<Uuid>,
    pub superseded_by_credential_id: Option<Uuid>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub supersession_valid_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for BundleCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BundleCredential")
            .field("id", &self.id)
            .field("public_id", &self.public_id)
            .field("verifier", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleCredentialScope {
    pub credential_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub scope: String,
    pub granted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundlePrincipal {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: PrincipalKind,
    pub external_reference: String,
    pub linked_service_account_id: Option<Uuid>,
    pub status: PrincipalStatus,
    pub tombstoned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleBody {
    pub tenants: Vec<BundleTenant>,
    pub applications: Vec<BundleApplication>,
    pub environments: Vec<BundleEnvironment>,
    pub service_accounts: Vec<BundleServiceAccount>,
    pub application_scopes: Vec<BundleApplicationScope>,
    pub credentials: Vec<BundleCredential>,
    pub credential_scopes: Vec<BundleCredentialScope>,
    pub principals: Vec<BundlePrincipal>,
}

impl BundleBody {
    pub fn counts(&self) -> BTreeMap<&'static str, usize> {
        BTreeMap::from([
            ("tenants", self.tenants.len()),
            ("applications", self.applications.len()),
            ("environments", self.environments.len()),
            ("serviceAccounts", self.service_accounts.len()),
            ("applicationScopes", self.application_scopes.len()),
            ("credentials", self.credentials.len()),
            ("credentialScopes", self.credential_scopes.len()),
            ("principals", self.principals.len()),
        ])
    }

    /// SHA-256 over `<credential id>:<verifier hex>:<algorithm>:<key version>\n` of every
    /// credential, sorted by id — equal on both sides iff every verifier arrived intact.
    pub fn verifier_digest(&self) -> String {
        let mut lines: Vec<String> = self
            .credentials
            .iter()
            .map(|c| {
                format!(
                    "{}:{}:{}:{}\n",
                    c.id, c.verifier, c.verifier_algorithm, c.verifier_key_version
                )
            })
            .collect();
        lines.sort();
        hex::encode(Sha256::digest(lines.concat().as_bytes()))
    }
}

/// Canonical JSON (object keys sorted at every level, no whitespace), independent of the
/// serde_json map implementation, so exporter and importer compute the same digest.
pub fn canonical_json(value: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).expect("string serializes"));
                    out.push(':');
                    write(&map[k], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            scalar => out.push_str(&serde_json::to_string(scalar).expect("scalar serializes")),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

pub fn body_digest(body: &Value) -> String {
    hex::encode(Sha256::digest(canonical_json(body).as_bytes()))
}

/// A parsed, integrity-checked bundle.
#[derive(Debug)]
pub struct IdentityBundle {
    pub source_service: String,
    pub exported_at: DateTime<Utc>,
    pub digest: String,
    pub body: BundleBody,
}

/// Parses and integrity-checks a bundle file.
pub fn parse_bundle(bytes: &[u8]) -> PlatformResult<IdentityBundle> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(PlatformError::Validation("the bundle is too large".to_string()));
    }
    let invalid = |m: &str| PlatformError::Validation(format!("invalid identity bundle: {m}"));
    let root: Value = serde_json::from_slice(bytes).map_err(|_| invalid("not JSON"))?;
    let obj = root.as_object().ok_or_else(|| invalid("not an object"))?;
    let allowed = ["bundleVersion", "sourceService", "exportedAt", "body", "digest"];
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) || obj.len() != allowed.len() {
        return Err(invalid("unexpected or missing top-level fields"));
    }
    if obj["bundleVersion"].as_u64() != Some(BUNDLE_VERSION) {
        return Err(invalid("unsupported bundleVersion"));
    }
    let source_service = obj["sourceService"]
        .as_str()
        .filter(|s| cf_service_auth::ServiceId::parse(s).is_ok())
        .ok_or_else(|| invalid("sourceService"))?
        .to_string();
    let exported_at = obj["exportedAt"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .ok_or_else(|| invalid("exportedAt"))?
        .with_timezone(&Utc);
    let stated = obj["digest"]
        .as_str()
        .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("digest"))?
        .to_ascii_lowercase();
    let computed = body_digest(&obj["body"]);
    if computed != stated {
        return Err(invalid("the body does not match its digest (corrupted or edited)"));
    }
    let body: BundleBody = serde_json::from_value(obj["body"].clone())
        .map_err(|_| invalid("the body does not match the bundle contract"))?;
    for c in &body.credentials {
        let ok = c.verifier.len() == 64 && c.verifier.bytes().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(invalid("a credential verifier is malformed"));
        }
    }
    Ok(IdentityBundle {
        source_service,
        exported_at,
        digest: computed,
        body,
    })
}

/// Serializes a bundle file for `body` (digest computed here).
pub fn write_bundle(source_service: &str, exported_at: DateTime<Utc>, body: &BundleBody) -> PlatformResult<Vec<u8>> {
    let body = serde_json::to_value(body)
        .map_err(|_| PlatformError::Validation("the bundle cannot be serialized".to_string()))?;
    let digest = body_digest(&body);
    let root = json!({
        "bundleVersion": BUNDLE_VERSION,
        "sourceService": source_service,
        "exportedAt": exported_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        "body": body,
        "digest": digest,
    });
    Ok(canonical_json(&root).into_bytes())
}

/// Receipt of an import (contract `cfmeet.platform-access.identity` `ImportReceipt`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReceipt {
    pub id: Uuid,
    pub source_service: String,
    pub bundle_digest: String,
    pub verifier_digest: String,
    pub counts: BTreeMap<String, u64>,
    pub imported_at: DateTime<Utc>,
    /// `true` when this call found an existing receipt (idempotent re-import).
    #[serde(default)]
    pub already_imported: bool,
}

#[derive(FromQueryResult)]
struct ReceiptRow {
    id: Uuid,
    source_service: String,
    bundle_digest: String,
    verifier_digest: String,
    counts: Value,
    imported_at: DateTime<Utc>,
}

impl ReceiptRow {
    fn into_receipt(self, already: bool) -> ImportReceipt {
        ImportReceipt {
            id: self.id,
            source_service: self.source_service,
            bundle_digest: self.bundle_digest,
            verifier_digest: self.verifier_digest,
            counts: serde_json::from_value(self.counts).unwrap_or_default(),
            imported_at: self.imported_at,
            already_imported: already,
        }
    }
}

/// The receipt of the import of `bundle_digest`, if any.
pub async fn receipt<C: ConnectionTrait>(db: &C, bundle_digest: &str) -> PlatformResult<Option<ImportReceipt>> {
    Ok(ReceiptRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, source_service, bundle_digest, verifier_digest, counts, imported_at \
         FROM platform_identity_imports WHERE bundle_digest = $1",
        [bundle_digest.to_ascii_lowercase().into()],
    ))
    .one(db)
    .await?
    .map(|r| r.into_receipt(false)))
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn decode_verifier(hex_value: &str) -> PlatformResult<Vec<u8>> {
    hex::decode(hex_value)
        .map_err(|_| PlatformError::Validation("a credential verifier is malformed".to_string()))
}

impl PlatformService {
    /// Imports a bundle (see the module documentation). All or nothing.
    pub async fn operator_import_bundle(
        &self,
        op: &OperatorContext,
        bundle: &IdentityBundle,
    ) -> PlatformResult<ImportReceipt> {
        if let Some(mut existing) = receipt(&self.db, &bundle.digest).await? {
            existing.already_imported = true;
            return Ok(existing);
        }
        let body = &bundle.body;
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        for t in &body.tenants {
            txn.execute(stmt(
                "INSERT INTO platform_tenants (id, slug, display_name, classification, status, \
                   billing_currency, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                vec![
                    t.id.into(),
                    t.slug.clone().into(),
                    t.display_name.clone().into(),
                    t.classification.as_str().into(),
                    t.status.as_str().into(),
                    t.billing_currency.clone().into(),
                    t.created_at.into(),
                    t.updated_at.into(),
                ],
            ))
            .await?;
        }
        for a in &body.applications {
            txn.execute(stmt(
                "INSERT INTO platform_applications (id, tenant_id, slug, display_name, status, \
                   created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                vec![
                    a.id.into(),
                    a.tenant_id.into(),
                    a.slug.clone().into(),
                    a.display_name.clone().into(),
                    a.status.as_str().into(),
                    a.created_at.into(),
                    a.updated_at.into(),
                ],
            ))
            .await?;
        }
        for e in &body.environments {
            txn.execute(stmt(
                "INSERT INTO platform_environments (id, tenant_id, application_id, kind, status, \
                   live_telecom_allowed, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                vec![
                    e.id.into(),
                    e.tenant_id.into(),
                    e.application_id.into(),
                    e.kind.as_str().into(),
                    e.status.as_str().into(),
                    e.live_telecom_allowed.into(),
                    e.created_at.into(),
                    e.updated_at.into(),
                ],
            ))
            .await?;
        }
        for s in &body.service_accounts {
            txn.execute(stmt(
                "INSERT INTO platform_service_accounts (id, tenant_id, application_id, environment_id, \
                   name, description, status, revoked_at, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                vec![
                    s.id.into(),
                    s.tenant_id.into(),
                    s.application_id.into(),
                    s.environment_id.into(),
                    s.name.clone().into(),
                    s.description.clone().into(),
                    s.status.as_str().into(),
                    s.revoked_at.into(),
                    s.created_at.into(),
                    s.updated_at.into(),
                ],
            ))
            .await?;
        }
        for g in &body.application_scopes {
            let kinds: Option<Vec<String>> = g
                .allowed_environment_kinds
                .as_ref()
                .map(|k| k.iter().map(|k| k.as_str().to_string()).collect());
            txn.execute(stmt(
                "INSERT INTO platform_application_scopes (tenant_id, application_id, scope, \
                   allowed_environment_kinds, grant_basis, granted_at) VALUES ($1, $2, $3, $4, $5, $6)",
                vec![
                    g.tenant_id.into(),
                    g.application_id.into(),
                    g.scope.clone().into(),
                    kinds.into(),
                    g.grant_basis.clone().unwrap_or_else(|| "STANDARD".to_string()).into(),
                    g.granted_at.into(),
                ],
            ))
            .await?;
        }
        // Predecessors first (rotation links reference them); supersession is set in a
        // second pass, exactly as rotation itself writes it.
        let mut credentials: Vec<&BundleCredential> = body.credentials.iter().collect();
        credentials.sort_by_key(|c| (c.created_at, c.id));
        for c in &credentials {
            txn.execute(stmt(
                "INSERT INTO platform_api_credentials (id, public_id, tenant_id, application_id, \
                   environment_id, service_account_id, label, verifier, verifier_algorithm, \
                   verifier_key_version, last_four, status, not_before, expires_at, revoked_at, \
                   rotated_from_credential_id, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
                vec![
                    c.id.into(),
                    c.public_id.clone().into(),
                    c.tenant_id.into(),
                    c.application_id.into(),
                    c.environment_id.into(),
                    c.service_account_id.into(),
                    c.label.clone().into(),
                    decode_verifier(&c.verifier)?.into(),
                    c.verifier_algorithm.clone().into(),
                    c.verifier_key_version.into(),
                    c.last_four.clone().into(),
                    c.status.as_str().into(),
                    c.not_before.into(),
                    c.expires_at.into(),
                    c.revoked_at.into(),
                    c.rotated_from_credential_id.into(),
                    c.created_at.into(),
                    c.updated_at.into(),
                ],
            ))
            .await?;
        }
        for c in credentials.iter().filter(|c| c.superseded_by_credential_id.is_some()) {
            txn.execute(stmt(
                "UPDATE platform_api_credentials SET superseded_by_credential_id = $2, \
                   superseded_at = $3, supersession_valid_until = $4, updated_at = $5 WHERE id = $1",
                vec![
                    c.id.into(),
                    c.superseded_by_credential_id.into(),
                    c.superseded_at.into(),
                    c.supersession_valid_until.into(),
                    c.updated_at.into(),
                ],
            ))
            .await?;
        }
        for s in &body.credential_scopes {
            txn.execute(stmt(
                "INSERT INTO platform_api_credential_scopes (credential_id, tenant_id, application_id, \
                   scope, granted_at) VALUES ($1, $2, $3, $4, $5)",
                vec![
                    s.credential_id.into(),
                    s.tenant_id.into(),
                    s.application_id.into(),
                    s.scope.clone().into(),
                    s.granted_at.into(),
                ],
            ))
            .await?;
        }
        for p in &body.principals {
            txn.execute(stmt(
                "INSERT INTO platform_principals (id, tenant_id, application_id, environment_id, kind, \
                   external_reference, linked_service_account_id, status, tombstoned_at, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                vec![
                    p.id.into(),
                    p.tenant_id.into(),
                    p.application_id.into(),
                    p.environment_id.into(),
                    p.kind.as_str().into(),
                    p.external_reference.clone().into(),
                    p.linked_service_account_id.into(),
                    p.status.as_str().into(),
                    p.tombstoned_at.into(),
                    p.created_at.into(),
                    p.updated_at.into(),
                ],
            ))
            .await?;
        }
        let counts: BTreeMap<String, u64> = body
            .counts()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v as u64))
            .collect();
        let id = Uuid::new_v4();
        let verifier_digest = body.verifier_digest();
        txn.execute(stmt(
            "INSERT INTO platform_identity_imports (id, source_service, bundle_digest, verifier_digest, \
               counts, actor, imported_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            vec![
                id.into(),
                bundle.source_service.clone().into(),
                bundle.digest.clone().into(),
                verifier_digest.clone().into(),
                serde_json::to_value(&counts).unwrap_or_default().into(),
                op.operator().into(),
                now.into(),
            ],
        ))
        .await?;
        let entry = AuditEntry::new(
            OperatorAction::IdentityImported,
            AuditTargetKind::IdentityImport,
            id,
        )
        .detail(json!({
            "sourceService": bundle.source_service,
            "bundleDigest": bundle.digest,
            "counts": counts,
        }));
        operator::record(&txn, op, &entry, now).await?;
        txn.commit().await?;
        tracing::info!(
            import_id = %id,
            source = %bundle.source_service,
            credentials = body.credentials.len(),
            "identity bundle imported"
        );
        Ok(ImportReceipt {
            id,
            source_service: bundle.source_service.clone(),
            bundle_digest: bundle.digest.clone(),
            verifier_digest,
            counts,
            imported_at: now,
            already_imported: false,
        })
    }

    /// Exports every identity row of this database as a bundle body (recovery format).
    pub async fn export_bundle_body(&self) -> PlatformResult<BundleBody> {
        let rows = |table: &str, order: &str, extra: &str| {
            format!(
                "SELECT COALESCE(jsonb_agg(to_jsonb(r) - 'revision' {extra} ORDER BY {order}), '[]'::jsonb) \
                 FROM {table} r"
            )
        };
        async fn load<T: serde::de::DeserializeOwned>(
            db: &sea_orm::DatabaseConnection,
            sql: String,
        ) -> PlatformResult<Vec<T>> {
            let row = db
                .query_one(Statement::from_string(DbBackend::Postgres, sql))
                .await?
                .ok_or(PlatformError::NotFound("export rows"))?;
            let value: Value = row.try_get_by_index(0)?;
            let items = value.as_array().cloned().unwrap_or_default();
            items
                .into_iter()
                .map(|v| super::snapshots::from_row_json(v).map_err(|_| PlatformError::Database {
                    summary: "an identity row does not match the bundle contract".to_string(),
                    connectivity: false,
                }))
                .collect()
        }
        let db = &self.db;
        let mut credentials: Vec<BundleCredential> = Vec::new();
        for row in db
            .query_all(Statement::from_string(
                DbBackend::Postgres,
                "SELECT to_jsonb(c) - 'revision' - 'verifier' AS row, encode(c.verifier, 'hex') AS verifier \
                 FROM platform_api_credentials c ORDER BY c.created_at, c.id",
            ))
            .await?
        {
            let mut value: Value = row.try_get("", "row")?;
            let verifier: String = row.try_get("", "verifier")?;
            value["verifier"] = Value::String(verifier);
            credentials.push(super::snapshots::from_row_json(value).map_err(|_| {
                PlatformError::Database {
                    summary: "a credential row does not match the bundle contract".to_string(),
                    connectivity: false,
                }
            })?);
        }
        Ok(BundleBody {
            tenants: load(db, rows("platform_tenants", "r.created_at, r.id", "")).await?,
            applications: load(db, rows("platform_applications", "r.created_at, r.id", "")).await?,
            environments: load(db, rows("platform_environments", "r.created_at, r.id", "")).await?,
            service_accounts: load(db, rows("platform_service_accounts", "r.created_at, r.id", ""))
                .await?,
            application_scopes: load(
                db,
                rows("platform_application_scopes", "r.application_id, r.scope", ""),
            )
            .await?,
            credentials,
            credential_scopes: load(
                db,
                rows("platform_api_credential_scopes", "r.credential_id, r.scope", ""),
            )
            .await?,
            principals: load(db, rows("platform_principals", "r.created_at, r.id", "")).await?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"d":[{"y":1,"x":2}],"c":"s"}}"#).unwrap();
        assert_eq!(canonical_json(&a), r#"{"a":{"c":"s","d":[{"x":2,"y":1}]},"b":1}"#);
    }

    #[test]
    fn bundles_round_trip_and_detect_tampering() {
        let body = BundleBody::default();
        let bytes = write_bundle("cfmeet.calling", Utc::now(), &body).unwrap();
        let parsed = parse_bundle(&bytes).unwrap();
        assert_eq!(parsed.source_service, "cfmeet.calling");
        let tampered = String::from_utf8(bytes)
            .unwrap()
            .replace("\"tenants\":[]", "\"tenants\":[1]");
        assert!(parse_bundle(tampered.as_bytes()).is_err());
        assert!(parse_bundle(b"{}").is_err());
        assert!(parse_bundle(b"not json").is_err());
    }
}
