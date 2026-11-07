//! ArtifactStorage — provider-neutral long-term artefact byte storage (Part G §14).
//!
//! Application nodes are never long-term storage. The meeting domain stores only an
//! opaque `(bucket, key)` reference; bytes live in S3-compatible object storage (AWS
//! S3, MinIO, Cloudflare R2, …) behind this trait, so business logic never changes
//! with the vendor. Access is always: authorize first, then mint a short-lived,
//! read-only, single-object signed URL that is never stored.
//!
//! [`S3ArtifactStorage`] speaks the S3 REST API through the shared
//! `cf-object-storage` crate (AWS Signature V4: header-signed admin calls, query-signed
//! reads) — one signer for the whole platform (Blueprint v1.0 Section 16, SEC-019).
//! Client reads are capped at the shared 900 s; only the server-to-provider transcription
//! source may last up to [`MAX_PROVIDER_READ`]. [`MemoryArtifactStorage`] is an
//! in-process backend for tests and local development only.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use md5::Md5;
use sha2::{Digest, Sha256};

use crate::config::operations::ObjectStorageConfig;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
    #[error("object storage unavailable: {0}")]
    Unavailable(String),
    #[error("object storage rejected the request: {0}")]
    Rejected(String),
    #[error("object exceeds the integrity-check size limit")]
    TooLarge,
    #[error("object storage is not configured")]
    NotConfigured,
}

pub type StorageResult<T> = Result<T, StorageError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    pub size: u64,
    pub content_type: Option<String>,
    /// Quoted-stripped ETag (MD5 hex for single-part uploads).
    pub etag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectDigest {
    pub sha256: String,
    pub md5: String,
    pub size: u64,
}

impl ObjectMeta {
    /// Single-part ETags are the object's MD5; multipart ETags (`…-N`) are not.
    pub fn md5_etag(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|e| e.len() == 32 && e.chars().all(|c| c.is_ascii_hexdigit()))
    }
}

/// A signed, read-only, single-object URL (never persisted).
#[derive(Debug, Clone)]
pub struct SignedUrl {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

#[async_trait]
pub trait ArtifactStorage: Send + Sync {
    fn name(&self) -> &'static str;
    fn bucket(&self) -> &str;
    fn is_configured(&self) -> bool {
        true
    }
    /// Full object key for a relative artefact path (applies the configured prefix).
    fn object_key(&self, relative: &str) -> String;
    async fn put(&self, key: &str, bytes: Vec<u8>, content_type: &str)
        -> StorageResult<ObjectMeta>;
    async fn head(&self, key: &str) -> StorageResult<Option<ObjectMeta>>;
    async fn exists(&self, key: &str) -> StorageResult<bool> {
        Ok(self.head(key).await?.is_some())
    }
    /// Stream the object and compute SHA-256 + MD5 (bounded by `max_bytes`).
    async fn digest(&self, key: &str, max_bytes: u64) -> StorageResult<Option<ObjectDigest>>;
    /// Delete (an absent object is success).
    async fn delete(&self, key: &str) -> StorageResult<()>;
    /// Read-only signed URL scoped to exactly one object.
    fn signed_read_url(
        &self,
        key: &str,
        ttl: Duration,
        download_name: Option<&str>,
    ) -> StorageResult<SignedUrl>;
    /// Read-only URL for a processing provider to fetch one object (server to provider,
    /// never a client); may last up to [`MAX_PROVIDER_READ`].
    fn provider_read_url(&self, key: &str, ttl: Duration) -> StorageResult<SignedUrl> {
        self.signed_read_url(key, ttl, None)
    }
}

/// Storage used when none is configured: every operation is unavailable.
pub struct UnconfiguredStorage;

#[async_trait]
impl ArtifactStorage for UnconfiguredStorage {
    fn name(&self) -> &'static str {
        "unconfigured"
    }
    fn bucket(&self) -> &str {
        ""
    }
    fn is_configured(&self) -> bool {
        false
    }
    fn object_key(&self, relative: &str) -> String {
        relative.to_string()
    }
    async fn put(&self, _: &str, _: Vec<u8>, _: &str) -> StorageResult<ObjectMeta> {
        Err(StorageError::NotConfigured)
    }
    async fn head(&self, _: &str) -> StorageResult<Option<ObjectMeta>> {
        Err(StorageError::NotConfigured)
    }
    async fn digest(&self, _: &str, _: u64) -> StorageResult<Option<ObjectDigest>> {
        Err(StorageError::NotConfigured)
    }
    async fn delete(&self, _: &str) -> StorageResult<()> {
        Err(StorageError::NotConfigured)
    }
    fn signed_read_url(&self, _: &str, _: Duration, _: Option<&str>) -> StorageResult<SignedUrl> {
        Err(StorageError::NotConfigured)
    }
}

