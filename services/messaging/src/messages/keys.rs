//! Envelope encryption of MANAGED content (Part V Appendix J).
//!
//! * A **key provider** ([`ManagedKeyProvider`]) owns the key-encryption keys (KEKs). The
//!   built-in provider is the deployment's key-ring file (`MESSAGING_MANAGED_KEYRING_FILE`,
//!   outside PostgreSQL); an external KMS / HSM plugs in through the same interface
//!   ([`ManagedKeyring::with_provider`]). KEKs only ever wrap and unwrap DEKs.
//! * Each application environment has one ACTIVE **data-encryption key** (DEK): 256 random
//!   bits, stored only WRAPPED (`messaging_managed_deks`), its wrapping bound to the
//!   tenant, application, environment and DEK id. A DEK is created in its own committed
//!   transaction before anything is sealed under it, so no content can reference a DEK
//!   that does not exist. Unwrapped DEKs live only in process memory (zeroized on drop).
//! * Content is sealed with XChaCha20-Poly1305 (random 192-bit nonce) under the
//!   environment's DEK; the associated data binds tenant, application, environment,
//!   conversation and message (or record) id. Key id `dek:<id>`. Content sealed before
//!   DEKs existed carries a key-ring key id and keeps opening with the ring.
//! * Every decryption names its **purpose** ([`DecryptPurpose`]), counted per purpose;
//!   administrative purposes are additionally audited by their operations (export,
//!   report review). There is no break-glass path that opens content outside these
//!   purposes.

use std::{collections::HashMap, fmt, path::Path, sync::Arc};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng, Payload},
    AeadCore, XChaCha20Poly1305, XNonce,
};
use sea_orm::{DatabaseConnection, FromQueryResult};
use tokio::sync::RwLock;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    db,
    error::{Error, ErrorCode, Result},
};

const SEAL_LABEL: &str = "cfmeet-messaging-managed:v1";
const DEK_LABEL: &str = "cfmeet-messaging-dek:v1";
const DEK_PREFIX: &str = "dek:";

/// Why MANAGED content is being opened (Appendix J: a documented service purpose).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecryptPurpose {
    /// A member reads the conversation (rendering, permalinks, replay, realtime).
    MemberRead,
    /// Idempotent retry comparison of a send.
    Idempotency,
    /// Revision history shown to members under policy.
    RevisionHistory,
    /// Compliance export (administration, audited by the export).
    ComplianceExport,
    /// Moderation review of reported content (administration, audited by the review).
    ModerationReview,
    /// Reporter-disclosed evidence of a report (administration, audited by the review).
    ReportEvidence,
    /// Carrier delivery of an SMS-bridged message.
    SmsDelivery,
}

impl DecryptPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            DecryptPurpose::MemberRead => "MEMBER_READ",
            DecryptPurpose::Idempotency => "IDEMPOTENCY",
            DecryptPurpose::RevisionHistory => "REVISION_HISTORY",
            DecryptPurpose::ComplianceExport => "COMPLIANCE_EXPORT",
            DecryptPurpose::ModerationReview => "MODERATION_REVIEW",
            DecryptPurpose::ReportEvidence => "REPORT_EVIDENCE",
            DecryptPurpose::SmsDelivery => "SMS_DELIVERY",
        }
    }
}

/// A wrapped data-encryption key.
#[derive(Debug, Clone)]
pub struct WrappedKey {
    pub kek_id: String,
    pub nonce: Vec<u8>,
    pub wrapped: Vec<u8>,
}

/// The key-encryption side of MANAGED envelope encryption (a key-ring file, a KMS, an HSM).
#[async_trait]
pub trait ManagedKeyProvider: Send + Sync {
    /// Wraps `dek` under the provider's CURRENT key-encryption key, bound to `aad`.
    async fn wrap(&self, dek: &[u8; 32], aad: &[u8]) -> Result<WrappedKey>;
    /// Unwraps a key this provider wrapped (any of its KEK versions); `None` when it
    /// cannot (unknown KEK, wrong binding, tampered).
    async fn unwrap(&self, key: &WrappedKey, aad: &[u8]) -> Option<Zeroizing<[u8; 32]>>;
}

