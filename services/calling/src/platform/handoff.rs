//! Controlled hand-off of the identity authority to Platform Access (Part IV §B2,
//! ADR 0032).
//!
//! ```text
//! 1. migration 000044 (expand: projection columns; the Part III binary keeps working)
//! 2. platform_admin identity export --out FILE        ─▶ bundle (rows + keyed verifiers)
//! 3. platform-access-admin identity import --bundle   ─▶ receipt (digests, counts)
//! 4. platform_admin identity verify-handoff --digest  ─▶ receipt == export? (verified)
//! 5. deploy the Platform-Access-consuming Calling      ─▶ authentication by Platform Access
//! 6. platform_admin identity complete-handoff --digest ─▶ Calling's verifiers scrubbed
//! rollback: platform-access-admin identity export + platform_admin identity
//!           restore-verifiers --bundle FILE, then migration 000044 down
//! ```
//!
//! No raw credential secret exists anywhere (only keyed HMAC verifiers, which never leave
//! these files and the two databases). Identity administration is frozen between 2 and 5
//! (the bundle is a point-in-time copy). Every step is operator-audited; the hand-off
//! record keeps digests and counts only.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    access::IdentityAuthority,
    operator::{self, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
    PlatformError, PlatformResult,
};

pub const BUNDLE_VERSION: u64 = 1;
/// The source identity recorded in exported bundles.
pub const SOURCE_SERVICE: &str = "cfmeet.calling";

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

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

fn camel_object(row: Value) -> Value {
    match row {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (camel(&k), v))
                .collect::<Map<_, _>>(),
        ),
        other => other,
    }
}

/// Canonical JSON (object keys sorted at every level, no whitespace) — the exact form
/// both services hash, independent of the JSON map implementation.
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

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// SHA-256 over `<id>:<verifier hex>:<algorithm>:<key version>\n` of every credential,
/// sorted — the same digest Platform Access records in its import receipt.
pub fn verifier_digest(credentials: &[Value]) -> String {
    let mut lines: Vec<String> = credentials
        .iter()
        .map(|c| {
            format!(
                "{}:{}:{}:{}\n",
                c["id"].as_str().unwrap_or_default(),
                c["verifier"].as_str().unwrap_or_default(),
                c["verifierAlgorithm"].as_str().unwrap_or_default(),
                c["verifierKeyVersion"]
            )
        })
        .collect();
    lines.sort();
    sha256_hex(lines.concat().as_bytes())
}

/// An exported bundle (file bytes) and its record.
pub struct Export {
    pub bytes: Vec<u8>,
    pub bundle_digest: String,
    pub verifier_digest: String,
    pub counts: BTreeMap<String, usize>,
}

impl std::fmt::Debug for Export {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Export")
            .field("bundle_digest", &self.bundle_digest)
            .field("counts", &self.counts)
            .finish()
    }
}

async fn rows<C: ConnectionTrait>(db: &C, sql: &str) -> PlatformResult<Vec<Value>> {
    let row = db
        .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await?
        .ok_or(PlatformError::NotFound("export rows"))?;
    let value: Value = row.try_get_by_index(0)?;
    Ok(value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(camel_object)
        .collect())
}