// ---------------------------------------------------------------------------
// S3 — the shared `cf-object-storage` signer (SEC-019)
// ---------------------------------------------------------------------------

/// Longest provider fetch URL (server-to-provider source for post-meeting
/// transcription). Client reads are capped by the shared library at
/// [`cf_object_storage::MAX_SIGNED_READ`] (900 s).
pub const MAX_PROVIDER_READ: Duration = Duration::from_secs(3600);

fn shared_error(e: cf_object_storage::StorageError) -> StorageError {
    use cf_object_storage::StorageError as S;
    match e {
        S::Unavailable => StorageError::Unavailable("object storage unavailable".into()),
        S::Rejected(code) => StorageError::Rejected(format!("HTTP {code}")),
        S::Integrity => StorageError::Rejected("object integrity check failed".into()),
        S::NotConfigured => StorageError::NotConfigured,
    }
}

/// `Content-Disposition` for a download name (restricted to `[A-Za-z0-9._-]`, ≤ 100).
pub fn attachment_disposition(name: &str) -> String {
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(100)
        .collect();
    format!("attachment; filename=\"{safe}\"")
}

/// Meeting artefacts on S3-compatible storage. Blueprint v1.0 Section 16 (SEC-019): every
/// signature (header-signed writes / heads / deletes / reads and query-signed URLs) comes
/// from the shared [`cf_object_storage::S3ObjectStore`], the same implementation Calling,
/// Messaging and Numbering use; this type only adds the artefact key prefix, MD5 + SHA-256
/// streaming digests and the meeting error vocabulary.
pub struct S3ArtifactStorage {
    config: ObjectStorageConfig,
    store: cf_object_storage::S3ObjectStore,
}

impl S3ArtifactStorage {
    pub fn new(config: ObjectStorageConfig) -> Result<Self, String> {
        let endpoint = config
            .endpoint
            .clone()
            .ok_or_else(|| "ARTIFACT_STORAGE_ENDPOINT is not set".to_string())?;
        let store = cf_object_storage::S3ObjectStore::new(cf_object_storage::S3Config {
            endpoint,
            public_endpoint: config.public_endpoint.clone(),
            region: config.region.clone(),
            bucket: config.bucket.clone(),
            access_key: config.access_key.clone(),
            secret_key: config.secret_key.clone(),
            path_style: config.path_style,
            server_side_encryption: None,
        })?;
        Ok(Self { config, store })
    }

    /// Create the bucket if it does not exist (certification / local setup).
    pub async fn ensure_bucket(&self) -> StorageResult<()> {
        self.store.ensure_bucket().await.map_err(shared_error)
    }

    /// Signed read at a given instant (pure; the shared signer caps `ttl` at 900 s).
    pub fn signed_read_url_at(
        &self,
        key: &str,
        ttl: Duration,
        download_name: Option<&str>,
        now: DateTime<Utc>,
    ) -> StorageResult<SignedUrl> {
        use cf_object_storage::ObjectStore;
        let signed = match download_name {
            Some(name) => self.store.signed_read_as(
                key,
                ttl,
                now,
                &cf_object_storage::ResponseOverrides {
                    content_type: None,
                    content_disposition: Some(attachment_disposition(name)),
                },
            ),
            None => self.store.signed_read(key, ttl, now),
        }
        .map_err(shared_error)?;
        Ok(SignedUrl {
            url: signed.url,
            expires_at: signed.expires_at,
        })
    }
}

