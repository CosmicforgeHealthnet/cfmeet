//! Provider-neutral PROTECTED object storage (Part F §106–§108, §160; ADR 0023).
//!
//! Application nodes are never long-term storage: artifacts (voicemail messages, call
//! recordings) live in a private S3-compatible bucket (AWS S3, MinIO, …) behind
//! [`ObjectStore`]. The owning service stores only an opaque key and integrity metadata
//! (size, SHA-256, content type); it never stores or returns a public URL. Reads are
//! always: the service authorizes the requester FIRST, then mints a short-lived,
//! read-only, single-object signed URL that is never persisted.
//!
//! * [`S3ObjectStore`] speaks the S3 REST API with AWS Signature V4 (header-signed
//!   writes / heads / deletes, query-signed reads) and can request server-side
//!   encryption for every object.
//! * [`MemoryObjectStore`] is an in-process backend for tests and local development,
//!   with an availability switch for failure injection.
//!
//! This crate carries no business vocabulary and no service's types.

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
use sha2::{Digest, Sha256};

/// Why a storage operation failed (never carries an object key or a credential).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
    /// Transient: the store could not be reached or answered 5xx — retry later.
    #[error("object storage unavailable")]
    Unavailable,
    /// Definitive refusal (4xx other than not-found).
    #[error("object storage rejected the request (HTTP {0})")]
    Rejected(u16),
    /// The object does not satisfy the requested integrity (size / digest).
    #[error("object integrity check failed")]
    Integrity,
    /// No store is configured in this deployment.
    #[error("object storage is not configured")]
    NotConfigured,
}

pub type StorageResult<T> = Result<T, StorageError>;

/// What a successful write stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub size: u64,
    /// Lower-case hex SHA-256 of the bytes.
    pub sha256: String,
    pub content_type: String,
    /// Whether the store confirmed server-side encryption of the object.
    pub encrypted: bool,
}

/// Metadata of an existing object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    pub size: u64,
    pub content_type: Option<String>,
    pub encrypted: bool,
}

/// A short-lived read URL (never stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedRead {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

/// Longest signed read the stores issue.
pub const MAX_SIGNED_READ: Duration = Duration::from_secs(900);
/// Longest signed write (upload) the stores issue.
pub const MAX_SIGNED_WRITE: Duration = Duration::from_secs(3600);

/// A short-lived, single-object upload: the client sends `method` to `url` with exactly
/// `headers` (they are signed: the object's size, SHA-256 and media type cannot be
/// changed) and the body. Never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedWrite {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub expires_at: DateTime<Utc>,
}

/// Response headers a signed read forces (safe browser handling of downloads).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseOverrides {
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
}

/// One listed object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedObject {
    pub key: String,
    pub size: u64,
    pub last_modified: Option<DateTime<Utc>>,
}

/// Lower-case hex of a base64 (standard) SHA-256, as S3 checksum headers carry it.
pub fn checksum_b64_to_hex(b64: &str) -> Option<String> {
    let bytes = base64_decode(b64.trim())?;
    (bytes.len() == 32).then(|| hex::encode(bytes))
}

/// Standard base64 of the bytes of a lower-case hex SHA-256 (`x-amz-checksum-sha256`).
pub fn checksum_hex_to_b64(hex_digest: &str) -> Option<String> {
    let bytes = hex::decode(hex_digest).ok().filter(|b| b.len() == 32)?;
    Some(base64_encode(&bytes))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = B64.iter().position(|b| *b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Lower-case hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// The storage port.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// Backend name for logs / metrics (bounded).
    fn backend(&self) -> &'static str;

    /// Whether a store is configured at all (an unconfigured deployment fails closed).
    fn configured(&self) -> bool {
        true
    }

    /// Writes `bytes` under `key` (overwriting an earlier attempt of the same key).
    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> StorageResult<StoredObject>;

    async fn head(&self, key: &str) -> StorageResult<Option<ObjectMeta>>;

    /// Reads ONE object of at most `max_bytes` (a larger object is
    /// [`StorageError::Integrity`], never truncated). `None`: no such object. Used by
    /// server-side finalizers only; clients read through [`ObjectStore::signed_read`].
    async fn get(&self, key: &str, max_bytes: usize) -> StorageResult<Option<Vec<u8>>>;

    /// Removes `key`. Idempotent: a missing object is success.
    async fn delete(&self, key: &str) -> StorageResult<()>;

    /// A read-only URL for ONE object valid for `ttl` (capped at [`MAX_SIGNED_READ`]).
    /// Callers authorize the requester before asking.
    fn signed_read(
        &self,
        key: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> StorageResult<SignedRead>;

    /// A read-only URL that also forces the response's media type / disposition (safe
    /// downloads). Stores that cannot force headers refuse (never a silent plain read).
    fn signed_read_as(
        &self,
        _key: &str,
        _ttl: Duration,
        _now: DateTime<Utc>,
        _overrides: &ResponseOverrides,
    ) -> StorageResult<SignedRead> {
        Err(StorageError::NotConfigured)
    }

    /// A short-lived upload of exactly `size` bytes with SHA-256 `sha256_hex` and media
    /// type `content_type` to `key` (capped at [`MAX_SIGNED_WRITE`]). The store rejects
    /// a body of another size or digest. Callers authorize the uploader first.
    fn signed_write(
        &self,
        _key: &str,
        _ttl: Duration,
        _now: DateTime<Utc>,
        _size: u64,
        _sha256_hex: &str,
        _content_type: &str,
    ) -> StorageResult<SignedWrite> {
        Err(StorageError::NotConfigured)
    }

    /// The SHA-256 (lower-case hex) the store recorded and verified for `key`, when it
    /// reports one (`Ok(None)`: not reported — the caller verifies another way).
    async fn stored_sha256(&self, _key: &str) -> StorageResult<Option<String>> {
        Ok(None)
    }

    /// Objects under `prefix` after `start_after` (lexicographic), at most `max`.
    async fn list(
        &self,
        _prefix: &str,
        _start_after: Option<&str>,
        _max: usize,
    ) -> StorageResult<Vec<ListedObject>> {
        Err(StorageError::NotConfigured)
    }
}

/// No store configured: every operation is [`StorageError::NotConfigured`].
pub struct UnconfiguredStore;

#[async_trait]
impl ObjectStore for UnconfiguredStore {
    fn backend(&self) -> &'static str {
        "unconfigured"
    }

    fn configured(&self) -> bool {
        false
    }

    async fn put(&self, _: &str, _: Vec<u8>, _: &str) -> StorageResult<StoredObject> {
        Err(StorageError::NotConfigured)
    }

    async fn head(&self, _: &str) -> StorageResult<Option<ObjectMeta>> {
        Err(StorageError::NotConfigured)
    }

    async fn get(&self, _: &str, _: usize) -> StorageResult<Option<Vec<u8>>> {
        Err(StorageError::NotConfigured)
    }

    async fn delete(&self, _: &str) -> StorageResult<()> {
        Err(StorageError::NotConfigured)
    }

    fn signed_read(&self, _: &str, _: Duration, _: DateTime<Utc>) -> StorageResult<SignedRead> {
        Err(StorageError::NotConfigured)
    }
}

// ---------------------------------------------------------------------------
// S3 (AWS Signature V4)
// ---------------------------------------------------------------------------

/// Configuration of an S3-compatible private bucket.
#[derive(Clone)]
pub struct S3Config {
    /// API endpoint, e.g. `https://s3.eu-west-1.amazonaws.com` or `http://127.0.0.1:9000`.
    pub endpoint: String,
    /// Endpoint embedded in signed read URLs (defaults to `endpoint`).
    pub public_endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    /// Path-style addressing (`/bucket/key`), as MinIO and most test stores need.
    pub path_style: bool,
    /// Server-side encryption requested for every object (`AES256` = SSE-S3), if any.
    pub server_side_encryption: Option<String>,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("path_style", &self.path_style)
            .field("server_side_encryption", &self.server_side_encryption)
            .finish_non_exhaustive()
    }
}

