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
/// this worker (`SKIP LOCKED`: concurrent workers take disjoint batches).
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
             WHERE encryption_key_version <> $1 AND encryption_key_version = ANY($2) \
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

/// Contacts per encryption key version (rotation progress evidence).
pub async fn key_version_counts<C: ConnectionTrait>(conn: &C) -> RelayResult<Vec<KeyVersionCount>> {
    db::all(
        conn,
        "SELECT encryption_key_version AS key_version, count(*)::bigint AS contacts \
         FROM protected_contacts GROUP BY encryption_key_version ORDER BY encryption_key_version",
        vec![],
    )
    .await
}