fn meta_of(m: cf_object_storage::ObjectMeta) -> ObjectMeta {
    ObjectMeta {
        size: m.size,
        content_type: m.content_type,
        etag: m.etag,
    }
}

#[async_trait]
impl ArtifactStorage for S3ArtifactStorage {
    fn name(&self) -> &'static str {
        "s3"
    }

    fn bucket(&self) -> &str {
        &self.config.bucket
    }

    fn object_key(&self, relative: &str) -> String {
        let prefix = self.config.prefix.trim_matches('/');
        if prefix.is_empty() {
            relative.to_string()
        } else {
            format!("{prefix}/{relative}")
        }
    }

    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> StorageResult<ObjectMeta> {
        use cf_object_storage::ObjectStore;
        let stored = self
            .store
            .put(key, bytes, content_type)
            .await
            .map_err(shared_error)?;
        Ok(ObjectMeta {
            size: stored.size,
            content_type: Some(stored.content_type),
            etag: stored.etag,
        })
    }

    async fn head(&self, key: &str) -> StorageResult<Option<ObjectMeta>> {
        use cf_object_storage::ObjectStore;
        Ok(self
            .store
            .head(key)
            .await
            .map_err(shared_error)?
            .map(meta_of))
    }

    async fn digest(&self, key: &str, max_bytes: u64) -> StorageResult<Option<ObjectDigest>> {
        let mut sha = Sha256::new();
        let mut md5 = Md5::new();
        let visited = self
            .store
            .get_streaming(key, max_bytes, |chunk| {
                sha.update(chunk);
                md5.update(chunk);
            })
            .await
            .map_err(|e| match e {
                cf_object_storage::StorageError::Integrity => StorageError::TooLarge,
                other => shared_error(other),
            })?;
        Ok(visited.map(|size| ObjectDigest {
            sha256: hex::encode(sha.finalize()),
            md5: hex::encode(md5.finalize()),
            size,
        }))
    }

    async fn delete(&self, key: &str) -> StorageResult<()> {
        use cf_object_storage::ObjectStore;
        self.store.delete(key).await.map_err(shared_error)
    }

    fn signed_read_url(
        &self,
        key: &str,
        ttl: Duration,
        download_name: Option<&str>,
    ) -> StorageResult<SignedUrl> {
        self.signed_read_url_at(key, ttl, download_name, Utc::now())
    }

    fn provider_read_url(&self, key: &str, ttl: Duration) -> StorageResult<SignedUrl> {
        if !cf_object_storage::valid_key(key) {
            return Err(StorageError::Rejected("HTTP 400".into()));
        }
        let ttl = ttl.min(MAX_PROVIDER_READ).max(Duration::from_secs(1));
        let now = Utc::now();
        Ok(SignedUrl {
            url: self.store.presign_get(key, ttl, now),
            expires_at: now + chrono::Duration::seconds(ttl.as_secs() as i64),
        })
    }
}

// ---------------------------------------------------------------------------
// In-process storage (tests / local development only)
// ---------------------------------------------------------------------------

/// HMAC-SHA256 for the memory store's fake signatures (not an S3 signer).
fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

#[derive(Default)]
pub struct MemoryArtifactStorage {
    objects: Mutex<HashMap<String, (Vec<u8>, String)>>,
    /// Failure injection.
    pub unavailable: AtomicBool,
    pub fail_delete: AtomicBool,
    pub fail_sign: AtomicBool,
    /// Ignore deletes (object "comes back" — absence never confirmed).
    pub sticky: AtomicBool,
}