impl S3Config {
    /// Reads `{prefix}ENDPOINT`, `{prefix}PUBLIC_ENDPOINT`, `{prefix}REGION`, `{prefix}BUCKET`,
    /// `{prefix}ACCESS_KEY_ID`, `{prefix}SECRET_ACCESS_KEY`, `{prefix}PATH_STYLE` (true/false,
    /// default true) and `{prefix}SERVER_SIDE_ENCRYPTION` (e.g. `AES256`). No endpoint = no
    /// store (`Ok(None)`); an endpoint with missing or invalid settings is an error.
    ///
    /// The secret access key may instead be a mounted secret file,
    /// `{prefix}SECRET_ACCESS_KEY_FILE` (trimmed contents; preferred). Both set is refused
    /// as ambiguous, an unreadable / empty file is an error (never a fallback), and outside
    /// the `local` / `test` runtime environments (`CF_RUNTIME_ENVIRONMENT`, unset = strict)
    /// a file readable by group or others is refused. Placeholder secrets are refused.
    pub fn from_lookup(
        prefix: &str,
        get: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, String> {
        let var = |name: &str| {
            get(&format!("{prefix}{name}"))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let Some(endpoint) = var("ENDPOINT") else {
            return Ok(None);
        };
        let required = |name: &str| var(name).ok_or_else(|| format!("{prefix}{name} is required"));
        let path_style = match var("PATH_STYLE").as_deref() {
            None | Some("true") | Some("1") => true,
            Some("false") | Some("0") => false,
            Some(_) => return Err(format!("{prefix}PATH_STYLE must be true or false")),
        };
        let config = Self {
            endpoint,
            public_endpoint: var("PUBLIC_ENDPOINT"),
            region: var("REGION").unwrap_or_else(|| "us-east-1".into()),
            bucket: required("BUCKET")?,
            access_key: required("ACCESS_KEY_ID")?,
            secret_key: secret_access_key(prefix, get)?,
            path_style,
            server_side_encryption: var("SERVER_SIDE_ENCRYPTION"),
        };
        Endpoint::parse(&config.endpoint)?;
        if let Some(public) = config.public_endpoint.as_deref() {
            Endpoint::parse(public)?;
        }
        Ok(Some(config))
    }
}

/// `{prefix}SECRET_ACCESS_KEY` or `{prefix}SECRET_ACCESS_KEY_FILE` (see
/// [`S3Config::from_lookup`]). Errors name the variable, never the value or the path.
fn secret_access_key(
    prefix: &str,
    get: &impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let name = format!("{prefix}SECRET_ACCESS_KEY");
    let file_var = format!("{name}_FILE");
    let inline = get(&name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let path = get(&file_var)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let value = match (inline, path) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "both {name} and {file_var} are set; configure exactly one"
            ))
        }
        (None, None) => return Err(format!("{name} is required")),
        (Some(value), None) => value,
        (None, Some(path)) => {
            let permissive = matches!(
                get("CF_RUNTIME_ENVIRONMENT").as_deref().map(str::trim),
                Some("local") | Some("test")
            );
            read_secret_file(&file_var, &path, permissive)?
        }
    };
    let lowered = value.to_ascii_lowercase();
    if value.len() < 8
        || ["changeme", "change-me", "placeholder", "your-", "secret123"]
            .iter()
            .any(|p| lowered.contains(p))
    {
        return Err(format!(
            "{name} must be a generated secret, not a placeholder"
        ));
    }
    Ok(value)
}

