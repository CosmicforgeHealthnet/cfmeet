//! Self-hosted malware scanning of MANAGED attachments (§72).
//!
//! Two adapters share one fail-closed contract:
//!
//! * [`GatewayScanner`] (production): the CosmicForge shared security scan gateway in
//!   front of an EXISTING ClamAV engine. Each request carries a fresh single-use service
//!   token of `cfmeet.messaging` and the SHA-256 of the bytes; only a CLEAN whose Ed25519
//!   verdict signature verifies AND is bound to this request id, this caller, this runtime
//!   environment, this digest and this length counts. Everything else is "unavailable".
//! * [`ClamAvScanner`] (local / test, legacy): a dedicated clamd. It speaks clamd's
//!   `INSTREAM` protocol over TCP (`MESSAGING_CLAMAV_ADDRESS`): `zINSTREAM\0`, then
//!   4-byte big-endian length-prefixed chunks, a zero-length terminator, and a
//!   `stream: OK` / `stream: <name> FOUND` reply.
//!
//! Neither adapter uses an external (paid / third-party) scanning service.
//!
//! Policy (fail closed): where scanning is required, an attachment becomes READY only
//! after a CLEAN result; an unavailable scanner leaves it VERIFYING (retried, never
//! downloadable). PRIVATE_E2EE objects are ciphertext: they are NEVER sent to a scanner
//! and never reported as scanned (`NOT_APPLICABLE_ENCRYPTED`).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanOutcome {
    Clean,
    Infected(String),
    /// The scanner could not give an answer (retry later; never treated as clean).
    Unavailable(String),
}

#[async_trait::async_trait]
pub trait AttachmentScanner: Send + Sync {
    fn engine(&self) -> &'static str;
    fn configured(&self) -> bool {
        true
    }
    async fn scan(&self, bytes: &[u8]) -> ScanOutcome;
}

pub struct UnconfiguredScanner;

#[async_trait::async_trait]
impl AttachmentScanner for UnconfiguredScanner {
    fn engine(&self) -> &'static str {
        "none"
    }

    fn configured(&self) -> bool {
        false
    }

    async fn scan(&self, _bytes: &[u8]) -> ScanOutcome {
        ScanOutcome::Unavailable("no scanner configured".into())
    }
}

pub struct ClamAvScanner {
    address: String,
    timeout: Duration,
}

impl ClamAvScanner {
    pub fn new(address: impl Into<String>, timeout: Duration) -> Self {
        Self {
            address: address.into(),
            timeout,
        }
    }

    async fn run(&self, bytes: &[u8]) -> std::io::Result<String> {
        let mut s = tokio::net::TcpStream::connect(&self.address).await?;
        s.write_all(b"zINSTREAM\0").await?;
        for chunk in bytes.chunks(64 * 1024) {
            s.write_all(&(chunk.len() as u32).to_be_bytes()).await?;
            s.write_all(chunk).await?;
        }
        s.write_all(&0u32.to_be_bytes()).await?;
        let mut reply = Vec::new();
        s.read_to_end(&mut reply).await?;
        Ok(String::from_utf8_lossy(&reply)
            .trim_end_matches(['\0', '\n'])
            .to_string())
    }
}

/// Interprets a clamd reply.
pub fn parse_reply(reply: &str) -> ScanOutcome {
    let body = reply
        .strip_prefix("stream:")
        .map(str::trim)
        .unwrap_or(reply.trim());
    if body == "OK" {
        ScanOutcome::Clean
    } else if let Some(name) = body.strip_suffix("FOUND") {
        let name: String = name
            .trim()
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(128)
            .collect();
        ScanOutcome::Infected(if name.is_empty() {
            "UNKNOWN".into()
        } else {
            name
        })
    } else {
        ScanOutcome::Unavailable(format!(
            "scanner error: {}",
            body.chars().take(80).collect::<String>()
        ))
    }
}

#[async_trait::async_trait]
impl AttachmentScanner for ClamAvScanner {
    fn engine(&self) -> &'static str {
        "clamav"
    }

    async fn scan(&self, bytes: &[u8]) -> ScanOutcome {
        match tokio::time::timeout(self.timeout, self.run(bytes)).await {
            Ok(Ok(reply)) => parse_reply(&reply),
            Ok(Err(e)) => ScanOutcome::Unavailable(format!("scanner unreachable: {}", e.kind())),
            Err(_) => ScanOutcome::Unavailable("scanner timeout".into()),
        }
    }
}