impl MemoryArtifactStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Simulate a provider (egress) writing the object directly.
    pub fn put_raw(&self, key: &str, bytes: Vec<u8>, content_type: &str) {
        self.objects
            .lock()
            .expect("storage lock")
            .insert(key.to_string(), (bytes, content_type.to_string()));
    }

    pub fn remove_raw(&self, key: &str) {
        self.objects.lock().expect("storage lock").remove(key);
    }

    pub fn contains(&self, key: &str) -> bool {
        self.objects.lock().expect("storage lock").contains_key(key)
    }

    pub fn get_raw(&self, key: &str) -> Option<Vec<u8>> {
        self.objects
            .lock()
            .expect("storage lock")
            .get(key)
            .map(|(b, _)| b.clone())
    }

    pub fn keys(&self) -> Vec<String> {
        self.objects
            .lock()
            .expect("storage lock")
            .keys()
            .cloned()
            .collect()
    }

    fn check(&self) -> StorageResult<()> {
        if self.unavailable.load(Ordering::SeqCst) {
            Err(StorageError::Unavailable("injected outage".into()))
        } else {
            Ok(())
        }
    }

    /// Verify a memory signed URL (key + expiry are part of the signature).
    pub fn verify_signed(&self, url: &str, now: DateTime<Utc>) -> Option<String> {
        let rest = url.strip_prefix("memory://local/")?;
        let (key, query) = rest.split_once('?')?;
        let params: HashMap<&str, &str> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .collect();
        let expires: i64 = params.get("expires")?.parse().ok()?;
        let sig = params.get("sig")?;
        if hex::encode(hmac(
            b"memory-storage",
            format!("{key}|{expires}").as_bytes(),
        )) != *sig
            || now.timestamp() > expires
        {
            return None;
        }
        Some(key.to_string())
    }
}

