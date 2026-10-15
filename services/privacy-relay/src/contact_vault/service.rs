//! Contact Vault operations (§31): create, read, update the value, disable, and the
//! zero-downtime re-encryption that completes an encryption-key rotation.

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    audit::{record, record_refusal, AccessEntry},
    crypto::{self, CryptoError},
    fingerprint::{self, Fingerprint},
    model::ContactRow,
    normalization::{normalize, CanonicalNumber, RawContactValue},
    repository::{self, ContactMetadata},
    ContactActor, ContactOperation, ContactVault, VaultKeys,
};
use crate::{
    error::{ErrorCode, RelayError, RelayResult},
    scope::RelayScope,
};
use sea_orm::TransactionTrait;

use super::keyring::{unreadable_versions, KeyPurpose};

/// Key versions stored rows name, and which of them the keyrings cannot read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RotationStatus {
    pub referenced_encryption: Vec<String>,
    pub referenced_fingerprint: Vec<String>,
    pub unreadable_encryption: Vec<String>,
    pub unreadable_fingerprint: Vec<String>,
}

/// Why the rotation guard refuses (names a version only, never material).
#[derive(Debug, thiserror::Error)]
pub enum RotationGuardError {
    #[error("the contact vault keyrings are not configured")]
    Unconfigured,
    #[error(
        "{purpose} key version {version} is referenced by stored rows but is not in the keyring"
    )]
    Unreadable {
        purpose: &'static str,
        version: String,
    },
    #[error("the referenced key versions cannot be read: {0}")]
    Database(RelayError),
}

/// `POST /v1/privacy-relay/contacts`. Status, verification, fingerprints, key versions and
/// scope are not inputs (unknown members are refused).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateContactRequest {
    pub telephone_number: RawContactValue,
    #[serde(default)]
    pub default_country: Option<String>,
}

/// `PATCH /v1/privacy-relay/contacts/{ref}`: replaces the telephone value.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateContactRequest {
    pub expected_version: i64,
    pub telephone_number: RawContactValue,
    #[serde(default)]
    pub default_country: Option<String>,
}

/// `POST /v1/privacy-relay/contacts/{ref}/disable`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisableContactRequest {
    pub expected_version: i64,
}

/// Progress of one re-encryption batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RewrapReport {
    pub rewrapped: u64,
    pub failed: u64,
    pub remaining: i64,
}

fn crypto_error(e: CryptoError) -> RelayError {
    tracing::error!(error = %e, "contact vault cryptographic failure");
    RelayError::Refused(ErrorCode::InternalError)
}

pub(crate) fn canonicalize(
    raw: &RawContactValue,
    default_country: Option<&str>,
) -> RelayResult<CanonicalNumber> {
    normalize(raw, default_country)
        .map_err(|e| RelayError::Coded(ErrorCode::ContactInvalid, e.message()))
}

pub(crate) fn metadata(number: &CanonicalNumber) -> ContactMetadata {
    ContactMetadata {
        masked_display: number.masked_display(),
        country_iso2: number.country_iso2().map(str::to_string),
    }
}

/// Serializes equality decisions on the ACTIVE fingerprint of the number in this scope
/// (two concurrent creates of the same number: one creates, the other finds it).
async fn lock_equality<C: sea_orm::ConnectionTrait>(
    conn: &C,
    scope: RelayScope,
    fingerprints: &[Fingerprint],
) -> RelayResult<()> {
    let Some(active) = fingerprints.first() else {
        return Err(RelayError::Refused(ErrorCode::InternalError));
    };
    crate::db::advisory_xact_lock(
        conn,
        &format!(
            "cfmeet.privacy-relay.contact-equality:{}:{}:{}:{}:{}",
            scope.tenant_id(),
            scope.application_id(),
            scope.environment_id(),
            active.key_version,
            hex::encode(active.value)
        ),
    )
    .await
}

fn check_version(expected: i64) -> RelayResult<()> {
    if expected < 1 {
        return Err(RelayError::Validation(
            "expectedVersion must be a positive integer.",
        ));
    }
    Ok(())
}

