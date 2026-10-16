//! Contact Vault SQL. Every query is scoped by (tenant, application, environment): a
//! foreign ContactReference is indistinguishable from a nonexistent one. No statement
//! ever carries a plaintext value — only ciphertext, nonce, key version and safe metadata
//! are bound as parameters.

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

use super::{
    crypto::{Sealed, ALGORITHM},
    model::{ContactRow, VerificationState, CONTACT_COLUMNS},
};
use crate::{db, error::RelayResult, scope::RelayScope};

/// Safe metadata of a new or updated value.
#[derive(Debug, Clone)]
pub struct ContactMetadata {
    pub masked_display: String,
    pub country_iso2: Option<String>,
}

pub async fn find<C: ConnectionTrait>(
    conn: &C,
    scope: RelayScope,
    id: Uuid,
    lock: bool,
) -> RelayResult<Option<ContactRow>> {
    let sql = format!(
        "SELECT {CONTACT_COLUMNS} FROM protected_contacts \
         WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    db::one(
        conn,
        &sql,
        vec![
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            id.into(),
        ],
    )
    .await
}

pub async fn insert<C: ConnectionTrait>(
    conn: &C,
    scope: RelayScope,
    id: Uuid,
    sealed: &Sealed,
    meta: &ContactMetadata,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        conn,
        "INSERT INTO protected_contacts (id, tenant_id, application_id, environment_id, version, ciphertext, nonce, \
           encryption_algorithm, encryption_key_version, masked_display, country_iso2, voice_capability, \
           verification_state, verification_expires_at, verification_updated_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 1, $5, $6, $7, $8, $9, $10, 'UNKNOWN', $11, NULL, $12, $12, $12)",
        vec![
            id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            sealed.ciphertext.clone().into(),
            sealed.nonce.clone().into(),
            ALGORITHM.into(),
            sealed.key_version.as_str().into(),
            meta.masked_display.clone().into(),
            meta.country_iso2.clone().into(),
            VerificationState::Unverified.as_str().into(),
            now.into(),
        ],
    )
    .await
    .map(|_| ())
}

/// A new telephone value: new ciphertext under the active key, new metadata, version + 1
/// and verification reset to UNVERIFIED (a changed value is never still verified).
pub async fn replace_value<C: ConnectionTrait>(
    conn: &C,
    row: &ContactRow,
    sealed: &Sealed,
    meta: &ContactMetadata,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        conn,
        "UPDATE protected_contacts SET ciphertext = $2, nonce = $3, encryption_key_version = $4, \
           masked_display = $5, country_iso2 = $6, voice_capability = 'UNKNOWN', verification_state = 'UNVERIFIED', \
           verification_expires_at = NULL, verification_updated_at = $7, version = version + 1, updated_at = $7 \
         WHERE id = $1",
        vec![
            row.id.into(),
            sealed.ciphertext.clone().into(),
            sealed.nonce.clone().into(),
            sealed.key_version.as_str().into(),
            meta.masked_display.clone().into(),
            meta.country_iso2.clone().into(),
            now.into(),
        ],
    )
    .await
    .map(|_| ())
}

pub async fn disable<C: ConnectionTrait>(
    conn: &C,
    id: Uuid,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        conn,
        "UPDATE protected_contacts SET disabled_at = $2, version = version + 1, updated_at = $2 WHERE id = $1",
        vec![id.into(), now.into()],
    )
    .await
    .map(|_| ())
}

/// Re-encryption keeps the resource version and every safe field: only the ciphertext,
/// nonce and key version change.
pub async fn rewrap<C: ConnectionTrait>(conn: &C, id: Uuid, sealed: &Sealed) -> RelayResult<()> {
    db::exec(
        conn,
        "UPDATE protected_contacts SET ciphertext = $2, nonce = $3, encryption_key_version = $4 WHERE id = $1",
        vec![
            id.into(),
            sealed.ciphertext.clone().into(),
            sealed.nonce.clone().into(),
            sealed.key_version.as_str().into(),
        ],
    )
    .await
    .map(|_| ())
}

/// Up to `limit` rows still encrypted under a readable non-active version, locked for
/// this worker (`SKIP LOCKED`: concurrent workers take disjoint batches). Erased rows
/// hold no ciphertext and are never selected.
pub async fn lock_for_rewrap<C: ConnectionTrait>(
    conn: &C,
    active: &str,
    readable: Vec<String>,
    limit: u64,
) -> RelayResult<Vec<ContactRow>> {
    db::all(
        conn,
        &format!(
            "SELECT {CONTACT_COLUMNS} FROM protected_contacts \
             WHERE erased_at IS NULL AND encryption_key_version <> $1 AND encryption_key_version = ANY($2) \
             ORDER BY encryption_key_version, id LIMIT $3 FOR UPDATE SKIP LOCKED"
        ),
        vec![active.into(), readable.into(), (limit as i64).into()],
    )
    .await
}