#[async_trait]
impl ArtifactStorage for MemoryArtifactStorage {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn bucket(&self) -> &str {
        "local"
    }
    fn object_key(&self, relative: &str) -> String {
        format!("artifacts/{relative}")
    }
    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> StorageResult<ObjectMeta> {
        self.check()?;
        let size = bytes.len() as u64;
        let etag = hex::encode(Md5::digest(&bytes));
        self.put_raw(key, bytes, content_type);
        Ok(ObjectMeta {
            size,
            content_type: Some(content_type.into()),
            etag: Some(etag),
        })
    }
    async fn head(&self, key: &str) -> StorageResult<Option<ObjectMeta>> {
        self.check()?;
        Ok(self
            .objects
            .lock()
            .expect("storage lock")
            .get(key)
            .map(|(b, ct)| ObjectMeta {
                size: b.len() as u64,
                content_type: Some(ct.clone()),
                etag: Some(hex::encode(Md5::digest(b))),
            }))
    }
    async fn digest(&self, key: &str, max_bytes: u64) -> StorageResult<Option<ObjectDigest>> {
        self.check()?;
        let Some(bytes) = self.get_raw(key) else {
            return Ok(None);
        };
        if bytes.len() as u64 > max_bytes {
            return Err(StorageError::TooLarge);
        }
        Ok(Some(ObjectDigest {
            sha256: hex::encode(Sha256::digest(&bytes)),
            md5: hex::encode(Md5::digest(&bytes)),
            size: bytes.len() as u64,
        }))
    }
    async fn delete(&self, key: &str) -> StorageResult<()> {
        self.check()?;
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(StorageError::Unavailable("injected delete failure".into()));
        }
        if !self.sticky.load(Ordering::SeqCst) {
            self.remove_raw(key);
        }
        Ok(())
    }
    fn signed_read_url(
        &self,
        key: &str,
        ttl: Duration,
        _download_name: Option<&str>,
    ) -> StorageResult<SignedUrl> {
        if self.fail_sign.load(Ordering::SeqCst) {
            return Err(StorageError::Unavailable("injected signing failure".into()));
        }
        let expires_at = Utc::now() + chrono::Duration::seconds(ttl.as_secs() as i64);
        let expires = expires_at.timestamp();
        let sig = hex::encode(hmac(
            b"memory-storage",
            format!("{key}|{expires}").as_bytes(),
        ));
        Ok(SignedUrl {
            url: format!("memory://local/{key}?expires={expires}&sig={sig}"),
            expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage() -> S3ArtifactStorage {
        S3ArtifactStorage::new(ObjectStorageConfig {
            endpoint: Some("https://examplebucket.s3.amazonaws.com".into()),
            public_endpoint: None,
            region: "us-east-1".into(),
            bucket: "examplebucket".into(),
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            path_style: false,
            prefix: String::new(),
        })
        .unwrap()
    }

    fn shared() -> cf_object_storage::S3ObjectStore {
        cf_object_storage::S3ObjectStore::new(cf_object_storage::S3Config {
            endpoint: "https://examplebucket.s3.amazonaws.com".into(),
            public_endpoint: None,
            region: "us-east-1".into(),
            bucket: "examplebucket".into(),
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            path_style: false,
            server_side_encryption: None,
        })
        .unwrap()
    }

    /// SEC-019: the artefact adapter has no signer of its own; its URLs are exactly the
    /// shared library's (the AWS reference vector is tested there).
    #[test]
    fn signed_reads_come_from_the_shared_signer() {
        use cf_object_storage::ObjectStore;
        let now = DateTime::parse_from_rfc3339("2026-11-07T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ttl = Duration::from_secs(300);
        let ours = storage()
            .signed_read_url_at("recordings/a/b.mp4", ttl, Some("b.mp4"), now)
            .unwrap();
        let theirs = shared()
            .signed_read_as(
                "recordings/a/b.mp4",
                ttl,
                now,
                &cf_object_storage::ResponseOverrides {
                    content_type: None,
                    content_disposition: Some("attachment; filename=\"b.mp4\"".into()),
                },
            )
            .unwrap();
        assert_eq!(ours.url, theirs.url);
        assert_eq!(ours.expires_at, theirs.expires_at);
        let plain = storage()
            .signed_read_url_at("recordings/a/b.mp4", ttl, None, now)
            .unwrap();
        assert_eq!(
            plain.url,
            shared()
                .signed_read("recordings/a/b.mp4", ttl, now)
                .unwrap()
                .url
        );
        assert!(ours.url.contains("/recordings/a/b.mp4?"));
        assert!(ours.url.contains("X-Amz-Expires=300"));
        assert!(ours.url.contains("response-content-disposition="));
        assert!(!ours.url.contains("wJalrXUtnFEMI"));
    }

    #[test]
    fn client_reads_are_capped_and_provider_reads_bounded() {
        let now = Utc::now();
        let s = storage();
        let read = s
            .signed_read_url_at("recordings/a/b.mp4", Duration::from_secs(3600), None, now)
            .unwrap();
        assert!(read.url.contains("X-Amz-Expires=900"), "{}", read.url);
        assert_eq!(read.expires_at, now + chrono::Duration::seconds(900));
        let provider = s
            .provider_read_url("recordings/a/b.mp4", Duration::from_secs(3600))
            .unwrap();
        assert!(provider.url.contains("X-Amz-Expires=3600"));
        let capped = s
            .provider_read_url("recordings/a/b.mp4", Duration::from_secs(7200))
            .unwrap();
        assert!(capped.url.contains("X-Amz-Expires=3600"));
    }

    #[test]
    fn unsafe_keys_and_names_are_refused_or_sanitized() {
        let s = storage();
        assert!(s
            .signed_read_url_at("../etc/passwd", Duration::from_secs(60), None, Utc::now())
            .is_err());
        assert!(s
            .provider_read_url("/abs/key", Duration::from_secs(60))
            .is_err());
        assert_eq!(
            attachment_disposition("a\"b; x=y.mp4"),
            "attachment; filename=\"abxy.mp4\""
        );
    }

    #[tokio::test]
    async fn memory_storage_signs_and_verifies_single_objects() {
        let m = MemoryArtifactStorage::new();
        m.put_raw("k1", b"abc".to_vec(), "text/plain");
        let signed = m
            .signed_read_url("k1", Duration::from_secs(60), None)
            .unwrap();
        assert_eq!(
            m.verify_signed(&signed.url, Utc::now()).as_deref(),
            Some("k1")
        );
        let tampered = signed.url.replace("k1", "k2");
        assert!(m.verify_signed(&tampered, Utc::now()).is_none());
        assert!(m
            .verify_signed(&signed.url, Utc::now() + chrono::Duration::seconds(120))
            .is_none());
        let d = m.digest("k1", 10).await.unwrap().unwrap();
        assert_eq!(d.size, 3);
        assert!(matches!(
            m.digest("k1", 2).await,
            Err(StorageError::TooLarge)
        ));
        m.delete("k1").await.unwrap();
        assert!(m.head("k1").await.unwrap().is_none());
    }
}