impl ContactVault {
    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }

    /// Creates a protected contact and returns its safe representation.
    pub async fn create(
        &self,
        actor: &ContactActor,
        request: CreateContactRequest,
    ) -> RelayResult<(u16, Value)> {
        let entry = AccessEntry::new(ContactOperation::Create, None);
        match self.create_inner(actor, request).await {
            Ok(r) => Ok(r),
            Err(e) => {
                record_refusal(&self.db, actor, entry, &e, self.now()).await;
                Err(e)
            }
        }
    }

    async fn create_inner(
        &self,
        actor: &ContactActor,
        request: CreateContactRequest,
    ) -> RelayResult<(u16, Value)> {
        let keys = self.keys_for_write()?;
        let number = canonicalize(
            &request.telephone_number,
            request.default_country.as_deref(),
        )?;
        drop(request);
        let (created, row) = self
            .protect(
                keys,
                actor,
                &number,
                AccessEntry::new(ContactOperation::Create, None),
            )
            .await?;
        let now = self.now();
        Ok((
            if created { 201 } else { 200 },
            json!({ "contact": row.view(now)? }),
        ))
    }

    /// Stores `number` for the actor's scope, or returns the contact of that scope that
    /// already protects it (exact equality through the scoped fingerprint; another
    /// scope's contacts are invisible). `true` when a new contact was created.
    pub(crate) async fn protect(
        &self,
        keys: &VaultKeys,
        actor: &ContactActor,
        number: &CanonicalNumber,
        entry: AccessEntry,
    ) -> RelayResult<(bool, ContactRow)> {
        let fingerprints = fingerprint::all(keys.fingerprint(), actor.scope, number);
        let now = self.now();
        let txn = self.db.begin().await?;
        lock_equality(&txn, actor.scope, &fingerprints).await?;
        if let Some(existing) =
            repository::find_by_fingerprints(&txn, actor.scope, &fingerprints).await?
        {
            let row = repository::find(&txn, actor.scope, existing, false)
                .await?
                .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
            let mut entry = entry.noted("DEDUPLICATED");
            entry.contact_reference = Some(existing);
            record(&txn, actor, &entry, now).await?;
            txn.commit().await?;
            return Ok((false, row));
        }
        let id = Uuid::new_v4();
        let sealed =
            crypto::seal(keys.encryption(), actor.scope, id, number).map_err(crypto_error)?;
        let meta = metadata(number);
        repository::insert(&txn, actor.scope, id, &sealed, &meta, now).await?;
        repository::insert_fingerprints(&txn, actor.scope, id, &fingerprints, now).await?;
        let mut entry = entry;
        entry.contact_reference = Some(id);
        record(&txn, actor, &entry, now).await?;
        let row = repository::find(&txn, actor.scope, id, false)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
        txn.commit().await?;
        Ok((true, row))
    }

    /// The safe representation of one contact of the actor's scope.
    pub async fn get(&self, actor: &ContactActor, id: Uuid) -> RelayResult<Value> {
        let now = self.now();
        let entry = AccessEntry::new(ContactOperation::Read, Some(id));
        let Some(row) = repository::find(&self.db, actor.scope, id, false).await? else {
            let e = RelayError::Refused(ErrorCode::ContactNotFound);
            record_refusal(&self.db, actor, entry, &e, now).await;
            return Err(e);
        };
        record(&self.db, actor, &entry, now).await?;
        Ok(json!({ "contact": row.view(now)? }))
    }

    /// Replaces the telephone value (expectedVersion; verification resets to UNVERIFIED).
    pub async fn update(
        &self,
        actor: &ContactActor,
        id: Uuid,
        request: UpdateContactRequest,
    ) -> RelayResult<Value> {
        let entry = AccessEntry::new(ContactOperation::Update, Some(id));
        match self.update_inner(actor, id, request).await {
            Ok(v) => Ok(v),
            Err(e) => {
                record_refusal(&self.db, actor, entry, &e, self.now()).await;
                Err(e)
            }
        }
    }

    async fn update_inner(
        &self,
        actor: &ContactActor,
        id: Uuid,
        request: UpdateContactRequest,
    ) -> RelayResult<Value> {
        check_version(request.expected_version)?;
        let keys = self.keys_for_write()?;
        let number = canonicalize(
            &request.telephone_number,
            request.default_country.as_deref(),
        )?;
        let expected = request.expected_version;
        drop(request);
        let now = self.now();
        let txn = self.db.begin().await?;
        let row = repository::find(&txn, actor.scope, id, true)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::ContactNotFound))?;
        if row.is_erased() {
            return Err(super::erased());
        }
        if row.is_disabled() {
            return Err(RelayError::Refused(ErrorCode::ContactDisabled));
        }
        if row.version != expected {
            return Err(RelayError::Refused(ErrorCode::ContactVersionConflict));
        }
        let fingerprints = fingerprint::all(keys.fingerprint(), actor.scope, &number);
        lock_equality(&txn, actor.scope, &fingerprints).await?;
        if let Some(other) =
            repository::find_by_fingerprints(&txn, actor.scope, &fingerprints).await?
        {
            if other != id {
                return Err(RelayError::Refused(ErrorCode::ContactAlreadyExists));
            }
        }
        let sealed =
            crypto::seal(keys.encryption(), actor.scope, id, &number).map_err(crypto_error)?;
        let meta = metadata(&number);
        drop(number);
        repository::replace_value(&txn, &row, &sealed, &meta, now).await?;
        repository::delete_fingerprints(&txn, id).await?;
        repository::insert_fingerprints(&txn, actor.scope, id, &fingerprints, now).await?;
        record(
            &txn,
            actor,
            &AccessEntry::new(ContactOperation::Update, Some(id)),
            now,
        )
        .await?;
        let row = repository::find(&txn, actor.scope, id, false)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
        txn.commit().await?;
        Ok(json!({ "contact": row.view(now)? }))
    }

    /// Disables a contact (restrictive: allowed while the vault feature is off). A
    /// disabled contact is never resolved or used for new communication.
    pub async fn disable(
        &self,
        actor: &ContactActor,
        id: Uuid,
        request: DisableContactRequest,
    ) -> RelayResult<Value> {
        let entry = AccessEntry::new(ContactOperation::Disable, Some(id));
        match self.disable_inner(actor, id, request).await {
            Ok(v) => Ok(v),
            Err(e) => {
                record_refusal(&self.db, actor, entry, &e, self.now()).await;
                Err(e)
            }
        }
    }

    async fn disable_inner(
        &self,
        actor: &ContactActor,
        id: Uuid,
        request: DisableContactRequest,
    ) -> RelayResult<Value> {
        check_version(request.expected_version)?;
        let now = self.now();
        let txn = self.db.begin().await?;
        let row = repository::find(&txn, actor.scope, id, true)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::ContactNotFound))?;
        if !row.is_disabled() {
            if row.version != request.expected_version {
                return Err(RelayError::Refused(ErrorCode::ContactVersionConflict));
            }
            repository::disable(&txn, id, now).await?;
            // A disabled contact keeps no equality identity: the number can be protected
            // again as a new contact.
            repository::delete_fingerprints(&txn, id).await?;
        }
        record(
            &txn,
            actor,
            &AccessEntry::new(ContactOperation::Disable, Some(id)),
            now,
        )
        .await?;
        let row = repository::find(&txn, actor.scope, id, false)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
        txn.commit().await?;
        Ok(json!({ "contact": row.view(now)? }))
    }

    /// Decrypts a stored contact (resolution and maintenance paths only).
    pub(crate) fn open_row(
        &self,
        keys: &VaultKeys,
        row: &ContactRow,
    ) -> RelayResult<CanonicalNumber> {
        crypto::open(keys.encryption(), row.scope(), row.id, &row.sealed()?).map_err(crypto_error)
    }

    /// One bounded re-encryption batch: rows under a readable, non-active key version are
    /// re-encrypted under the active version (ContactReference, version and every safe
    /// field unchanged). Each batch commits atomically, so a crash loses at most the
    /// uncommitted batch and the next run resumes; concurrent workers take disjoint rows.
    pub async fn rewrap_batch(&self, limit: u64) -> RelayResult<RewrapReport> {
        let keys = self.keys_any()?;
        let active = keys.encryption().current_key().0.as_str().to_string();
        let readable: Vec<String> = keys
            .encryption()
            .versions()
            .iter()
            .map(|v| v.as_str().to_string())
            .collect();
        let now = self.now();
        let txn = self.db.begin().await?;
        let rows =
            repository::lock_for_rewrap(&txn, &active, readable, limit.clamp(1, 1000)).await?;
        let mut report = RewrapReport::default();
        for row in &rows {
            let actor = ContactActor::system(row.scope());
            let opened = match self.open_row(keys, row) {
                Ok(n) => n,
                Err(e) => {
                    report.failed += 1;
                    record_refusal(
                        &txn,
                        &actor,
                        AccessEntry::new(ContactOperation::Reencrypt, Some(row.id))
                            .purpose("KEY_ROTATION"),
                        &e,
                        now,
                    )
                    .await;
                    continue;
                }
            };
            let sealed = crypto::seal(keys.encryption(), row.scope(), row.id, &opened)
                .map_err(crypto_error)?;
            drop(opened);
            repository::rewrap(&txn, row.id, &sealed).await?;
            record(
                &txn,
                &actor,
                &AccessEntry::new(ContactOperation::Reencrypt, Some(row.id))
                    .purpose("KEY_ROTATION"),
                now,
            )
            .await?;
            report.rewrapped += 1;
        }
        txn.commit().await?;
        report.remaining = crate::db::scalar_i64(
            &self.db,
            "SELECT count(*)::bigint FROM protected_contacts WHERE erased_at IS NULL AND encryption_key_version <> $1",
            vec![active.into()],
        )
        .await?;
        if report.rewrapped > 0 || report.failed > 0 {
            tracing::info!(
                rewrapped = report.rewrapped,
                failed = report.failed,
                remaining = report.remaining,
                "contact re-encryption batch"
            );
        }
        Ok(report)
    }

    /// Contacts per encryption key version (rotation evidence: the old version may be
    /// removed from the keyring only once it has no contacts).
    pub async fn key_version_counts(&self) -> RelayResult<Vec<(String, i64)>> {
        Ok(repository::key_version_counts(&self.db)
            .await?
            .into_iter()
            .map(|c| (c.key_version, c.contacts))
            .collect())
    }

    /// Rotation safety guard (§29): every key version a stored contact or fingerprint
    /// names must be readable by the configured keyrings. Called before the service
    /// serves (a keyring that lost a referenced version is a deployment error, not a
    /// runtime surprise) and before a version is retired.
    pub async fn verify_referenced_key_versions(&self) -> Result<(), RotationGuardError> {
        let keys = self
            .keys_any()
            .map_err(|_| RotationGuardError::Unconfigured)?;
        let status = self.rotation_status().await?;
        if let Some(v) = status.unreadable_encryption.first() {
            return Err(RotationGuardError::Unreadable {
                purpose: KeyPurpose::Encryption.as_str(),
                version: v.clone(),
            });
        }
        if let Some(v) = status.unreadable_fingerprint.first() {
            return Err(RotationGuardError::Unreadable {
                purpose: KeyPurpose::Fingerprint.as_str(),
                version: v.clone(),
            });
        }
        let _ = keys;
        Ok(())
    }

    /// Referenced versions against the configured keyrings (rotation-status evidence).
    pub async fn rotation_status(&self) -> Result<RotationStatus, RotationGuardError> {
        let keys = self
            .keys_any()
            .map_err(|_| RotationGuardError::Unconfigured)?;
        let encryption = repository::referenced_encryption_versions(&self.db)
            .await
            .map_err(RotationGuardError::Database)?;
        let fingerprint = repository::referenced_fingerprint_versions(&self.db)
            .await
            .map_err(RotationGuardError::Database)?;
        Ok(RotationStatus {
            unreadable_encryption: unreadable_versions(keys.encryption(), &encryption),
            unreadable_fingerprint: unreadable_versions(keys.fingerprint(), &fingerprint),
            referenced_encryption: encryption,
            referenced_fingerprint: fingerprint,
        })
    }

    /// Whether `id` is an existing, enabled contact of `scope` (Communication Context
    /// participants may only reference such contacts).
    pub async fn exists_enabled(&self, scope: RelayScope, id: Uuid) -> RelayResult<bool> {
        Ok(repository::find(&self.db, scope, id, false)
            .await?
            .is_some_and(|r| !r.is_disabled()))
    }
}

