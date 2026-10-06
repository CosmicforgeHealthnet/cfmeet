//! ArtifactStorage — provider-neutral long-term artefact byte storage (Part G §14).
//!
//! Application nodes are never long-term storage. The meeting domain stores only an
//! opaque `(bucket, key)` reference; bytes live in S3-compatible object storage (AWS
//! S3, MinIO, Cloudflare R2, …) behind this trait, so business logic never changes
//! with the vendor. Access is always: authorize first, then mint a short-lived,
//! read-only, single-object signed URL that is never stored.
//!
//! [`S3ArtifactStorage`] speaks the S3 REST API with AWS Signature V4 (header-signed
//! admin calls, query-signed client reads). [`MemoryArtifactStorage`] is an in-process
//! backend for tests and local development only.

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
// S3 (Signature V4)
// ---------------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// RFC 3986 encoding as SigV4 requires (`/` kept when `keep_slash`).
pub fn uri_encode(input: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Debug, Clone)]
struct Endpoint {
    scheme: String,
    /// host[:port] as sent in the Host header.
    host: String,
}

impl Endpoint {
    fn parse(raw: &str) -> Result<Self, String> {
        let url = url::Url::parse(raw).map_err(|e| format!("invalid storage endpoint: {e}"))?;
        let host = url
            .host_str()
            .ok_or_else(|| "storage endpoint has no host".to_string())?;
        let host = match url.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };
        Ok(Self {
            scheme: url.scheme().to_string(),
            host,
        })
    }
}

pub struct S3ArtifactStorage {
    config: ObjectStorageConfig,
    endpoint: Endpoint,
    public_endpoint: Endpoint,
    client: reqwest::Client,
}

struct Signed {
    url: String,
    headers: Vec<(String, String)>,
}