#[derive(Debug, Clone, sea_orm::FromQueryResult)]
pub struct KeyVersionCount {
    pub key_version: String,
    pub contacts: i64,
}

/// Contacts per encryption key version (rotation progress evidence; erased rows name
/// no version).
pub async fn key_version_counts<C: ConnectionTrait>(conn: &C) -> RelayResult<Vec<KeyVersionCount>> {
    db::all(
        conn,
        "SELECT encryption_key_version AS key_version, count(*)::bigint AS contacts \
         FROM protected_contacts WHERE erased_at IS NULL \
         GROUP BY encryption_key_version ORDER BY encryption_key_version",
        vec![],
    )
    .await
}

#[derive(Debug, Clone, sea_orm::FromQueryResult)]
struct ReferencedVersion {
    key_version: String,
}

/// Every encryption key version a stored (non-erased) contact names (§29 rotation
/// guard: each must be readable by the keyring).
pub async fn referenced_encryption_versions<C: ConnectionTrait>(
    conn: &C,
) -> RelayResult<Vec<String>> {
    let rows: Vec<ReferencedVersion> = db::all(
        conn,
        "SELECT DISTINCT encryption_key_version AS key_version FROM protected_contacts \
         WHERE erased_at IS NULL ORDER BY encryption_key_version",
        vec![],
    )
    .await?;
    Ok(rows.into_iter().map(|r| r.key_version).collect())
}

/// Every fingerprint key version a stored fingerprint names.
pub async fn referenced_fingerprint_versions<C: ConnectionTrait>(
    conn: &C,
) -> RelayResult<Vec<String>> {
    let rows: Vec<ReferencedVersion> = db::all(
        conn,
        "SELECT DISTINCT fingerprint_key_version AS key_version FROM contact_fingerprints \
         ORDER BY fingerprint_key_version",
        vec![],
    )
    .await?;
    Ok(rows.into_iter().map(|r| r.key_version).collect())
}

// ---------------------------------------------------------------------------
// Retention minimization (§80): erasure of a disabled contact's value.
// ---------------------------------------------------------------------------

/// Up to `limit` contacts DISABLED at or before `cutoff` and not yet erased, locked for
/// this worker (`SKIP LOCKED`), skipping every contact an active retention hold covers.
pub async fn lock_for_erasure<C: ConnectionTrait>(
    conn: &C,
    cutoff: DateTime<Utc>,
    limit: u64,
) -> RelayResult<Vec<ContactRow>> {
    db::all(
        conn,
        &format!(
            "SELECT {CONTACT_COLUMNS} FROM protected_contacts c \
             WHERE c.disabled_at IS NOT NULL AND c.disabled_at <= $1 AND c.erased_at IS NULL \
               AND NOT privacy_relay_retention_held($2, c.id, c.tenant_id, c.application_id, c.environment_id) \
             ORDER BY c.disabled_at, c.id LIMIT $3 FOR UPDATE SKIP LOCKED"
        ),
        vec![
            cutoff.into(),
            crate::retention::DataClass::ProtectedContactValue
                .as_str()
                .into(),
            (limit as i64).into(),
        ],
    )
    .await
}

/// Erases one contact's value: ciphertext, nonce and key version become NULL, the masked
/// display keeps only the country calling code, the version advances. Idempotent
/// (`erased_at IS NULL`); returns whether this call erased it.
pub async fn erase<C: ConnectionTrait>(
    conn: &C,
    id: Uuid,
    now: DateTime<Utc>,
) -> RelayResult<bool> {
    let n = db::exec(
        conn,
        "UPDATE protected_contacts SET ciphertext = NULL, nonce = NULL, encryption_key_version = NULL, \
           masked_display = COALESCE(substring(masked_display from '^\\+[0-9]{1,3}') || '••••••', '•••'), \
           erased_at = $2, version = version + 1, updated_at = $2 \
         WHERE id = $1 AND erased_at IS NULL AND disabled_at IS NOT NULL",
        vec![id.into(), now.into()],
    )
    .await?;
    Ok(n == 1)
}

// ---------------------------------------------------------------------------
// Equality fingerprints (§32).
// ---------------------------------------------------------------------------

use super::fingerprint::Fingerprint;