impl ContactVault {
    /// One maintenance sweep (workers): re-encryption batches until a batch is short.
    pub async fn run_sweep(&self) -> RelayResult<()> {
        if self.keys.is_none() {
            // Verification expiry and grant expiry need no key material.
            while self.expire_verifications(REWRAP_BATCH).await? == REWRAP_BATCH {}
            while self.expire_grants(REWRAP_BATCH).await? == REWRAP_BATCH {}
            return Ok(());
        }
        loop {
            let report = self.rewrap_batch(REWRAP_BATCH).await?;
            if report.rewrapped + report.failed < REWRAP_BATCH || report.rewrapped == 0 {
                break;
            }
        }
        loop {
            let report = self.backfill_fingerprints(REWRAP_BATCH).await?;
            if report.backfilled == 0 || report.remaining == 0 {
                break;
            }
        }
        while self.expire_verifications(REWRAP_BATCH).await? == REWRAP_BATCH {}
        while self.expire_grants(REWRAP_BATCH).await? == REWRAP_BATCH {}
        Ok(())
    }
}

const REWRAP_BATCH: u64 = 100;

/// Progress of one fingerprint backfill batch (fingerprint-key rotation, §32).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillReport {
    pub backfilled: u64,
    pub failed: u64,
    pub remaining: i64,
}

