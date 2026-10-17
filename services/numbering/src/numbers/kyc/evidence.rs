//! Protected KYC evidence (Part IV Workstream F §6).
//!
//! * Content never touches Numbering's database, logs, events or ordinary responses: the
//!   bytes are sealed with AES-256-GCM under the deployment's evidence key
//!   (`cf_webhooks::secret_box`, associated data = the evidence id, so a blob copied to
//!   another reference does not open) and stored as a private object
//!   (`cf_object_storage`, server-side encryption requested by the store configuration).
//!   The database keeps an opaque object key, the content type, size, SHA-256 of the
//!   plaintext (integrity), the key id and the retention deadline.
//! * Without a configured key AND store, evidence is refused (`EVIDENCE_STORAGE_UNAVAILABLE`)
//!   — never stored unprotected.
//! * Content is minimal: PDF / JPEG / PNG only, ≤ 5 MiB, magic bytes must match the type.
//! * [`EvidenceBytes`] redacts its `Debug` output and zeroizes on drop.

use std::{fmt, sync::Arc};

use base64::Engine;
use cf_object_storage::{sha256_hex, ObjectStore, StorageError};
use cf_webhooks::secret_box::SecretBox;
use zeroize::Zeroizing;

use crate::numbers::{DomainError, DomainResult};

/// Largest accepted evidence object.
pub const MAX_EVIDENCE_BYTES: usize = 5 * 1024 * 1024;

pub const ACCEPTED_CONTENT_TYPES: &[&str] = &["application/pdf", "image/jpeg", "image/png"];

/// Evidence content (redacted `Debug`, zeroized on drop).
pub struct EvidenceBytes(Zeroizing<Vec<u8>>);

impl EvidenceBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for EvidenceBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EvidenceBytes(<redacted {} bytes>)", self.0.len())
    }
}

/// Validates content type, size and magic bytes (minimum-data handling).
pub fn check_content(content_type: &str, bytes: &[u8]) -> DomainResult<()> {
    if !ACCEPTED_CONTENT_TYPES.contains(&content_type) {
        return Err(DomainError::Validation(
            "evidence is PDF, JPEG or PNG".into(),
        ));
    }
    if bytes.is_empty() || bytes.len() > MAX_EVIDENCE_BYTES {
        return Err(DomainError::Validation(
            "evidence is 1 byte to 5 MiB".into(),
        ));
    }
    let magic_ok = match content_type {
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        _ => false,
    };
    if !magic_ok {
        return Err(DomainError::Validation(
            "the evidence content does not match its type".into(),
        ));
    }
    Ok(())
}

/// The protected store of evidence objects.
pub struct EvidenceVault {
    store: Arc<dyn ObjectStore>,
    sealing: Option<Arc<SecretBox>>,
}

impl fmt::Debug for EvidenceVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvidenceVault")
            .field(
                "sealing",
                &self.sealing.as_ref().map(|s| s.current_kid().to_string()),
            )
            .finish_non_exhaustive()
    }
}

fn unavailable() -> DomainError {
    DomainError::precondition(
        "EVIDENCE_STORAGE_UNAVAILABLE",
        "protected evidence storage is not available",
    )
}

impl EvidenceVault {
    pub fn new(store: Arc<dyn ObjectStore>, sealing: Option<Arc<SecretBox>>) -> Self {
        Self { store, sealing }
    }

    /// No store, no key: every evidence operation is refused.
    pub fn unconfigured() -> Self {
        Self::new(Arc::new(cf_object_storage::UnconfiguredStore), None)
    }

    pub fn is_configured(&self) -> bool {
        self.sealing.is_some()
    }

    pub fn object_key(case_id: uuid::Uuid, evidence_id: uuid::Uuid) -> String {
        format!("kyc-evidence/{case_id}/{evidence_id}")
    }

    /// Seals and stores; returns the key id used.
    pub async fn put(
        &self,
        key: &str,
        evidence_id: uuid::Uuid,
        bytes: &[u8],
    ) -> DomainResult<String> {
        let sealing = self.sealing.as_ref().ok_or_else(unavailable)?;
        let encoded = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(bytes));
        let sealed = sealing.seal(&encoded, &evidence_id.to_string());
        match self
            .store
            .put(key, sealed.into_bytes(), "application/octet-stream")
            .await
        {
            Ok(_) => Ok(sealing.current_kid().to_string()),
            Err(StorageError::NotConfigured) => Err(unavailable()),
            Err(_) => Err(DomainError::Database {
                summary: "evidence storage unavailable".into(),
                connectivity: true,
            }),
        }
    }

    /// Reads, opens and integrity-checks an object.
    pub async fn get(
        &self,
        key: &str,
        evidence_id: uuid::Uuid,
        sha256: &str,
    ) -> DomainResult<Option<EvidenceBytes>> {
        let sealing = self.sealing.as_ref().ok_or_else(unavailable)?;
        let raw = match self.store.get(key, MAX_EVIDENCE_BYTES * 2).await {
            Ok(Some(raw)) => raw,
            Ok(None) => return Ok(None),
            Err(StorageError::NotConfigured) => return Err(unavailable()),
            Err(_) => {
                return Err(DomainError::Database {
                    summary: "evidence storage unavailable".into(),
                    connectivity: true,
                })
            }
        };
        let sealed = String::from_utf8(raw).map_err(|_| {
            DomainError::precondition("EVIDENCE_INTEGRITY", "corrupt evidence object")
        })?;
        let encoded = Zeroizing::new(sealing.open(&sealed, &evidence_id.to_string()).map_err(
            |_| {
                DomainError::precondition(
                    "EVIDENCE_INTEGRITY",
                    "evidence does not open under this reference",
                )
            },
        )?);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| {
                DomainError::precondition("EVIDENCE_INTEGRITY", "corrupt evidence object")
            })?;
        if sha256_hex(&bytes) != sha256 {
            return Err(DomainError::precondition(
                "EVIDENCE_INTEGRITY",
                "evidence integrity check failed",
            ));
        }
        Ok(Some(EvidenceBytes::new(bytes)))
    }

    /// Deletes an object (idempotent).
    pub async fn delete(&self, key: &str) -> DomainResult<()> {
        self.store
            .delete(key)
            .await
            .map_err(|_| DomainError::Database {
                summary: "evidence storage unavailable".into(),
                connectivity: true,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_is_minimal_and_typed_and_never_printed() {
        assert!(check_content("application/pdf", b"%PDF-1.7 ...").is_ok());
        assert!(check_content(
            "image/png",
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]
        )
        .is_ok());
        assert!(check_content("image/jpeg", &[0xFF, 0xD8, 0xFF, 0xE0]).is_ok());
        for (t, b) in [
            ("application/pdf", b"not a pdf".as_slice()),
            ("text/plain", b"hello".as_slice()),
            ("image/png", b"%PDF-1.7".as_slice()),
            ("application/pdf", b"".as_slice()),
        ] {
            assert!(check_content(t, b).is_err(), "{t}");
        }
        let e = EvidenceBytes::new(b"passport number X123".to_vec());
        assert_eq!(format!("{e:?}"), "EvidenceBytes(<redacted 20 bytes>)");
    }
}