/// The deployment's key-ring file: `<key id> <base64url 32-byte key>` per line, the FIRST
/// line active. Its keys are KEKs for DEKs, and (for content sealed before DEKs existed)
/// the keys of that legacy content.
struct FileKeys {
    active: String,
    keys: HashMap<String, Zeroizing<[u8; 32]>>,
}

fn aead_seal(key: &[u8; 32], aad: &[u8], msg: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let sealed = cipher
        .encrypt(&nonce, Payload { msg, aad })
        .map_err(|_| Error::code(ErrorCode::InternalError))?;
    Ok((sealed, nonce.to_vec()))
}

fn aead_open(key: &[u8; 32], aad: &[u8], sealed: &[u8], nonce: &[u8]) -> Option<Vec<u8>> {
    let nonce: [u8; 24] = nonce.try_into().ok()?;
    XChaCha20Poly1305::new(key.into())
        .decrypt(&XNonce::from(nonce), Payload { msg: sealed, aad })
        .ok()
}

#[async_trait]
impl ManagedKeyProvider for FileKeys {
    async fn wrap(&self, dek: &[u8; 32], aad: &[u8]) -> Result<WrappedKey> {
        let kek = self
            .keys
            .get(&self.active)
            .ok_or(Error::code(ErrorCode::InternalError))?;
        let (wrapped, nonce) = aead_seal(kek, aad, dek)?;
        Ok(WrappedKey {
            kek_id: format!("file:{}", self.active),
            nonce,
            wrapped,
        })
    }

    async fn unwrap(&self, key: &WrappedKey, aad: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
        let kek = self.keys.get(key.kek_id.strip_prefix("file:")?)?;
        let plain = Zeroizing::new(aead_open(kek, aad, &key.wrapped, &key.nonce)?);
        let dek: [u8; 32] = plain.as_slice().try_into().ok()?;
        Some(Zeroizing::new(dek))
    }
}

/// The binding of one sealed MANAGED value.
pub(crate) struct SealScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub conversation_id: Uuid,
    pub message_id: Uuid,
}

pub(crate) struct Sealed {
    pub sealed: Vec<u8>,
    pub nonce: Vec<u8>,
    pub key_id: String,
}

/// The environment a sealed value belongs to (DEK selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

#[derive(Default)]
struct Cache {
    active: HashMap<Uuid, Uuid>,
    deks: HashMap<Uuid, Arc<Zeroizing<[u8; 32]>>>,
}

/// MANAGED content keys: the key provider, the deployment key ring and the DEK cache.
pub struct ManagedKeyring {
    file: Arc<FileKeys>,
    provider: Arc<dyn ManagedKeyProvider>,
    cache: RwLock<Cache>,
}

impl fmt::Debug for ManagedKeyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManagedKeyring")
            .field("active", &self.file.active)
            .field("keys", &self.file.keys.len())
            .finish()
    }
}

fn valid_key_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[derive(Debug, FromQueryResult)]
struct DekRow {
    id: Uuid,
    kek_id: String,
    wrap_nonce: Vec<u8>,
    wrapped_dek: Vec<u8>,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
}

fn dek_aad(scope: &KeyScope, id: Uuid) -> Vec<u8> {
    format!(
        "{DEK_LABEL}:{}:{}:{}:{}",
        scope.tenant_id, scope.application_id, scope.environment_id, id
    )
    .into_bytes()
}

impl ManagedKeyring {
    fn from_file(file: FileKeys) -> Self {
        let file = Arc::new(file);
        Self {
            provider: file.clone(),
            file,
            cache: RwLock::new(Cache::default()),
        }
    }