/// Exports every identity row of Calling's database (the Part III authority) with its
/// keyed verifier, and records the hand-off (audited). Refuses when a credential has no
/// verifier — the authority was already handed off (or the row is a projection).
pub async fn export(
    db: &DatabaseConnection,
    op: &OperatorContext,
    now: DateTime<Utc>,
) -> PlatformResult<Export> {
    let missing: i64 = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*) FROM platform_api_credentials WHERE verifier IS NULL".to_string(),
        ))
        .await?
        .map(|r| r.try_get_by_index::<i64>(0))
        .transpose()?
        .unwrap_or(0);
    if missing > 0 {
        return Err(PlatformError::InvalidState(format!(
            "{missing} credential(s) carry no verifier: the identity authority was already handed off; export from Platform Access instead"
        )));
    }
    let agg = |table: &str, order: &str, strip: &str| {
        format!(
            "SELECT COALESCE(jsonb_agg(to_jsonb(r) - 'pa_revision' {strip} ORDER BY {order}), '[]'::jsonb) FROM {table} r"
        )
    };
    let tenants = rows(db, &agg("platform_tenants", "r.created_at, r.id", "")).await?;
    let applications = rows(db, &agg("platform_applications", "r.created_at, r.id", "")).await?;
    let environments = rows(db, &agg("platform_environments", "r.created_at, r.id", "")).await?;
    let service_accounts = rows(
        db,
        &agg("platform_service_accounts", "r.created_at, r.id", ""),
    )
    .await?;
    let application_scopes = rows(
        db,
        "SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY r.application_id, r.scope), '[]'::jsonb) \
         FROM platform_application_scopes r",
    )
    .await?;
    let credential_scopes = rows(
        db,
        "SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY r.credential_id, r.scope), '[]'::jsonb) \
         FROM platform_api_credential_scopes r",
    )
    .await?;
    let credentials = rows(
        db,
        "SELECT COALESCE(jsonb_agg((to_jsonb(r) - 'pa_revision' - 'verifier') \
           || jsonb_build_object('verifier', encode(r.verifier, 'hex')) ORDER BY r.created_at, r.id), '[]'::jsonb) \
         FROM platform_api_credentials r",
    )
    .await?;
    let principals = rows(db, &agg("platform_principals", "r.created_at, r.id", "")).await?;
    let verifier_digest = verifier_digest(&credentials);
    let counts: BTreeMap<String, usize> = [
        ("tenants", tenants.len()),
        ("applications", applications.len()),
        ("environments", environments.len()),
        ("serviceAccounts", service_accounts.len()),
        ("applicationScopes", application_scopes.len()),
        ("credentials", credentials.len()),
        ("credentialScopes", credential_scopes.len()),
        ("principals", principals.len()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let body = json!({
        "tenants": tenants,
        "applications": applications,
        "environments": environments,
        "serviceAccounts": service_accounts,
        "applicationScopes": application_scopes,
        "credentials": credentials,
        "credentialScopes": credential_scopes,
        "principals": principals,
    });
    let bundle_digest = sha256_hex(canonical_json(&body).as_bytes());
    let root = json!({
        "bundleVersion": BUNDLE_VERSION,
        "sourceService": SOURCE_SERVICE,
        "exportedAt": now.to_rfc3339_opts(SecondsFormat::Micros, true),
        "body": body,
        "digest": bundle_digest,
    });
    let bytes = canonical_json(&root).into_bytes();
    let txn = db.begin().await?;
    let id = Uuid::new_v4();
    txn.execute(stmt(
        "INSERT INTO platform_identity_handoffs (id, bundle_digest, verifier_digest, counts, exported_at, exported_by) \
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (bundle_digest) DO NOTHING",
        vec![
            id.into(),
            bundle_digest.clone().into(),
            verifier_digest.clone().into(),
            serde_json::to_value(&counts).unwrap_or_default().into(),
            now.into(),
            op.operator().into(),
        ],
    ))
    .await?;
    operator::record(
        &txn,
        op,
        &AuditEntry::new(
            OperatorAction::IdentityExported,
            AuditTargetKind::IdentityHandoff,
            id,
        )
        .detail(json!({ "bundleDigest": bundle_digest, "counts": counts })),
        now,
    )
    .await?;
    txn.commit().await?;
    tracing::info!(bundle_digest = %bundle_digest, credentials = counts["credentials"], "identity bundle exported");
    Ok(Export {
        bytes,
        bundle_digest,
        verifier_digest,
        counts,
    })
}

/// The recorded state of a hand-off.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffView {
    pub id: Uuid,
    pub bundle_digest: String,
    pub verifier_digest: String,
    pub counts: Value,
    pub exported_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

async fn handoff<C: ConnectionTrait>(
    db: &C,
    digest: &str,
    lock: bool,
) -> PlatformResult<HandoffView> {
    let row = db
        .query_one(stmt(
            &format!(
                "SELECT id, bundle_digest, verifier_digest, counts, exported_at, verified_at, completed_at \
                 FROM platform_identity_handoffs WHERE bundle_digest = $1{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            vec![digest.to_ascii_lowercase().into()],
        ))
        .await?
        .ok_or(PlatformError::NotFound("identity hand-off"))?;
    Ok(HandoffView {
        id: row.try_get("", "id")?,
        bundle_digest: row.try_get("", "bundle_digest")?,
        verifier_digest: row.try_get("", "verifier_digest")?,
        counts: row.try_get("", "counts")?,
        exported_at: row.try_get("", "exported_at")?,
        verified_at: row.try_get("", "verified_at")?,
        completed_at: row.try_get("", "completed_at")?,
    })
}

pub async fn status(db: &DatabaseConnection, digest: &str) -> PlatformResult<HandoffView> {
    handoff(db, digest, false).await
}

/// Step 4: Platform Access's receipt must match the export (bundle digest, verifier
/// digest, counts). Idempotent once verified.
pub async fn verify(
    db: &DatabaseConnection,
    authority: &dyn IdentityAuthority,
    op: &OperatorContext,
    digest: &str,
    now: DateTime<Utc>,
) -> PlatformResult<HandoffView> {
    let current = handoff(db, digest, false).await?;
    if current.verified_at.is_some() {
        return Ok(current);
    }
    let receipt = authority
        .import_receipt(&current.bundle_digest)
        .await
        .map_err(|e| {
            PlatformError::InvalidState(format!("platform access receipt unavailable: {e}"))
        })?
        .ok_or_else(|| {
            PlatformError::InvalidState(
                "platform access has no receipt for this bundle: import it first".to_string(),
            )
        })?;
    let expected: BTreeMap<String, u64> =
        serde_json::from_value(current.counts.clone()).unwrap_or_default();
    if receipt.bundle_digest != current.bundle_digest
        || receipt.verifier_digest != current.verifier_digest
        || receipt.counts != expected
    {
        return Err(PlatformError::InvalidState(
            "platform access's receipt does not match the export (digest, verifiers or counts differ); do not cut over"
                .to_string(),
        ));
    }
    let txn = db.begin().await?;
    let locked = handoff(&txn, digest, true).await?;
    if locked.verified_at.is_none() {
        txn.execute(stmt(
            "UPDATE platform_identity_handoffs SET verified_at = $2 WHERE id = $1",
            vec![locked.id.into(), now.into()],
        ))
        .await?;
        operator::record(
            &txn,
            op,
            &AuditEntry::new(
                OperatorAction::IdentityHandoffVerified,
                AuditTargetKind::IdentityHandoff,
                locked.id,
            )
            .detail(json!({ "bundleDigest": locked.bundle_digest, "importId": receipt.id })),
            now,
        )
        .await?;
    }
    txn.commit().await?;
    handoff(db, digest, false).await
}

/// Step 6: scrubs Calling's verifier material (only after a verified hand-off).
/// Returns the number of credentials scrubbed. Idempotent.
pub async fn complete(
    db: &DatabaseConnection,
    op: &OperatorContext,
    digest: &str,
    now: DateTime<Utc>,
) -> PlatformResult<u64> {
    let txn = db.begin().await?;
    let h = handoff(&txn, digest, true).await?;
    if h.verified_at.is_none() {
        return Err(PlatformError::InvalidState(
            "the hand-off is not verified: run `identity verify-handoff` first".to_string(),
        ));
    }
    let scrubbed = txn
        .execute(stmt(
            "UPDATE platform_api_credentials SET verifier = NULL, verifier_algorithm = NULL, \
               verifier_key_version = NULL WHERE verifier IS NOT NULL",
            vec![],
        ))
        .await?
        .rows_affected();
    if h.completed_at.is_none() {
        txn.execute(stmt(
            "UPDATE platform_identity_handoffs SET completed_at = $2 WHERE id = $1",
            vec![h.id.into(), now.into()],
        ))
        .await?;
    }
    operator::record(
        &txn,
        op,
        &AuditEntry::new(
            OperatorAction::IdentityHandoffCompleted,
            AuditTargetKind::IdentityHandoff,
            h.id,
        )
        .detail(json!({ "bundleDigest": h.bundle_digest, "verifiersScrubbed": scrubbed })),
        now,
    )
    .await?;
    txn.commit().await?;
    tracing::info!(
        scrubbed,
        "identity hand-off completed: verifier material removed from Calling"
    );
    Ok(scrubbed)
}

/// Rollback / recovery: restores verifiers from a bundle (Calling's own export or a
/// Platform Access export) into credentials that have none. Integrity-checked; never
/// replaces an existing verifier (the database refuses that too). Returns how many were
/// restored.
pub async fn restore_verifiers(
    db: &DatabaseConnection,
    op: &OperatorContext,
    bundle: &[u8],
    now: DateTime<Utc>,
) -> PlatformResult<u64> {
    let invalid = |m: &str| PlatformError::Validation(format!("invalid identity bundle: {m}"));
    let root: Value = serde_json::from_slice(bundle).map_err(|_| invalid("not JSON"))?;
    if root["bundleVersion"].as_u64() != Some(BUNDLE_VERSION) {
        return Err(invalid("unsupported bundleVersion"));
    }
    let stated = root["digest"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if sha256_hex(canonical_json(&root["body"]).as_bytes()) != stated {
        return Err(invalid(
            "the body does not match its digest (corrupted or edited)",
        ));
    }
    let credentials = root["body"]["credentials"]
        .as_array()
        .ok_or_else(|| invalid("credentials"))?;
    let txn = db.begin().await?;
    let mut restored = 0;
    for c in credentials {
        let id = c["id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| invalid("credential id"))?;
        let verifier = c["verifier"]
            .as_str()
            .filter(|v| v.len() == 64)
            .and_then(|v| hex::decode(v).ok())
            .ok_or_else(|| invalid("credential verifier"))?;
        let algorithm = c["verifierAlgorithm"]
            .as_str()
            .ok_or_else(|| invalid("verifier algorithm"))?;
        let version = c["verifierKeyVersion"]
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| invalid("verifier key version"))?;
        restored += txn
            .execute(stmt(
                "UPDATE platform_api_credentials SET verifier = $2, verifier_algorithm = $3, \
                   verifier_key_version = $4 WHERE id = $1 AND verifier IS NULL",
                vec![id.into(), verifier.into(), algorithm.into(), version.into()],
            ))
            .await?
            .rows_affected();
    }
    operator::record(
        &txn,
        op,
        &AuditEntry {
            action: OperatorAction::IdentityVerifiersRestored,
            target_kind: AuditTargetKind::IdentityHandoff,
            target_id: None,
            tenant_id: None,
            application_id: None,
            environment_id: None,
            detail: json!({ "bundleDigest": stated, "restored": restored }),
        },
        now,
    )
    .await?;
    txn.commit().await?;
    tracing::info!(restored, "credential verifiers restored for a rollback");
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_matches_the_platform_access_form() {
        let v: Value =
            serde_json::from_str(r#"{"b":1,"a":{"d":[{"y":1,"x":2}],"c":"s"}}"#).unwrap();
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"c":"s","d":[{"x":2,"y":1}]},"b":1}"#
        );
    }

    #[test]
    fn verifier_digest_is_order_independent() {
        let a = json!({"id": "a", "verifier": "00", "verifierAlgorithm": "HMAC_SHA256", "verifierKeyVersion": 1});
        let b = json!({"id": "b", "verifier": "11", "verifierAlgorithm": "HMAC_SHA256", "verifierKeyVersion": 2});
        assert_eq!(
            verifier_digest(&[a.clone(), b.clone()]),
            verifier_digest(&[b, a])
        );
    }
}