impl S3ArtifactStorage {
    pub fn new(config: ObjectStorageConfig) -> Result<Self, String> {
        let endpoint = Endpoint::parse(
            config
                .endpoint
                .as_deref()
                .ok_or_else(|| "ARTIFACT_STORAGE_ENDPOINT is not set".to_string())?,
        )?;
        let public_endpoint = match config.public_endpoint.as_deref() {
            Some(p) => Endpoint::parse(p)?,
            None => endpoint.clone(),
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            config,
            endpoint,
            public_endpoint,
            client,
        })
    }

    fn host_and_path(&self, ep: &Endpoint, key: &str) -> (String, String) {
        let key = uri_encode(key, true);
        if self.config.path_style {
            (
                ep.host.clone(),
                format!("/{}/{}", uri_encode(&self.config.bucket, false), key),
            )
        } else {
            (
                format!("{}.{}", self.config.bucket, ep.host),
                format!("/{key}"),
            )
        }
    }

    fn scope(&self, date: &str) -> String {
        format!("{date}/{}/s3/aws4_request", self.config.region)
    }

    fn signing_key(&self, date: &str) -> Vec<u8> {
        let k_date = hmac(
            format!("AWS4{}", self.config.secret_key).as_bytes(),
            date.as_bytes(),
        );
        let k_region = hmac(&k_date, self.config.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        hmac(&k_service, b"aws4_request")
    }

    /// Header-signed admin request.
    fn sign(&self, method: &str, key: &str, payload_hash: &str, now: DateTime<Utc>) -> Signed {
        let (host, path) = self.host_and_path(&self.endpoint, key);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let canonical_headers =
            format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical =
            format!("{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{}\n{}",
            self.scope(&date),
            sha256_hex(canonical.as_bytes())
        );
        let signature = hex::encode(hmac(&self.signing_key(&date), to_sign.as_bytes()));
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={signed_headers}, Signature={signature}",
            self.config.access_key,
            self.scope(&date)
        );
        Signed {
            url: format!("{}://{host}{path}", self.endpoint.scheme),
            headers: vec![
                ("x-amz-content-sha256".into(), payload_hash.into()),
                ("x-amz-date".into(), amz_date),
                ("authorization".into(), authorization),
            ],
        }
    }

    /// Query-signed (presigned) GET for one object.
    pub fn presign_get(
        &self,
        key: &str,
        ttl: Duration,
        download_name: Option<&str>,
        now: DateTime<Utc>,
    ) -> String {
        let (host, path) = self.host_and_path(&self.public_endpoint, key);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let credential = format!("{}/{}", self.config.access_key, self.scope(&date));
        let mut params: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
            ("X-Amz-Credential".into(), credential),
            ("X-Amz-Date".into(), amz_date.clone()),
            ("X-Amz-Expires".into(), ttl.as_secs().to_string()),
            ("X-Amz-SignedHeaders".into(), "host".into()),
        ];
        if let Some(name) = download_name {
            let safe: String = name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .take(100)
                .collect();
            params.push((
                "response-content-disposition".into(),
                format!("attachment; filename=\"{safe}\""),
            ));
        }
        params.sort();
        let query = params
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, false), uri_encode(v, false)))
            .collect::<Vec<_>>()
            .join("&");
        let canonical = format!("GET\n{path}\n{query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD");
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{}\n{}",
            self.scope(&date),
            sha256_hex(canonical.as_bytes())
        );
        let signature = hex::encode(hmac(&self.signing_key(&date), to_sign.as_bytes()));
        format!(
            "{}://{host}{path}?{query}&X-Amz-Signature={signature}",
            self.public_endpoint.scheme
        )
    }

    fn request(
        &self,
        method: reqwest::Method,
        key: &str,
        payload_hash: &str,
    ) -> reqwest::RequestBuilder {
        let signed = self.sign(method.as_str(), key, payload_hash, Utc::now());
        let mut rb = self.client.request(method, &signed.url);
        for (k, v) in signed.headers {
            rb = rb.header(k, v);
        }
        rb
    }

    /// Create the bucket if it does not exist (certification / local setup).
    pub async fn ensure_bucket(&self) -> StorageResult<()> {
        let (host, path) = if self.config.path_style {
            (
                self.endpoint.host.clone(),
                format!("/{}", uri_encode(&self.config.bucket, false)),
            )
        } else {
            (
                format!("{}.{}", self.config.bucket, self.endpoint.host),
                "/".to_string(),
            )
        };
        let now = Utc::now();
        let payload = sha256_hex(b"");
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let canonical = format!(
            "PUT\n{path}\n\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{amz_date}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload}"
        );
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{}\n{}",
            self.scope(&date),
            sha256_hex(canonical.as_bytes())
        );
        let signature = hex::encode(hmac(&self.signing_key(&date), to_sign.as_bytes()));
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={signature}",
            self.config.access_key,
            self.scope(&date)
        );
        let res = self
            .client
            .put(format!("{}://{host}{path}", self.endpoint.scheme))
            .header("x-amz-content-sha256", payload)
            .header("x-amz-date", amz_date)
            .header("authorization", auth)
            .send()
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;
        match res.status().as_u16() {
            200 | 409 => Ok(()),
            s => Err(StorageError::Rejected(format!("create bucket: HTTP {s}"))),
        }
    }
}

fn unavailable(e: reqwest::Error) -> StorageError {
    StorageError::Unavailable(e.to_string())
}