impl ContactVault {
    /// Rotation step "backfill": contacts missing a fingerprint under any READABLE
    /// fingerprint-key version get it (decrypt → canonical → HMAC; nothing else is
    /// compared or decrypted). Bounded, SKIP LOCKED, resumable.
    pub async fn backfill_fingerprints(&self, limit: u64) -> RelayResult<BackfillReport> {
        let keys = self.keys_any()?;
        let mut report = BackfillReport::default();
        let now = self.now();
        for version in keys.fingerprint().versions() {
            let txn = self.db.begin().await?;
            let rows =
                repository::lock_missing_fingerprint(&txn, version.as_str(), limit.clamp(1, 1000))
                    .await?;
            for row in &rows {
                let actor = ContactActor::system(row.scope());
                let entry = AccessEntry::new(ContactOperation::FingerprintBackfill, Some(row.id))
                    .purpose("KEY_ROTATION");
                let number = match self.open_row(keys, row) {
                    Ok(n) => n,
                    Err(e) => {
                        report.failed += 1;
                        record_refusal(&txn, &actor, entry, &e, now).await;
                        continue;
                    }
                };
                let Some(fp) =
                    fingerprint::under(keys.fingerprint(), &version, row.scope(), &number)
                else {
                    continue;
                };
                drop(number);
                repository::insert_fingerprints(
                    &txn,
                    row.scope(),
                    row.id,
                    std::slice::from_ref(&fp),
                    now,
                )
                .await?;
                record(&txn, &actor, &entry, now).await?;
                report.backfilled += 1;
            }
            txn.commit().await?;
        }
        for version in keys.fingerprint().versions() {
            report.remaining +=
                repository::missing_fingerprint_count(&self.db, version.as_str()).await?;
        }
        if report.backfilled > 0 || report.failed > 0 {
            tracing::info!(
                backfilled = report.backfilled,
                failed = report.failed,
                remaining = report.remaining,
                "contact fingerprint backfill batch"
            );
        }
        Ok(report)
    }