/// The CosmicForge shared security scan gateway (see the module documentation).
pub struct GatewayScanner {
    http: reqwest::Client,
    url: String,
    signer: std::sync::Arc<cf_service_auth::ServiceTokenSigner>,
    keys: cf_scan_gateway_contract::VerdictKeys,
}

/// Largest gateway answer read (a verdict is a few hundred bytes).
const MAX_GATEWAY_RESPONSE: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum GatewayConfigError {
    #[error("the scan gateway URL must use HTTPS outside local / test runtimes")]
    InsecureUrl,
    #[error("the verdict key set cannot be read")]
    KeysUnreadable,
    #[error("{0}")]
    Keys(#[from] cf_scan_gateway_contract::VerdictError),
    #[error("HTTP client cannot be built")]
    Http,
}

impl GatewayScanner {
    pub fn new(
        base_url: &str,
        keys_json: &str,
        signer: std::sync::Arc<cf_service_auth::ServiceTokenSigner>,
        timeout: Duration,
    ) -> Result<Self, GatewayConfigError> {
        if !base_url.starts_with("https://")
            && !signer.runtime_environment().allows_insecure_transport()
        {
            return Err(GatewayConfigError::InsecureUrl);
        }
        let keys = cf_scan_gateway_contract::VerdictKeys::from_json(keys_json)?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| GatewayConfigError::Http)?;
        Ok(Self {
            http,
            url: format!("{base_url}{}", cf_scan_gateway_contract::SCAN_PATH),
            signer,
            keys,
        })
    }

    pub fn from_files(
        base_url: &str,
        keys_file: &std::path::Path,
        signer: std::sync::Arc<cf_service_auth::ServiceTokenSigner>,
        timeout: Duration,
    ) -> Result<Self, GatewayConfigError> {
        let keys =
            std::fs::read_to_string(keys_file).map_err(|_| GatewayConfigError::KeysUnreadable)?;
        Self::new(base_url, &keys, signer, timeout)
    }

    async fn exchange(&self, bytes: &[u8]) -> ScanOutcome {
        use cf_scan_gateway_contract::{Expected, ScanResponse, Verdict};
        use cf_service_auth::{OutboundServiceRequest, ServiceId};
        let digest = cf_object_storage::sha256_hex(bytes);
        let signed = match self.signer.sign(&OutboundServiceRequest::new_operation(
            ServiceId::security_scan(),
        )) {
            Ok(s) => s,
            Err(_) => return ScanOutcome::Unavailable("scan gateway token not signed".into()),
        };
        let response = self
            .http
            .post(&self.url)
            .headers(signed.headers())
            .header(
                reqwest::header::CONTENT_TYPE,
                cf_scan_gateway_contract::BODY_CONTENT_TYPE,
            )
            .header(cf_scan_gateway_contract::CONTENT_SHA256_HEADER, &digest)
            .header(reqwest::header::CONTENT_LENGTH, bytes.len())
            .body(bytes.to_vec())
            .send()
            .await;
        let mut response = match response {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return ScanOutcome::Unavailable("scan gateway timeout".into())
            }
            Err(_) => return ScanOutcome::Unavailable("scan gateway unreachable".into()),
        };
        let status = response.status().as_u16();
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if body.len() + chunk.len() <= MAX_GATEWAY_RESPONSE => {
                    body.extend_from_slice(&chunk)
                }
                Ok(None) => break,
                _ => {
                    return ScanOutcome::Unavailable(format!(
                        "scan gateway answer unusable ({status})"
                    ))
                }
            }
        }
        let Ok(verdict) = serde_json::from_slice::<ScanResponse>(&body) else {
            return ScanOutcome::Unavailable(format!("scan gateway refused ({status})"));
        };
        if !verdict.verdict.is_definitive() {
            return ScanOutcome::Unavailable(format!(
                "scan gateway {}: {}",
                verdict.verdict.as_str(),
                verdict.reason.map(|r| r.as_str()).unwrap_or("NONE")
            ));
        }
        let environment = self.signer.runtime_environment();
        let expected = Expected {
            request_id: signed.request_id().as_str(),
            client_id: self.signer.issuer().as_str(),
            environment: environment.as_str(),
            sha256: &digest,
            bytes: bytes.len() as u64,
        };
        if status != 200 || self.keys.verify(&verdict, expected).is_err() {
            // A definitive-looking answer that does not verify is treated as no answer.
            return ScanOutcome::Unavailable("scan gateway verdict did not verify".into());
        }
        match verdict.verdict {
            Verdict::Clean => ScanOutcome::Clean,
            Verdict::Infected => ScanOutcome::Infected(
                verdict
                    .finding
                    .unwrap_or_default()
                    .chars()
                    .filter(|c| c.is_ascii_graphic() || *c == ' ')
                    .take(128)
                    .collect(),
            ),
            _ => ScanOutcome::Unavailable("scan gateway verdict not definitive".into()),
        }
    }
}