fn read_secret_file(var: &str, path: &str, permissive: bool) -> Result<String, String> {
    let unreadable = || format!("{var}: the secret file cannot be read");
    let meta = std::fs::metadata(path).map_err(|_| unreadable())?;
    if !meta.is_file() || meta.len() > 16 * 1024 {
        return Err(format!(
            "{var}: not a regular secret file of at most 16 KiB"
        ));
    }
    #[cfg(unix)]
    if !permissive {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "{var}: the secret file must not be readable by group or others"
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = permissive;
    let text = std::fs::read_to_string(path).map_err(|_| unreadable())?;
    let value = text.trim();
    if value.is_empty() {
        return Err(format!("{var}: the secret file is empty"));
    }
    Ok(value.to_string())
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
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
    /// `host[:port]` as sent in the Host header.
    host: String,
}

impl Endpoint {
    fn parse(raw: &str) -> Result<Self, String> {
        let url = url::Url::parse(raw).map_err(|_| "invalid storage endpoint".to_string())?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("storage endpoint must be http(s)".into());
        }
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

/// A private S3-compatible bucket.
pub struct S3ObjectStore {
    config: S3Config,
    endpoint: Endpoint,
    public_endpoint: Endpoint,
    client: reqwest::Client,
}

impl std::fmt::Debug for S3ObjectStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3ObjectStore")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

struct Signed {
    url: String,
    headers: Vec<(String, String)>,
}

/// A key the stores accept: 1-512 bytes of `[A-Za-z0-9/_.-]`, no `..`, no leading `/`.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 512
        && !key.starts_with('/')
        && !key.contains("..")
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'.' | b'-'))
}

impl S3ObjectStore {
    pub fn new(config: S3Config) -> Result<Self, String> {
        if config.bucket.is_empty() || config.access_key.is_empty() || config.secret_key.is_empty()
        {
            return Err("bucket and credentials are required".into());
        }
        let endpoint = Endpoint::parse(&config.endpoint)?;
        let public_endpoint = match config.public_endpoint.as_deref() {
            Some(p) => Endpoint::parse(p)?,
            None => endpoint.clone(),
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot build the storage client".to_string())?;
        Ok(Self {
            config,
            endpoint,
            public_endpoint,
            client,
        })
    }

    fn host_and_path(&self, ep: &Endpoint, key: Option<&str>) -> (String, String) {
        let key = key.map(|k| uri_encode(k, true)).unwrap_or_default();
        if self.config.path_style {
            let bucket = uri_encode(&self.config.bucket, false);
            let path = if key.is_empty() {
                format!("/{bucket}")
            } else {
                format!("/{bucket}/{key}")
            };
            (ep.host.clone(), path)
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

    /// A header-signed request. `extra` headers (lower-case names) are signed too.
    fn sign(
        &self,
        method: &str,
        key: Option<&str>,
        payload_hash: &str,
        extra: &[(&str, &str)],
        now: DateTime<Utc>,
    ) -> Signed {
        let (host, path) = self.host_and_path(&self.endpoint, key);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let mut headers: Vec<(String, String)> = vec![
            ("host".into(), host.clone()),
            ("x-amz-content-sha256".into(), payload_hash.into()),
            ("x-amz-date".into(), amz_date.clone()),
        ];
        for (k, v) in extra {
            headers.push((k.to_string(), v.trim().to_string()));
        }
        headers.sort();
        let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let signed_headers = headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");
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
        let mut out: Vec<(String, String)> =
            headers.into_iter().filter(|(k, _)| k != "host").collect();
        out.push(("authorization".into(), authorization));
        Signed {
            url: format!("{}://{host}{path}", self.endpoint.scheme),
            headers: out,
        }
    }

    fn request(
        &self,
        method: reqwest::Method,
        key: Option<&str>,
        payload_hash: &str,
        extra: &[(&str, &str)],
    ) -> reqwest::RequestBuilder {
        let signed = self.sign(method.as_str(), key, payload_hash, extra, Utc::now());
        let mut rb = self.client.request(method, &signed.url);
        for (k, v) in signed.headers {
            rb = rb.header(k, v);
        }
        rb
    }

    /// The query-signed read URL (pure: `now` given).
    pub fn presign_get(&self, key: &str, ttl: Duration, now: DateTime<Utc>) -> String {
        self.presign("GET", key, ttl, now, &[], &[])
    }

    /// A query-signed URL for `method` on `key`. `headers` (lower-case names) are signed
    /// and must be sent unchanged; `params` are extra signed query parameters.
    fn presign(
        &self,
        method: &str,
        key: &str,
        ttl: Duration,
        now: DateTime<Utc>,
        headers: &[(&str, String)],
        params: &[(&str, String)],
    ) -> String {
        let (host, path) = self.host_and_path(&self.public_endpoint, Some(key));
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let credential = format!("{}/{}", self.config.access_key, self.scope(&date));
        let mut signed: Vec<(String, String)> = vec![("host".into(), host.clone())];
        for (k, v) in headers {
            signed.push((k.to_string(), v.trim().to_string()));
        }
        signed.sort();
        let signed_names = signed
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let mut query_params: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
            ("X-Amz-Credential".into(), credential),
            ("X-Amz-Date".into(), amz_date.clone()),
            ("X-Amz-Expires".into(), ttl.as_secs().to_string()),
            ("X-Amz-SignedHeaders".into(), signed_names.clone()),
        ];
        for (k, v) in params {
            query_params.push((k.to_string(), v.clone()));
        }
        let mut encoded: Vec<(String, String)> = query_params
            .iter()
            .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
            .collect();
        encoded.sort();
        let query = encoded
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let canonical_headers: String = signed.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let canonical = format!(
            "{method}\n{path}\n{query}\n{canonical_headers}\n{signed_names}\nUNSIGNED-PAYLOAD"
        );
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

    /// A header-signed request with a canonical query string (pure: `now` given).
    fn sign_query(&self, method: &str, query: &[(&str, String)], now: DateTime<Utc>) -> Signed {
        let (host, path) = self.host_and_path(&self.endpoint, None);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let payload = sha256_hex(b"");
        let mut encoded: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
            .collect();
        encoded.sort();
        let qs = encoded
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let canonical = format!(
            "{method}\n{path}\n{qs}\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{amz_date}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload}"
        );
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{}\n{}",
            self.scope(&date),
            sha256_hex(canonical.as_bytes())
        );
        let signature = hex::encode(hmac(&self.signing_key(&date), to_sign.as_bytes()));
        Signed {
            url: format!("{}://{host}{path}?{qs}", self.endpoint.scheme),
            headers: vec![
                ("x-amz-content-sha256".into(), payload),
                ("x-amz-date".into(), amz_date),
                (
                    "authorization".into(),
                    format!(
                        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={signature}",
                        self.config.access_key,
                        self.scope(&date)
                    ),
                ),
            ],
        }
    }

    /// Creates the bucket if missing (local / certification setup only).
    pub async fn ensure_bucket(&self) -> StorageResult<()> {
        let empty = sha256_hex(b"");
        let res = self
            .request(reqwest::Method::PUT, None, &empty, &[])
            .send()
            .await
            .map_err(|_| StorageError::Unavailable)?;
        match res.status().as_u16() {
            200 | 409 => Ok(()),
            s if s >= 500 => Err(StorageError::Unavailable),
            s => Err(StorageError::Rejected(s)),
        }
    }
}

fn status_error(status: u16) -> StorageError {
    if status >= 500 || status == 429 {
        StorageError::Unavailable
    } else {
        StorageError::Rejected(status)
    }
}

#[async_trait]
impl ObjectStore for S3ObjectStore {
    fn backend(&self) -> &'static str {
        "s3"
    }

    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> StorageResult<StoredObject> {
        if !valid_key(key) {
            return Err(StorageError::Rejected(400));
        }
        let sha256 = sha256_hex(&bytes);
        let size = bytes.len() as u64;
        let mut extra: Vec<(&str, &str)> = vec![("content-type", content_type)];
        if let Some(sse) = self.config.server_side_encryption.as_deref() {
            extra.push(("x-amz-server-side-encryption", sse));
        }
        let res = self
            .request(reqwest::Method::PUT, Some(key), &sha256, &extra)
            .body(bytes)
            .send()
            .await
            .map_err(|_| StorageError::Unavailable)?;
        if !res.status().is_success() {
            return Err(status_error(res.status().as_u16()));
        }
        let encrypted = res.headers().get("x-amz-server-side-encryption").is_some();
        if self.config.server_side_encryption.is_some() && !encrypted {
            // Asked for encryption at rest and the store did not confirm it: refuse to
            // treat the object as stored (the caller retries / alerts).
            let _ = self.delete(key).await;
            return Err(StorageError::Integrity);
        }
        Ok(StoredObject {
            size,
            sha256,
            content_type: content_type.to_string(),
            encrypted,
        })
    }

    async fn head(&self, key: &str) -> StorageResult<Option<ObjectMeta>> {
        let empty = sha256_hex(b"");
        let res = self
            .request(reqwest::Method::HEAD, Some(key), &empty, &[])
            .send()
            .await
            .map_err(|_| StorageError::Unavailable)?;
        match res.status().as_u16() {
            404 => Ok(None),
            s if (200..300).contains(&s) => {
                let h = res.headers();
                Ok(Some(ObjectMeta {
                    size: h
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                    content_type: h
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string),
                    encrypted: h.get("x-amz-server-side-encryption").is_some(),
                }))
            }
            s => Err(status_error(s)),
        }
    }