    /// Rotation step "prune": removes every fingerprint of a NON-active version, only
    /// when every enabled contact already has a fingerprint under the active version (no
    /// dedup gap). Returns the number of removed rows.
    pub async fn prune_fingerprint_version(&self, version: &str) -> RelayResult<u64> {
        let keys = self.keys_any()?;
        let active = keys.fingerprint().current_key().0.as_str().to_string();
        if version == active {
            return Err(RelayError::Validation(
                "The active fingerprint key version cannot be pruned.",
            ));
        }
        let txn = self.db.begin().await?;
        crate::db::advisory_xact_lock(&txn, "cfmeet.privacy-relay.contact-fingerprint-prune")
            .await?;
        if repository::missing_fingerprint_count(&txn, &active).await? > 0 {
            return Err(RelayError::Validation(
                "Some contacts have no fingerprint under the active version yet; run the backfill first.",
            ));
        }
        let removed = repository::delete_fingerprint_version(&txn, version).await?;
        txn.commit().await?;
        Ok(removed)
    }

    /// Fingerprints per key version (rotation evidence).
    pub async fn fingerprint_version_counts(&self) -> RelayResult<Vec<(String, i64)>> {
        Ok(repository::fingerprint_version_counts(&self.db)
            .await?
            .into_iter()
            .map(|c| (c.key_version, c.contacts))
            .collect())
    }
}

/// Progress of one contact-erasure batch (retention, §80).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErasureReport {
    pub erased: Vec<Uuid>,
}

/// Erases (minimizes) up to `limit` contacts DISABLED at or before `cutoff`, inside the
/// caller's transaction: ciphertext, nonce and key version gone, the opaque reference and
/// its digit-free masked display kept, one `ERASE` access-audit row per contact, no key
/// material needed. Idempotent and `SKIP LOCKED` (concurrent workers erase each contact
/// once); contacts under an active retention hold are skipped.
pub async fn erase_disabled_contacts<C: sea_orm::ConnectionTrait>(
    conn: &C,
    cutoff: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
    limit: u64,
) -> RelayResult<ErasureReport> {
    let rows = repository::lock_for_erasure(conn, cutoff, limit.clamp(1, 500)).await?;
    let mut report = ErasureReport::default();
    for row in &rows {
        if !repository::erase(conn, row.id, now).await? {
            continue;
        }
        let actor = ContactActor::system(row.scope());
        record(
            conn,
            &actor,
            &AccessEntry::new(ContactOperation::Erase, Some(row.id)).purpose("DATA_RETENTION"),
            now,
        )
        .await?;
        report.erased.push(row.id);
    }
    Ok(report)
}