fn meta_from(headers: &reqwest::header::HeaderMap) -> ObjectMeta {
    let get = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    ObjectMeta {
        size: get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        content_type: get("content-type"),
        etag: get("etag").map(|e| e.trim_matches('"').to_string()),
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
        let hash = sha256_hex(&bytes);
        let size = bytes.len() as u64;
        let res = self
            .request(reqwest::Method::PUT, key, &hash)
            .header("content-type", content_type)
            .body(bytes)
            .send()
            .await
            .map_err(unavailable)?;
        if !res.status().is_success() {
            return Err(match res.status().as_u16() {
                500..=599 => StorageError::Unavailable(format!("HTTP {}", res.status())),
                s => StorageError::Rejected(format!("HTTP {s}")),
            });
        }
        let mut meta = meta_from(res.headers());
        meta.size = size;
        meta.content_type = Some(content_type.to_string());
        Ok(meta)
    }

    async fn head(&self, key: &str) -> StorageResult<Option<ObjectMeta>> {
        let res = self
            .request(reqwest::Method::HEAD, key, &sha256_hex(b""))
            .send()
            .await
            .map_err(unavailable)?;
        match res.status().as_u16() {
            200 => Ok(Some(meta_from(res.headers()))),
            404 => Ok(None),
            s @ 500..=599 => Err(StorageError::Unavailable(format!("HTTP {s}"))),
            s => Err(StorageError::Rejected(format!("HTTP {s}"))),
        }
    }

    async fn digest(&self, key: &str, max_bytes: u64) -> StorageResult<Option<ObjectDigest>> {
        let mut res = self
            .request(reqwest::Method::GET, key, &sha256_hex(b""))
            .send()
            .await
            .map_err(unavailable)?;
        match res.status().as_u16() {
            200 => {}
            404 => return Ok(None),
            s @ 500..=599 => return Err(StorageError::Unavailable(format!("HTTP {s}"))),
            s => return Err(StorageError::Rejected(format!("HTTP {s}"))),
        }
        let mut sha = Sha256::new();
        let mut md5 = Md5::new();
        let mut size = 0u64;
        while let Some(chunk) = res.chunk().await.map_err(unavailable)? {
            size += chunk.len() as u64;
            if size > max_bytes {
                return Err(StorageError::TooLarge);
            }
            sha.update(&chunk);
            md5.update(&chunk);
        }
        Ok(Some(ObjectDigest {
            sha256: hex::encode(sha.finalize()),
            md5: hex::encode(md5.finalize()),
            size,
        }))
    }

    async fn delete(&self, key: &str) -> StorageResult<()> {
        let res = self
            .request(reqwest::Method::DELETE, key, &sha256_hex(b""))
            .send()
            .await
            .map_err(unavailable)?;
        match res.status().as_u16() {
            200 | 204 | 404 => Ok(()),
            s @ 500..=599 => Err(StorageError::Unavailable(format!("HTTP {s}"))),
            s => Err(StorageError::Rejected(format!("HTTP {s}"))),
        }
    }

    fn signed_read_url(
        &self,
        key: &str,
        ttl: Duration,
        download_name: Option<&str>,
    ) -> StorageResult<SignedUrl> {
        let now = Utc::now();
        Ok(SignedUrl {
            url: self.presign_get(key, ttl, download_name, now),
            expires_at: now + chrono::Duration::seconds(ttl.as_secs() as i64),
        })
    }
}

// ---------------------------------------------------------------------------
// In-process storage (tests / local development only)
// ---------------------------------------------------------------------------

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

    #[test]
    fn sigv4_presign_matches_aws_reference_vector() {
        // AWS documentation example ("Example: A presigned URL"), path-style disabled:
        // GET https://examplebucket.s3.amazonaws.com/test.txt, 86400 s, 20130524T000000Z.
        let s = S3ArtifactStorage::new(ObjectStorageConfig {
            endpoint: Some("https://s3.amazonaws.com".into()),
            public_endpoint: None,
            region: "us-east-1".into(),
            bucket: "examplebucket".into(),
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            path_style: false,
            prefix: String::new(),
        })
        .unwrap();
        let now = DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let url = s.presign_get("test.txt", Duration::from_secs(86400), None, now);
        assert!(url.ends_with(
            "X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        ));
        assert!(url.starts_with("https://examplebucket.s3.amazonaws.com/test.txt?"));
    }

    #[test]
    fn presigned_url_is_scoped_and_carries_no_secret() {
        let s = storage();
        let url = s.presign_get(
            "recordings/a/b.mp4",
            Duration::from_secs(300),
            Some("b.mp4"),
            Utc::now(),
        );
        assert!(url.contains("/recordings/a/b.mp4?"));
        assert!(url.contains("X-Amz-Expires=300"));
        assert!(!url.contains("wJalrXUtnFEMI"));
    }

    #[test]
    fn uri_encoding_follows_rfc3986() {
        assert_eq!(uri_encode("a b/c+d", true), "a%20b/c%2Bd");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
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