pub async fn insert_fingerprints<C: ConnectionTrait>(
    conn: &C,
    scope: RelayScope,
    contact: Uuid,
    fingerprints: &[Fingerprint],
    now: DateTime<Utc>,
) -> RelayResult<()> {
    for f in fingerprints {
        db::exec(
            conn,
            "INSERT INTO contact_fingerprints (contact_reference, tenant_id, application_id, environment_id, \
               fingerprint_key_version, fingerprint, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            vec![
                contact.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
                f.key_version.as_str().into(),
                f.value.to_vec().into(),
                now.into(),
            ],
        )
        .await?;
    }
    Ok(())
}

pub async fn delete_fingerprints<C: ConnectionTrait>(conn: &C, contact: Uuid) -> RelayResult<u64> {
    db::exec(
        conn,
        "DELETE FROM contact_fingerprints WHERE contact_reference = $1",
        vec![contact.into()],
    )
    .await
}

/// The contact of `scope` holding any of `fingerprints` (exact equality; never a scan or
/// a decryption of other rows).
pub async fn find_by_fingerprints<C: ConnectionTrait>(
    conn: &C,
    scope: RelayScope,
    fingerprints: &[Fingerprint],
) -> RelayResult<Option<Uuid>> {
    #[derive(sea_orm::FromQueryResult)]
    struct Hit {
        contact_reference: Uuid,
    }
    for f in fingerprints {
        let hit: Option<Hit> = db::one(
            conn,
            "SELECT contact_reference FROM contact_fingerprints \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND fingerprint_key_version = $4 AND fingerprint = $5",
            vec![
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
                f.key_version.as_str().into(),
                f.value.to_vec().into(),
            ],
        )
        .await?;
        if let Some(h) = hit {
            return Ok(Some(h.contact_reference));
        }
    }
    Ok(None)
}

/// Up to `limit` enabled contacts without a fingerprint under `version`, locked for
/// this worker (`SKIP LOCKED`).
pub async fn lock_missing_fingerprint<C: ConnectionTrait>(
    conn: &C,
    version: &str,
    limit: u64,
) -> RelayResult<Vec<ContactRow>> {
    db::all(
        conn,
        &format!(
            "SELECT {CONTACT_COLUMNS} FROM protected_contacts c \
             WHERE c.disabled_at IS NULL AND NOT EXISTS ( \
               SELECT 1 FROM contact_fingerprints f WHERE f.contact_reference = c.id AND f.fingerprint_key_version = $1) \
             ORDER BY c.id LIMIT $2 FOR UPDATE SKIP LOCKED"
        ),
        vec![version.into(), (limit as i64).into()],
    )
    .await
}

/// Enabled contacts without a fingerprint under `version`.
pub async fn missing_fingerprint_count<C: ConnectionTrait>(
    conn: &C,
    version: &str,
) -> RelayResult<i64> {
    db::scalar_i64(
        conn,
        "SELECT count(*)::bigint FROM protected_contacts c WHERE c.disabled_at IS NULL AND NOT EXISTS ( \
           SELECT 1 FROM contact_fingerprints f WHERE f.contact_reference = c.id AND f.fingerprint_key_version = $1)",
        vec![version.into()],
    )
    .await
}

/// Fingerprints per key version (rotation evidence).
pub async fn fingerprint_version_counts<C: ConnectionTrait>(
    conn: &C,
) -> RelayResult<Vec<KeyVersionCount>> {
    db::all(
        conn,
        "SELECT fingerprint_key_version AS key_version, count(*)::bigint AS contacts \
         FROM contact_fingerprints GROUP BY fingerprint_key_version ORDER BY fingerprint_key_version",
        vec![],
    )
    .await
}

pub async fn delete_fingerprint_version<C: ConnectionTrait>(
    conn: &C,
    version: &str,
) -> RelayResult<u64> {
    db::exec(
        conn,
        "DELETE FROM contact_fingerprints WHERE fingerprint_key_version = $1",
        vec![version.into()],
    )
    .await
}

/// Whether `id` is an enabled contact of `scope`, share-locked for the rest of the
/// transaction (a Communication Context may only reference such contacts; a foreign or
/// unknown ContactReference is the same `false`).
pub async fn lock_enabled_in_scope<C: ConnectionTrait>(
    conn: &C,
    scope: RelayScope,
    id: Uuid,
) -> RelayResult<bool> {
    Ok(db::scalar_i64(
        conn,
        "SELECT count(*)::bigint FROM (SELECT 1 FROM protected_contacts \
           WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4 \
             AND disabled_at IS NULL FOR SHARE) q",
        vec![
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            id.into(),
        ],
    )
    .await?
        == 1)
}