    /// Parses a key-ring file: one `<key id> <base64url 32-byte key>` per line; the
    /// FIRST line is the active key. Errors never echo key material.
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let mut keys = HashMap::new();
        let mut active = None;
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let (id, key) = line
                .split_once(char::is_whitespace)
                .ok_or("each line must be `<key id> <base64url key>`")?;
            if !valid_key_id(id) {
                return Err("invalid key id (1-32 of [A-Za-z0-9._-])".into());
            }
            let bytes = Zeroizing::new(
                URL_SAFE_NO_PAD
                    .decode(key.trim())
                    .map_err(|_| "keys must be unpadded base64url")?,
            );
            let key: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| "keys must be exactly 32 bytes")?;
            if keys.insert(id.to_string(), Zeroizing::new(key)).is_some() {
                return Err("duplicate key id".into());
            }
            active.get_or_insert_with(|| id.to_string());
        }
        Ok(Self::from_file(FileKeys {
            active: active.ok_or("the key ring is empty")?,
            keys,
        }))
    }

    pub fn load(path: &Path) -> std::result::Result<Self, String> {
        let text = Zeroizing::new(
            std::fs::read_to_string(path).map_err(|_| "cannot be read".to_string())?,
        );
        Self::parse(&text)
    }

    /// A random single-key ring (tests and local development only).
    pub fn generate(id: &str) -> Self {
        let mut keys = HashMap::new();
        keys.insert(
            id.to_string(),
            Zeroizing::new(XChaCha20Poly1305::generate_key(&mut OsRng).into()),
        );
        Self::from_file(FileKeys {
            active: id.to_string(),
            keys,
        })
    }

    /// Wraps DEKs with an external key provider (KMS / HSM) instead of the key-ring file.
    /// Legacy content sealed directly under the key ring keeps opening with the ring.
    pub fn with_provider(mut self, provider: Arc<dyn ManagedKeyProvider>) -> Self {
        self.provider = provider;
        self
    }

    async fn unwrap_row(&self, row: &DekRow) -> Option<Arc<Zeroizing<[u8; 32]>>> {
        let scope = KeyScope {
            tenant_id: row.tenant_id,
            application_id: row.application_id,
            environment_id: row.environment_id,
        };
        let key = self
            .provider
            .unwrap(
                &WrappedKey {
                    kek_id: row.kek_id.clone(),
                    nonce: row.wrap_nonce.clone(),
                    wrapped: row.wrapped_dek.clone(),
                },
                &dek_aad(&scope, row.id),
            )
            .await?;
        let key = Arc::new(key);
        self.cache.write().await.deks.insert(row.id, key.clone());
        Some(key)
    }

    /// The environment's ACTIVE DEK, created (and committed) on first use.
    async fn active_dek(
        &self,
        pool: &DatabaseConnection,
        scope: &KeyScope,
    ) -> Result<(Uuid, Arc<Zeroizing<[u8; 32]>>)> {
        {
            let c = self.cache.read().await;
            if let Some(id) = c.active.get(&scope.environment_id) {
                if let Some(k) = c.deks.get(id) {
                    return Ok((*id, k.clone()));
                }
            }
        }
        const COLUMNS: &str =
            "id, kek_id, wrap_nonce, wrapped_dek, tenant_id, application_id, environment_id";
        let select = format!(
            "SELECT {COLUMNS} FROM messaging_managed_deks WHERE environment_id = $1 AND status = 'ACTIVE'"
        );
        let mut row: Option<DekRow> =
            db::one(pool, &select, vec![scope.environment_id.into()]).await?;
        if row.is_none() {
            let id = Uuid::new_v4();
            let dek = Zeroizing::new(XChaCha20Poly1305::generate_key(&mut OsRng).into());
            let w = self.provider.wrap(&dek, &dek_aad(scope, id)).await?;
            // Its own statement on the pool (autocommit): committed before any content
            // is sealed under it. A concurrent creator wins; its DEK is used.
            db::exec(
                pool,
                "INSERT INTO messaging_managed_deks (id, tenant_id, application_id, environment_id, kek_id, wrap_nonce, \
                   wrapped_dek, status, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, 'ACTIVE', now()) \
                 ON CONFLICT (environment_id) WHERE status = 'ACTIVE' DO NOTHING",
                vec![
                    id.into(),
                    scope.tenant_id.into(),
                    scope.application_id.into(),
                    scope.environment_id.into(),
                    w.kek_id.into(),
                    w.nonce.into(),
                    w.wrapped.into(),
                ],
            )
            .await?;
            row = db::one(pool, &select, vec![scope.environment_id.into()]).await?;
            crate::metrics::inc("messaging_managed_deks_created_total", &[]);
        }
        let row = row.ok_or(Error::code(ErrorCode::InternalError))?;
        if (row.tenant_id, row.application_id) != (scope.tenant_id, scope.application_id) {
            return Err(Error::code(ErrorCode::InternalError));
        }
        let key = self.unwrap_row(&row).await.ok_or(Error::reason(
            ErrorCode::ServiceUnavailable,
            "MANAGED_KEY_UNAVAILABLE",
        ))?;
        self.cache
            .write()
            .await
            .active
            .insert(scope.environment_id, row.id);
        Ok((row.id, key))
    }

    /// A DEK by id (any status), for opening content.
    async fn dek(&self, pool: &DatabaseConnection, id: Uuid) -> Option<Arc<Zeroizing<[u8; 32]>>> {
        if let Some(k) = self.cache.read().await.deks.get(&id) {
            return Some(k.clone());
        }
        let row: DekRow = db::one(
            pool,
            "SELECT id, kek_id, wrap_nonce, wrapped_dek, tenant_id, application_id, environment_id \
               FROM messaging_managed_deks WHERE id = $1",
            vec![id.into()],
        )
        .await
        .ok()??;
        self.unwrap_row(&row).await
    }

    fn content_aad(scope: &SealScope) -> Vec<u8> {
        format!(
            "{SEAL_LABEL}:{}:{}:{}:{}:{}",
            scope.tenant_id,
            scope.application_id,
            scope.environment_id,
            scope.conversation_id,
            scope.message_id
        )
        .into_bytes()
    }

    /// Seals arbitrary bytes under the environment's DEK, bound to `aad`.
    async fn seal_bytes(
        &self,
        pool: &DatabaseConnection,
        scope: &KeyScope,
        aad: &[u8],
        data: &[u8],
    ) -> Result<Sealed> {
        let (id, key) = self.active_dek(pool, scope).await?;
        let (sealed, nonce) = aead_seal(&key, aad, data)?;
        Ok(Sealed {
            sealed,
            nonce,
            key_id: format!("{DEK_PREFIX}{}", id.simple()),
        })
    }

    async fn open_bytes(
        &self,
        pool: &DatabaseConnection,
        environment_id: Uuid,
        aad: &[u8],
        sealed: &[u8],
        nonce: &[u8],
        key_id: &str,
        purpose: DecryptPurpose,
    ) -> Option<Vec<u8>> {
        let plain = match key_id.strip_prefix(DEK_PREFIX) {
            Some(id) => {
                let key = self.dek(pool, Uuid::parse_str(id).ok()?).await?;
                // A DEK belongs to one environment; its cache entry was unwrapped with
                // that binding, and the content AAD binds the environment again.
                let _ = environment_id;
                aead_open(&key, aad, sealed, nonce)?
            }
            None => aead_open(self.file.keys.get(key_id)?, aad, sealed, nonce)?,
        };
        crate::metrics::inc(
            "messaging_managed_decrypt_total",
            &[("purpose", purpose.as_str())],
        );
        Some(plain)
    }

    /// Seals MANAGED text for one message.
    pub(crate) async fn seal(
        &self,
        pool: &DatabaseConnection,
        scope: &SealScope,
        text: &str,
    ) -> Result<Sealed> {
        self.seal_bytes(
            pool,
            &KeyScope {
                tenant_id: scope.tenant_id,
                application_id: scope.application_id,
                environment_id: scope.environment_id,
            },
            &Self::content_aad(scope),
            text.as_bytes(),
        )
        .await
    }

    /// Opens MANAGED text of one message for `purpose`.
    pub(crate) async fn open(
        &self,
        pool: &DatabaseConnection,
        scope: &SealScope,
        sealed: &[u8],
        nonce: &[u8],
        key_id: &str,
        purpose: DecryptPurpose,
    ) -> Option<String> {
        let plain = self
            .open_bytes(
                pool,
                scope.environment_id,
                &Self::content_aad(scope),
                sealed,
                nonce,
                key_id,
                purpose,
            )
            .await?;
        String::from_utf8(plain).ok()
    }

    /// Seals a server-held record other than message text (e.g. evidence a reporter
    /// voluntarily disclosed) under the environment's DEK, bound to `aad`.
    /// Returns `(sealed, nonce, key id)`.
    pub async fn seal_record(
        &self,
        pool: &DatabaseConnection,
        scope: &KeyScope,
        aad: &[u8],
        data: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>, String)> {
        let s = self.seal_bytes(pool, scope, aad, data).await?;
        Ok((s.sealed, s.nonce, s.key_id))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn open_record(
        &self,
        pool: &DatabaseConnection,
        environment_id: Uuid,
        aad: &[u8],
        sealed: &[u8],
        nonce: &[u8],
        key_id: &str,
        purpose: DecryptPurpose,
    ) -> Option<Vec<u8>> {
        self.open_bytes(pool, environment_id, aad, sealed, nonce, key_id, purpose)
            .await
    }

    /// Whether `key_id` names a wrapped DEK (envelope encryption) rather than a key-ring key.
    pub fn is_envelope_key(key_id: &str) -> bool {
        key_id.starts_with(DEK_PREFIX)
    }

    #[cfg(test)]
    pub(crate) fn legacy_seal(&self, scope: &SealScope, text: &str) -> Sealed {
        let key = &self.file.keys[&self.file.active];
        let (sealed, nonce) = aead_seal(key, &Self::content_aad(scope), text.as_bytes()).unwrap();
        Sealed {
            sealed,
            nonce,
            key_id: self.file.active.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_ring_parses_strictly_and_never_echoes_keys() {
        let key = URL_SAFE_NO_PAD.encode([7u8; 32]);
        let ring = ManagedKeyring::parse(&format!(
            "# ring\nk2 {key}\nk1 {}\n",
            URL_SAFE_NO_PAD.encode([8u8; 32])
        ))
        .unwrap();
        assert_eq!(ring.file.active, "k2");
        assert!(!format!("{ring:?}").contains(&key));
        for bad in [
            "",
            "k1",
            "k1 short",
            "bad id! xx",
            &format!("k1 {key}\nk1 {key}"),
        ] {
            let err = ManagedKeyring::parse(bad).unwrap_err();
            assert!(!err.contains(&key));
        }
    }

    #[tokio::test]
    async fn legacy_ring_content_keeps_opening_and_stays_bound_to_its_message() {
        let ring = ManagedKeyring::generate("legacy-1");
        let pool = DatabaseConnection::Disconnected;
        let scope = SealScope {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            message_id: Uuid::new_v4(),
        };
        let s = ring.legacy_seal(&scope, "hello managed");
        assert!(!s.sealed.windows(5).any(|w| w == b"hello"));
        assert!(!ManagedKeyring::is_envelope_key(&s.key_id));
        let opened = ring
            .open(
                &pool,
                &scope,
                &s.sealed,
                &s.nonce,
                &s.key_id,
                DecryptPurpose::MemberRead,
            )
            .await;
        assert_eq!(opened.as_deref(), Some("hello managed"));
        let moved = SealScope {
            message_id: Uuid::new_v4(),
            ..scope
        };
        let opened = ring
            .open(
                &pool,
                &moved,
                &s.sealed,
                &s.nonce,
                &s.key_id,
                DecryptPurpose::MemberRead,
            )
            .await;
        assert_eq!(opened, None);
    }

    #[tokio::test]
    async fn the_file_provider_wraps_bound_deks_only() {
        let ring = ManagedKeyring::generate("kek-1");
        let dek = [9u8; 32];
        let w = ring.provider.wrap(&dek, b"bound-a").await.unwrap();
        assert!(w.kek_id.starts_with("file:"));
        assert!(!w.wrapped.windows(32).any(|x| x == dek));
        assert_eq!(*ring.provider.unwrap(&w, b"bound-a").await.unwrap(), dek);
        assert!(
            ring.provider.unwrap(&w, b"bound-b").await.is_none(),
            "another binding opens nothing"
        );
        let other = ManagedKeyring::generate("kek-1");
        assert!(
            other.provider.unwrap(&w, b"bound-a").await.is_none(),
            "another KEK opens nothing"
        );
    }
}