#[async_trait::async_trait]
impl AttachmentScanner for GatewayScanner {
    fn engine(&self) -> &'static str {
        "clamav-gateway"
    }

    async fn scan(&self, bytes: &[u8]) -> ScanOutcome {
        self.exchange(bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamd_replies_are_interpreted_fail_closed() {
        assert_eq!(parse_reply("stream: OK"), ScanOutcome::Clean);
        assert_eq!(
            parse_reply("stream: Eicar-Test-Signature FOUND"),
            ScanOutcome::Infected("Eicar-Test-Signature".into())
        );
        assert!(matches!(
            parse_reply("INSTREAM size limit exceeded. ERROR"),
            ScanOutcome::Unavailable(_)
        ));
        assert!(matches!(parse_reply(""), ScanOutcome::Unavailable(_)));
    }

    #[tokio::test]
    async fn an_unreachable_scanner_is_unavailable_never_clean() {
        let s = ClamAvScanner::new("127.0.0.1:1", Duration::from_millis(500));
        assert!(matches!(s.scan(b"x").await, ScanOutcome::Unavailable(_)));
        assert!(matches!(
            UnconfiguredScanner.scan(b"x").await,
            ScanOutcome::Unavailable(_)
        ));
    }

    mod gateway {
        use super::super::*;
        use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
        use cf_scan_gateway_contract::{
            public_key, sign, EngineFacts, Reason, ScanResponse, Verdict, CONTRACT_VERSION,
        };
        use cf_service_auth::{
            KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
            SystemClock, TokenTtl,
        };
        use std::sync::Arc;

        #[derive(Clone, Copy, PartialEq)]
        enum Mode {
            SignedClean,
            SignedInfected,
            Unsigned,
            ForeignKey,
            OtherRequest,
            OtherDigest,
            Unavailable,
            Garbage,
        }

        struct Fake {
            mode: Mode,
            key: ed25519_dalek::SigningKey,
            foreign: ed25519_dalek::SigningKey,
        }

        async fn handler(
            State(f): State<Arc<Fake>>,
            headers: HeaderMap,
            body: axum::body::Bytes,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            let request_id = headers["x-cf-request-id"].to_str().unwrap().to_string();
            assert!(headers.contains_key("x-cf-service-authorization"));
            let digest = headers[cf_scan_gateway_contract::CONTENT_SHA256_HEADER]
                .to_str()
                .unwrap()
                .to_string();
            assert_eq!(digest, cf_object_storage::sha256_hex(&body));
            if f.mode == Mode::Garbage {
                return (axum::http::StatusCode::OK, "stream: OK").into_response();
            }
            let infected = f.mode == Mode::SignedInfected;
            let mut r = ScanResponse {
                contract_version: CONTRACT_VERSION.into(),
                request_id: if f.mode == Mode::OtherRequest {
                    "req_other".into()
                } else {
                    request_id
                },
                client_id: "cfmeet.messaging".into(),
                product: "cfmeet".into(),
                environment: "production".into(),
                verdict: if infected {
                    Verdict::Infected
                } else {
                    Verdict::Clean
                },
                reason: None,
                finding: infected.then(|| "Eicar-Test-Signature".to_string()),
                sha256: if f.mode == Mode::OtherDigest {
                    "0".repeat(64)
                } else {
                    digest
                },
                bytes: body.len() as u64,
                engine: Some(EngineFacts {
                    name: "clamav".into(),
                    version: "1.4.6".into(),
                    signature_version: 28173,
                    signature_age_seconds: 60,
                }),
                scanned_at: 1,
                verdict_key_id: None,
                verdict_signature: None,
            };
            match f.mode {
                Mode::Unsigned => {}
                Mode::ForeignKey => sign(&mut r, "v1", &f.foreign).unwrap(),
                Mode::Unavailable => {
                    r.verdict = Verdict::Unavailable;
                    r.reason = Some(Reason::SignaturesStale);
                    r.engine = None;
                    return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(r)).into_response();
                }
                _ => sign(&mut r, "v1", &f.key).unwrap(),
            }
            Json(r).into_response()
        }

        async fn scanner(mode: Mode) -> GatewayScanner {
            let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
            let foreign = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
            let keys = serde_json::json!({ "keys": [public_key("v1", &key)] }).to_string();
            let fake = Arc::new(Fake { mode, key, foreign });
            let app = Router::new()
                .route(cf_scan_gateway_contract::SCAN_PATH, post(handler))
                .with_state(fake);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await });
            GatewayScanner::new(
                &url,
                &keys,
                signer(RuntimeEnvironment::Test),
                Duration::from_secs(5),
            )
            .unwrap()
        }

        fn signer(env: RuntimeEnvironment) -> Arc<ServiceTokenSigner> {
            Arc::new(ServiceTokenSigner::new(
                ServiceId::messaging(),
                env,
                ServiceSigningKey::generate(KeyId::parse("messaging-1").unwrap()),
                TokenTtl::default(),
                Arc::new(SystemClock),
            ))
        }

        // The fake signs "production" while the caller runs "test": a verdict for another
        // runtime environment never counts. Re-sign in the caller's own environment.
        async fn scan_as(mode: Mode, env_matches: bool) -> ScanOutcome {
            let mut s = scanner(mode).await;
            if env_matches {
                s.signer = Arc::new(ServiceTokenSigner::new(
                    ServiceId::messaging(),
                    RuntimeEnvironment::Production,
                    ServiceSigningKey::generate(KeyId::parse("messaging-1").unwrap()),
                    TokenTtl::default(),
                    Arc::new(SystemClock),
                ));
            }
            s.scan(b"managed attachment bytes").await
        }

        #[tokio::test]
        async fn only_a_signed_verdict_bound_to_this_request_and_content_counts() {
            assert_eq!(scan_as(Mode::SignedClean, true).await, ScanOutcome::Clean);
            assert_eq!(
                scan_as(Mode::SignedInfected, true).await,
                ScanOutcome::Infected("Eicar-Test-Signature".into())
            );
            for mode in [
                Mode::Unsigned,
                Mode::ForeignKey,
                Mode::OtherRequest,
                Mode::OtherDigest,
                Mode::Unavailable,
                Mode::Garbage,
            ] {
                assert!(matches!(
                    scan_as(mode, true).await,
                    ScanOutcome::Unavailable(_)
                ));
            }
            // A genuine CLEAN for another runtime environment is not accepted either.
            assert!(matches!(
                scan_as(Mode::SignedClean, false).await,
                ScanOutcome::Unavailable(_)
            ));
        }

        #[tokio::test]
        async fn an_unreachable_gateway_is_unavailable() {
            let keys = serde_json::json!({ "keys": [public_key("v1", &ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]))] }).to_string();
            let s = GatewayScanner::new(
                "http://127.0.0.1:1",
                &keys,
                signer(RuntimeEnvironment::Test),
                Duration::from_secs(5),
            )
            .unwrap();
            assert!(matches!(s.scan(b"x").await, ScanOutcome::Unavailable(_)));
        }

        #[test]
        fn production_requires_https_and_a_valid_key_set() {
            let keys = serde_json::json!({ "keys": [public_key("v1", &ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]))] }).to_string();
            assert!(matches!(
                GatewayScanner::new(
                    "http://scan",
                    &keys,
                    signer(RuntimeEnvironment::Production),
                    Duration::from_secs(5)
                ),
                Err(GatewayConfigError::InsecureUrl)
            ));
            assert!(GatewayScanner::new(
                "https://scan",
                &keys,
                signer(RuntimeEnvironment::Production),
                Duration::from_secs(5)
            )
            .is_ok());
            assert!(GatewayScanner::new(
                "https://scan",
                r#"{"keys":[]}"#,
                signer(RuntimeEnvironment::Production),
                Duration::from_secs(5)
            )
            .is_err());
        }
    }
}