    async fn get(&self, key: &str, max_bytes: usize) -> StorageResult<Option<Vec<u8>>> {
        if !valid_key(key) {
            return Err(StorageError::Rejected(400));
        }
        let empty = sha256_hex(b"");
        let mut res = self
            .request(reqwest::Method::GET, Some(key), &empty, &[])
            .send()
            .await
            .map_err(|_| StorageError::Unavailable)?;
        match res.status().as_u16() {
            404 => return Ok(None),
            s if (200..300).contains(&s) => {}
            s => return Err(status_error(s)),
        }
        if res.content_length().is_some_and(|n| n > max_bytes as u64) {
            return Err(StorageError::Integrity);
        }
        let mut out = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|_| StorageError::Unavailable)? {
            if out.len() + chunk.len() > max_bytes {
                return Err(StorageError::Integrity);
            }
            out.extend_from_slice(&chunk);
        }
        Ok(Some(out))
    }

    async fn delete(&self, key: &str) -> StorageResult<()> {
        let empty = sha256_hex(b"");
        let res = self
            .request(reqwest::Method::DELETE, Some(key), &empty, &[])
            .send()
            .await
            .map_err(|_| StorageError::Unavailable)?;
        match res.status().as_u16() {
            200 | 202 | 204 | 404 => Ok(()),
            s => Err(status_error(s)),
        }
    }

    fn signed_read(
        &self,
        key: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> StorageResult<SignedRead> {
        if !valid_key(key) {
            return Err(StorageError::Rejected(400));
        }
        let ttl = ttl.min(MAX_SIGNED_READ).max(Duration::from_secs(1));
        Ok(SignedRead {
            url: self.presign_get(key, ttl, now),
            expires_at: now + chrono::Duration::seconds(ttl.as_secs() as i64),
        })
    }

    fn signed_read_as(
        &self,
        key: &str,
        ttl: Duration,
        now: DateTime<Utc>,
        overrides: &ResponseOverrides,
    ) -> StorageResult<SignedRead> {
        if !valid_key(key) {
            return Err(StorageError::Rejected(400));
        }
        let ttl = ttl.min(MAX_SIGNED_READ).max(Duration::from_secs(1));
        let mut params = Vec::new();
        if let Some(ct) = &overrides.content_type {
            params.push(("response-content-type", ct.clone()));
        }
        if let Some(cd) = &overrides.content_disposition {
            params.push(("response-content-disposition", cd.clone()));
        }
        Ok(SignedRead {
            url: self.presign("GET", key, ttl, now, &[], &params),
            expires_at: now + chrono::Duration::seconds(ttl.as_secs() as i64),
        })
    }

    fn signed_write(
        &self,
        key: &str,
        ttl: Duration,
        now: DateTime<Utc>,
        size: u64,
        sha256_hex: &str,
        content_type: &str,
    ) -> StorageResult<SignedWrite> {
        if !valid_key(key) || content_type.is_empty() || content_type.len() > 255 {
            return Err(StorageError::Rejected(400));
        }
        let checksum = checksum_hex_to_b64(sha256_hex).ok_or(StorageError::Rejected(400))?;
        let ttl = ttl.min(MAX_SIGNED_WRITE).max(Duration::from_secs(1));
        let mut headers: Vec<(&str, String)> = vec![
            ("content-length", size.to_string()),
            ("content-type", content_type.to_string()),
            ("x-amz-checksum-sha256", checksum),
        ];
        if let Some(sse) = &self.config.server_side_encryption {
            headers.push(("x-amz-server-side-encryption", sse.clone()));
        }
        let url = self.presign("PUT", key, ttl, now, &headers, &[]);
        Ok(SignedWrite {
            method: "PUT",
            url,
            headers: headers
                .into_iter()
                .filter(|(k, _)| *k != "content-length")
                .map(|(k, v)| (k.to_string(), v))
                .chain(std::iter::once((
                    "content-length".to_string(),
                    size.to_string(),
                )))
                .collect(),
            expires_at: now + chrono::Duration::seconds(ttl.as_secs() as i64),
        })
    }

    async fn stored_sha256(&self, key: &str) -> StorageResult<Option<String>> {
        let empty = sha256_hex(b"");
        let res = self
            .request(
                reqwest::Method::HEAD,
                Some(key),
                &empty,
                &[("x-amz-checksum-mode", "ENABLED")],
            )
            .send()
            .await
            .map_err(|_| StorageError::Unavailable)?;
        match res.status().as_u16() {
            404 => Ok(None),
            s if (200..300).contains(&s) => Ok(res
                .headers()
                .get("x-amz-checksum-sha256")
                .and_then(|v| v.to_str().ok())
                .and_then(checksum_b64_to_hex)),
            s => Err(status_error(s)),
        }
    }

    async fn list(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        max: usize,
    ) -> StorageResult<Vec<ListedObject>> {
        let mut query: Vec<(&str, String)> = vec![
            ("list-type", "2".into()),
            ("prefix", prefix.to_string()),
            ("max-keys", max.clamp(1, 1000).to_string()),
        ];
        if let Some(a) = start_after {
            query.push(("start-after", a.to_string()));
        }
        let signed = self.sign_query("GET", &query, Utc::now());
        let mut rb = self.client.get(&signed.url);
        for (k, v) in signed.headers {
            rb = rb.header(k, v);
        }
        let res = rb.send().await.map_err(|_| StorageError::Unavailable)?;
        if !res.status().is_success() {
            return Err(status_error(res.status().as_u16()));
        }
        let text = res.text().await.map_err(|_| StorageError::Unavailable)?;
        Ok(parse_list(&text))
    }
}

/// Extracts `<Contents><Key>…</Key><Size>…</Size><LastModified>…</LastModified></Contents>`
/// from a ListObjectsV2 response (keys are restricted to [`valid_key`] characters, so no
/// XML entity can appear in a key the stores accept).
fn parse_list(xml: &str) -> Vec<ListedObject> {
    let tag = |s: &str, name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let a = s.find(&open)? + open.len();
        let b = s[a..].find(&close)? + a;
        Some(s[a..b].to_string())
    };
    xml.split("<Contents>")
        .skip(1)
        .filter_map(|c| {
            let key = tag(c, "Key")?;
            valid_key(&key).then(|| ListedObject {
                size: tag(c, "Size").and_then(|s| s.parse().ok()).unwrap_or(0),
                last_modified: tag(c, "LastModified")
                    .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
                    .map(|t| t.with_timezone(&Utc)),
                key,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// In-memory store (tests, local development)
// ---------------------------------------------------------------------------

/// An in-process store. Never for production data.
#[derive(Default)]
pub struct MemoryObjectStore {
    objects: Mutex<HashMap<String, (Vec<u8>, String)>>,
    unavailable: AtomicBool,
    /// Failure injection: writes store the bytes but report a different digest.
    corrupt_writes: AtomicBool,
    /// Failure injection: deletes fail (Unavailable) while everything else works.
    failing_deletes: AtomicBool,
}

impl MemoryObjectStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Failure injection: while unavailable every operation is
    /// [`StorageError::Unavailable`].
    pub fn set_available(&self, available: bool) {
        self.unavailable.store(!available, Ordering::SeqCst);
    }

    /// Failure injection: while set, `put` stores the object but reports a digest that
    /// does not match the bytes (an integrity failure discovered after the write).
    pub fn set_corrupt_writes(&self, corrupt: bool) {
        self.corrupt_writes.store(corrupt, Ordering::SeqCst);
    }

    /// Failure injection: while set, `delete` fails with [`StorageError::Unavailable`]
    /// (writes and reads keep working).
    pub fn set_failing_deletes(&self, failing: bool) {
        self.failing_deletes.store(failing, Ordering::SeqCst);
    }

    pub fn get_raw(&self, key: &str) -> Option<Vec<u8>> {
        self.objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .map(|(b, _)| b.clone())
    }

    pub fn len(&self) -> usize {
        self.objects.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Every stored key (sorted).
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Plays the client's part of a [`SignedWrite`] against this store, enforcing what an
    /// S3 store enforces: unexpired, the signed size, digest and media type.
    pub fn upload_signed(
        &self,
        write: &SignedWrite,
        now: DateTime<Utc>,
        bytes: Vec<u8>,
    ) -> StorageResult<()> {
        self.check()?;
        if now > write.expires_at {
            return Err(StorageError::Rejected(403));
        }
        let header = |n: &str| {
            write
                .headers
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.clone())
        };
        let key = write
            .url
            .strip_prefix("memory://upload/")
            .and_then(|r| r.split('?').next())
            .ok_or(StorageError::Rejected(400))?
            .to_string();
        if header("content-length").and_then(|v| v.parse::<u64>().ok()) != Some(bytes.len() as u64)
        {
            return Err(StorageError::Rejected(400));
        }
        if header("x-amz-checksum-sha256").and_then(|v| checksum_b64_to_hex(&v))
            != Some(sha256_hex(&bytes))
        {
            return Err(StorageError::Integrity);
        }
        let ct = header("content-type").unwrap_or_default();
        self.objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, (bytes, ct));
        Ok(())
    }

    /// Writes raw bytes without any check (simulates a misbehaving client or store).
    pub fn put_raw(&self, key: &str, bytes: Vec<u8>) {
        self.objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.to_string(), (bytes, "application/octet-stream".into()));
    }

    fn check(&self) -> StorageResult<()> {
        if self.unavailable.load(Ordering::SeqCst) {
            Err(StorageError::Unavailable)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl ObjectStore for MemoryObjectStore {
    fn backend(&self) -> &'static str {
        "memory"
    }

    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> StorageResult<StoredObject> {
        self.check()?;
        if !valid_key(key) {
            return Err(StorageError::Rejected(400));
        }
        let sha256 = if self.corrupt_writes.load(Ordering::SeqCst) {
            sha256_hex(&[bytes.as_slice(), b"corrupted"].concat())
        } else {
            sha256_hex(&bytes)
        };
        let out = StoredObject {
            size: bytes.len() as u64,
            sha256,
            content_type: content_type.to_string(),
            encrypted: true,
        };
        self.objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.to_string(), (bytes, content_type.to_string()));
        Ok(out)
    }

    async fn head(&self, key: &str) -> StorageResult<Option<ObjectMeta>> {
        self.check()?;
        Ok(self
            .objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .map(|(b, ct)| ObjectMeta {
                size: b.len() as u64,
                content_type: Some(ct.clone()),
                encrypted: true,
            }))
    }

    async fn get(&self, key: &str, max_bytes: usize) -> StorageResult<Option<Vec<u8>>> {
        self.check()?;
        match self.get_raw(key) {
            Some(b) if b.len() > max_bytes => Err(StorageError::Integrity),
            other => Ok(other),
        }
    }

    async fn delete(&self, key: &str) -> StorageResult<()> {
        self.check()?;
        if self.failing_deletes.load(Ordering::SeqCst) {
            return Err(StorageError::Unavailable);
        }
        self.objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(key);
        Ok(())
    }

    fn signed_read(
        &self,
        key: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> StorageResult<SignedRead> {
        self.check()?;
        if !valid_key(key) {
            return Err(StorageError::Rejected(400));
        }
        let ttl = ttl.min(MAX_SIGNED_READ);
        let expires_at = now + chrono::Duration::seconds(ttl.as_secs() as i64);
        // An opaque, non-guessable stand-in: the digest of key + expiry.
        let token = sha256_hex(format!("{key}|{}", expires_at.timestamp()).as_bytes());
        Ok(SignedRead {
            url: format!("memory://object/{token}?expires={}", expires_at.timestamp()),
            expires_at,
        })
    }

    fn signed_read_as(
        &self,
        key: &str,
        ttl: Duration,
        now: DateTime<Utc>,
        overrides: &ResponseOverrides,
    ) -> StorageResult<SignedRead> {
        let mut r = self.signed_read(key, ttl, now)?;
        if let Some(ct) = &overrides.content_type {
            r.url
                .push_str(&format!("&response-content-type={}", uri_encode(ct, false)));
        }
        if let Some(cd) = &overrides.content_disposition {
            r.url.push_str(&format!(
                "&response-content-disposition={}",
                uri_encode(cd, false)
            ));
        }
        Ok(r)
    }

    fn signed_write(
        &self,
        key: &str,
        ttl: Duration,
        now: DateTime<Utc>,
        size: u64,
        sha256_hex: &str,
        content_type: &str,
    ) -> StorageResult<SignedWrite> {
        self.check()?;
        if !valid_key(key) {
            return Err(StorageError::Rejected(400));
        }
        let checksum = checksum_hex_to_b64(sha256_hex).ok_or(StorageError::Rejected(400))?;
        let ttl = ttl.min(MAX_SIGNED_WRITE);
        let expires_at = now + chrono::Duration::seconds(ttl.as_secs() as i64);
        Ok(SignedWrite {
            method: "PUT",
            url: format!("memory://upload/{key}?expires={}", expires_at.timestamp()),
            headers: vec![
                ("content-type".into(), content_type.to_string()),
                ("x-amz-checksum-sha256".into(), checksum),
                ("content-length".into(), size.to_string()),
            ],
            expires_at,
        })
    }

    async fn stored_sha256(&self, key: &str) -> StorageResult<Option<String>> {
        self.check()?;
        Ok(self.get_raw(key).map(|b| sha256_hex(&b)))
    }

    async fn list(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        max: usize,
    ) -> StorageResult<Vec<ListedObject>> {
        self.check()?;
        let objects = self.objects.lock().unwrap_or_else(|p| p.into_inner());
        let mut keys: Vec<(&String, u64)> = objects
            .iter()
            .filter(|(k, _)| k.starts_with(prefix) && start_after.is_none_or(|a| k.as_str() > a))
            .map(|(k, (b, _))| (k, b.len() as u64))
            .collect();
        keys.sort();
        Ok(keys
            .into_iter()
            .take(max)
            .map(|(k, size)| ListedObject {
                key: k.clone(),
                size,
                last_modified: None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn checksum_encodings_round_trip_and_reject_garbage() {
        let hex_digest = sha256_hex(b"hello");
        let b64 = checksum_hex_to_b64(&hex_digest).unwrap();
        assert_eq!(b64, "LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=");
        assert_eq!(
            checksum_b64_to_hex(&b64).as_deref(),
            Some(hex_digest.as_str())
        );
        assert_eq!(checksum_hex_to_b64("abcd"), None);
        assert_eq!(checksum_b64_to_hex("!!!"), None);
        assert_eq!(checksum_b64_to_hex("aGVsbG8="), None, "not 32 bytes");
        for n in 0..10 {
            let data: Vec<u8> = (0..n).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
    }

    #[test]
    fn list_responses_parse_only_valid_keys() {
        let xml = "<ListBucketResult><Contents><Key>a/1.bin</Key><Size>12</Size><LastModified>2026-10-18T10:00:00.000Z</LastModified></Contents>\
                   <Contents><Key>bad key&amp;</Key><Size>1</Size></Contents><Contents><Key>a/2.bin</Key><Size>3</Size></Contents></ListBucketResult>";
        let l = parse_list(xml);
        assert_eq!(
            l.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
            vec!["a/1.bin", "a/2.bin"]
        );
        assert_eq!(l[0].size, 12);
        assert!(l[0].last_modified.is_some());
    }

    #[tokio::test]
    async fn memory_signed_writes_enforce_size_digest_and_expiry() {
        let store = MemoryObjectStore::new();
        let now = Utc::now();
        let body = b"ciphertext-bytes".to_vec();
        let w = store
            .signed_write(
                "x/obj",
                Duration::from_secs(60),
                now,
                body.len() as u64,
                &sha256_hex(&body),
                "application/octet-stream",
            )
            .unwrap();
        assert!(
            store
                .upload_signed(&w, now, b"other-bytes-xxxx".to_vec())
                .is_err(),
            "wrong digest"
        );
        assert!(
            store.upload_signed(&w, now, b"short".to_vec()).is_err(),
            "wrong size"
        );
        assert!(
            store
                .upload_signed(&w, now + chrono::Duration::seconds(61), body.clone())
                .is_err(),
            "expired"
        );
        store.upload_signed(&w, now, body.clone()).unwrap();
        assert_eq!(
            store.stored_sha256("x/obj").await.unwrap(),
            Some(sha256_hex(&body))
        );
        assert_eq!(store.list("x/", None, 10).await.unwrap().len(), 1);
        assert!(store.list("y/", None, 10).await.unwrap().is_empty());
        let r = store
            .signed_read_as(
                "x/obj",
                Duration::from_secs(60),
                now,
                &ResponseOverrides {
                    content_type: Some("application/octet-stream".into()),
                    content_disposition: Some("attachment".into()),
                },
            )
            .unwrap();
        assert!(r.url.contains("response-content-disposition=attachment"));
        // Unconfigured stores refuse instead of silently ignoring.
        assert_eq!(
            UnconfiguredStore
                .signed_write(
                    "k",
                    Duration::from_secs(1),
                    now,
                    1,
                    &sha256_hex(b"x"),
                    "a/b"
                )
                .unwrap_err(),
            StorageError::NotConfigured
        );
    }

    fn aws_example() -> S3ObjectStore {
        S3ObjectStore::new(S3Config {
            endpoint: "https://s3.amazonaws.com".into(),
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

    /// The published AWS SigV4 query-string example (S3 GET Object, 86400 s).
    #[test]
    fn presigned_reads_match_the_aws_signature_v4_example() {
        let store = aws_example();
        let now = Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap();
        let url = store.presign_get("test.txt", Duration::from_secs(86400), now);
        assert_eq!(
            url,
            "https://examplebucket.s3.amazonaws.com/test.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host\
             &X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        );
    }

    /// The published AWS SigV4 header example (S3 GET Object with a Range header is not
    /// used here; the PUT example's signing key derivation is exercised through sign()).
    #[test]
    fn header_signing_is_deterministic_and_covers_extra_headers() {
        let store = aws_example();
        let now = Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap();
        let a = store.sign(
            "PUT",
            Some("a/b.wav"),
            "abc",
            &[("content-type", "audio/wav")],
            now,
        );
        let b = store.sign(
            "PUT",
            Some("a/b.wav"),
            "abc",
            &[("content-type", "audio/wav")],
            now,
        );
        let auth = |s: &Signed| {
            s.headers
                .iter()
                .find(|(k, _)| k == "authorization")
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(auth(&a), auth(&b));
        assert!(
            auth(&a).contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date")
        );
        let c = store.sign(
            "PUT",
            Some("a/b.wav"),
            "abc",
            &[("content-type", "audio/mpeg")],
            now,
        );
        assert_ne!(auth(&a), auth(&c));
        assert_eq!(a.url, "https://examplebucket.s3.amazonaws.com/a/b.wav");
    }

    #[test]
    fn keys_are_bounded_and_never_traverse() {
        assert!(valid_key("voicemail/t/a/b/c.wav"));
        for bad in [
            "",
            "/abs",
            "a/../b",
            "a b",
            "a?b",
            "a%2Fb",
            &"x".repeat(513),
        ] {
            assert!(!valid_key(bad), "{bad}");
        }
    }

    #[test]
    fn signed_reads_are_short_lived() {
        let store = aws_example();
        let now = Utc.with_ymd_and_hms(2031, 1, 1, 0, 0, 0).unwrap();
        let read = store
            .signed_read("k.wav", Duration::from_secs(86400), now)
            .unwrap();
        assert_eq!(read.expires_at, now + chrono::Duration::seconds(900));
        assert!(read.url.contains("X-Amz-Expires=900"));
        assert!(store
            .signed_read("../etc", Duration::from_secs(60), now)
            .is_err());
    }

    #[tokio::test]
    async fn memory_store_round_trip_idempotent_delete_and_failure_injection() {
        let store = MemoryObjectStore::new();
        let stored = store
            .put("v/1.wav", b"RIFF....".to_vec(), "audio/wav")
            .await
            .unwrap();
        assert_eq!(stored.size, 8);
        assert_eq!(stored.sha256, sha256_hex(b"RIFF...."));
        assert!(store.head("v/1.wav").await.unwrap().is_some());
        assert_eq!(
            store.get("v/1.wav", 8).await.unwrap(),
            Some(b"RIFF....".to_vec())
        );
        assert_eq!(store.get("v/1.wav", 7).await, Err(StorageError::Integrity));
        store.delete("v/1.wav").await.unwrap();
        store.delete("v/1.wav").await.unwrap();
        assert!(store.head("v/1.wav").await.unwrap().is_none());
        assert_eq!(store.get("v/1.wav", 8).await.unwrap(), None);
        store.set_available(false);
        assert_eq!(
            store.get("v/2.wav", 8).await,
            Err(StorageError::Unavailable)
        );
        assert_eq!(
            store.put("v/2.wav", vec![1], "audio/wav").await,
            Err(StorageError::Unavailable)
        );
        store.set_available(true);
        assert!(store.put("v/2.wav", vec![1], "audio/wav").await.is_ok());
    }

    struct TempSecret(std::path::PathBuf);

    impl TempSecret {
        fn new(contents: &str, mode: u32) -> Self {
            let path = std::env::temp_dir().join(format!(
                "cf-object-storage-secret-{}-{}",
                std::process::id(),
                sha256_hex(
                    format!("{contents}{mode}{:?}", std::time::SystemTime::now()).as_bytes()
                )
            ));
            std::fs::write(&path, contents).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            #[cfg(not(unix))]
            let _ = mode;
            Self(path)
        }
    }

    impl Drop for TempSecret {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn lookup(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.clone())
        }
    }

    const SECRET: &str = "s3-secret-access-key-0123456789abcdef";

    fn base() -> Vec<(&'static str, String)> {
        vec![
            ("X_ENDPOINT", "http://127.0.0.1:9000".into()),
            ("X_BUCKET", "artifacts".into()),
            ("X_ACCESS_KEY_ID", "access".into()),
        ]
    }

    #[test]
    fn the_secret_access_key_may_be_a_secret_file() {
        let file = TempSecret::new(&format!("{SECRET}\n"), 0o600);
        let mut pairs = base();
        pairs.push(("CF_RUNTIME_ENVIRONMENT", "production".into()));
        pairs.push((
            "X_SECRET_ACCESS_KEY_FILE",
            file.0.to_string_lossy().into_owned(),
        ));
        let config = S3Config::from_lookup("X_", &lookup(pairs))
            .unwrap()
            .unwrap();
        assert_eq!(config.secret_key, SECRET);
        assert!(!format!("{config:?}").contains(SECRET));
        // Inline still works.
        let mut pairs = base();
        pairs.push(("X_SECRET_ACCESS_KEY", SECRET.into()));
        assert_eq!(
            S3Config::from_lookup("X_", &lookup(pairs))
                .unwrap()
                .unwrap()
                .secret_key,
            SECRET
        );
    }

    #[test]
    fn ambiguous_missing_permissive_or_placeholder_secrets_are_refused() {
        let file = TempSecret::new(SECRET, 0o600);
        let path = file.0.to_string_lossy().into_owned();
        // Both set.
        let mut pairs = base();
        pairs.push(("X_SECRET_ACCESS_KEY", SECRET.into()));
        pairs.push(("X_SECRET_ACCESS_KEY_FILE", path.clone()));
        let err = S3Config::from_lookup("X_", &lookup(pairs)).unwrap_err();
        assert!(
            err.contains("exactly one") && !err.contains(SECRET),
            "{err}"
        );
        // Missing file: an error, never a fallback.
        let mut pairs = base();
        pairs.push(("X_SECRET_ACCESS_KEY_FILE", "/nonexistent/s3-secret".into()));
        let err = S3Config::from_lookup("X_", &lookup(pairs)).unwrap_err();
        assert!(!err.contains("/nonexistent"), "{err}");
        // Neither set.
        assert!(S3Config::from_lookup("X_", &lookup(base())).is_err());
        // Placeholder.
        let mut pairs = base();
        pairs.push(("X_SECRET_ACCESS_KEY", "changeme-changeme".into()));
        assert!(S3Config::from_lookup("X_", &lookup(pairs)).is_err());
        #[cfg(unix)]
        {
            let readable = TempSecret::new(SECRET, 0o644);
            let readable_path = readable.0.to_string_lossy().into_owned();
            for runtime in [Some("production"), Some("staging"), None] {
                let mut pairs = base();
                pairs.push(("X_SECRET_ACCESS_KEY_FILE", readable_path.clone()));
                if let Some(r) = runtime {
                    pairs.push(("CF_RUNTIME_ENVIRONMENT", r.into()));
                }
                let err = S3Config::from_lookup("X_", &lookup(pairs)).unwrap_err();
                assert!(err.contains("group or others"), "{runtime:?}: {err}");
            }
            let mut pairs = base();
            pairs.push(("X_SECRET_ACCESS_KEY_FILE", readable_path));
            pairs.push(("CF_RUNTIME_ENVIRONMENT", "local".into()));
            assert!(S3Config::from_lookup("X_", &lookup(pairs)).is_ok());
        }
    }

    #[tokio::test]
    async fn memory_store_corrupt_writes_and_failing_deletes() {
        let store = MemoryObjectStore::new();
        store.set_corrupt_writes(true);
        let stored = store
            .put("v/3.wav", b"RIFF".to_vec(), "audio/wav")
            .await
            .unwrap();
        assert_ne!(stored.sha256, sha256_hex(b"RIFF"));
        assert_eq!(store.len(), 1, "the object is written");
        store.set_corrupt_writes(false);
        store.set_failing_deletes(true);
        assert_eq!(
            store.delete("v/3.wav").await,
            Err(StorageError::Unavailable)
        );
        assert_eq!(store.len(), 1);
        assert!(store.head("v/3.wav").await.unwrap().is_some());
        store.set_failing_deletes(false);
        store.delete("v/3.wav").await.unwrap();
        assert!(store.is_empty());
    }
}
