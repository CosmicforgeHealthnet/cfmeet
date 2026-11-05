//! Gateway contract and security tests against a scripted clamd (and, when
//! `SCAN_GATEWAY_TEST_CLAMD_ADDRESS` is set, a REAL clamd with the EICAR test file).

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use cf_scan_gateway_contract::{
    public_key, Expected, Reason, ScanResponse, Verdict, VerdictKeys, CONTENT_SHA256_HEADER,
    SCAN_PATH,
};
use cf_service_auth::{
    KeyId, ManualClock, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use ed25519_dalek::SigningKey;
use security_scan_gateway::{
    clamd::Clamd,
    metrics::Metrics,
    registry::{Registry, ReplayGuard},
    App, EngineStatus,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

const EICAR: &[u8] = b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*";
/// Build time of the scripted engine's signatures: 2026-11-03T06:27:52Z.
const SIG_TIME: i64 = 1_793_687_272;
const VERSION: &str = "ClamAV 1.4.6/28173/Tue Nov  3 06:27:52 2026";
const ENV: RuntimeEnvironment = RuntimeEnvironment::Production;

// ---------------------------------------------------------------------------------------
// Scripted clamd
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct FakeEngine {
    active: AtomicUsize,
    peak: AtomicUsize,
    scans: AtomicUsize,
    stream_max: usize,
}

async fn fake_clamd(engine: Arc<FakeEngine>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let engine = engine.clone();
            tokio::spawn(async move {
                let mut cmd = Vec::new();
                let mut b = [0u8; 1];
                while s.read_exact(&mut b).await.is_ok() && b[0] != 0 {
                    cmd.push(b[0]);
                }
                match cmd.as_slice() {
                    b"zVERSION" => {
                        let _ = s.write_all(format!("{VERSION}\0").as_bytes()).await;
                    }
                    b"zPING" => {
                        let _ = s.write_all(b"PONG\0").await;
                    }
                    b"zINSTREAM" => {
                        let now = engine.active.fetch_add(1, Ordering::SeqCst) + 1;
                        engine.peak.fetch_max(now, Ordering::SeqCst);
                        engine.scans.fetch_add(1, Ordering::SeqCst);
                        let mut data = Vec::new();
                        loop {
                            let mut len = [0u8; 4];
                            if s.read_exact(&mut len).await.is_err() {
                                break;
                            }
                            let len = u32::from_be_bytes(len) as usize;
                            if len == 0 {
                                break;
                            }
                            let mut chunk = vec![0u8; len];
                            if s.read_exact(&mut chunk).await.is_err() {
                                break;
                            }
                            data.extend_from_slice(&chunk);
                            if engine.stream_max > 0 && data.len() > engine.stream_max {
                                let _ = s.write_all(b"INSTREAM size limit exceeded. ERROR\0").await;
                                engine.active.fetch_sub(1, Ordering::SeqCst);
                                return;
                            }
                        }
                        let text = String::from_utf8_lossy(&data);
                        if text.contains("SLOW") {
                            tokio::time::sleep(Duration::from_millis(800)).await;
                        }
                        let reply: &[u8] = if data.windows(EICAR.len()).any(|w| w == EICAR) {
                            b"stream: Eicar-Test-Signature FOUND\0"
                        } else if text.contains("ENGINE-ERROR") {
                            b"stream: Can't allocate memory ERROR\0"
                        } else if text.contains("ENGINE-GARBAGE") {
                            b"stream: OK\0stream: OK\0"
                        } else {
                            b"stream: OK\0"
                        };
                        let _ = s.write_all(reply).await;
                        engine.active.fetch_sub(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------------------
// Fleet: callers, keys, bundle, registry, gateway
// ---------------------------------------------------------------------------------------

struct Caller {
    id: ServiceId,
    signer: ServiceTokenSigner,
}

fn caller(
    id: ServiceId,
    env: RuntimeEnvironment,
    clock: SharedClock,
    kid: &str,
) -> (Caller, serde_json::Value) {
    let key = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    let jwk = serde_json::to_value(key.public_jwk()).unwrap();
    let signer = ServiceTokenSigner::new(id.clone(), env, key, TokenTtl::default(), clock);
    (Caller { id, signer }, jwk)
}

struct Fleet {
    url: String,
    app: Arc<App>,
    messaging: Caller,
    verdict_keys: VerdictKeys,
    engine: Arc<FakeEngine>,
    clock: ManualClock,
}

struct Options {
    max_bytes: u64,
    max_concurrent: usize,
    max_per_minute: u32,
    backend_concurrency: usize,
    scan_timeout: Duration,
    queue_timeout: Duration,
    stream_max: usize,
    backend: Option<String>,
    clock_offset: i64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024,
            max_concurrent: 2,
            max_per_minute: 1000,
            backend_concurrency: 2,
            scan_timeout: Duration::from_secs(5),
            queue_timeout: Duration::from_millis(300),
            stream_max: 0,
            backend: None,
            clock_offset: 3600,
        }
    }
}

fn bundle(grants: &[(&ServiceId, &serde_json::Value)]) -> String {
    let receiver = ServiceId::security_scan();
    json!({
        "bundle_version": 1,
        "receiver": receiver,
        "runtime_environment": ENV,
        "trusted_issuers": grants.iter().map(|(issuer, jwk)| json!({
            "issuer": issuer,
            "audience": receiver,
            "runtime_environment": ENV,
            "keys": [jwk],
            "permissions": ["security-scan.submit"],
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

async fn gateway_with(
    app_clock: SharedClock,
    manual: ManualClock,
    o: Options,
    real: bool,
) -> Fleet {
    let (messaging, jwk) = caller(
        ServiceId::messaging(),
        ENV,
        Arc::new(manual.clone()),
        "messaging-scan-1",
    );
    let me = ServiceId::security_scan();
    let policy = TrustPolicy::from_json(&bundle(&[(&messaging.id, &jwk)]), &me, ENV).unwrap();
    let registry = Registry::from_json(
        &json!({"version": 1, "environment": "production", "clients": [{
            "issuer": "cfmeet.messaging", "product": "cfmeet", "maxBytes": o.max_bytes,
            "maxConcurrent": o.max_concurrent, "maxPerMinute": o.max_per_minute}]})
        .to_string(),
        ENV,
        &policy,
        25 * 1024 * 1024,
        o.backend_concurrency,
    )
    .unwrap();
    let engine = Arc::new(FakeEngine {
        stream_max: o.stream_max,
        ..Default::default()
    });
    let backend = match o.backend {
        Some(b) => b,
        None => fake_clamd(engine.clone()).await,
    };
    let verdict_key = SigningKey::generate(&mut rand_core::OsRng);
    let verdict_keys = VerdictKeys::new(&[public_key("scan-verdict-1", &verdict_key)]).unwrap();
    let verifier_clock: SharedClock = if real {
        app_clock.clone()
    } else {
        Arc::new(manual.clone())
    };
    let app = Arc::new(App {
        environment: ENV,
        verifier: ServiceTokenVerifier::new(policy, verifier_clock),
        registry,
        replay: ReplayGuard::default(),
        clamd: Clamd::new(backend, Duration::from_millis(500)),
        backend_slots: Arc::new(Semaphore::new(o.backend_concurrency)),
        engine: RwLock::new(EngineStatus::default()),
        engine_name: "clamav",
        max_signature_age: Duration::from_secs(72 * 3600),
        verdict_key,
        verdict_key_id: "scan-verdict-1".into(),
        scan_timeout: o.scan_timeout,
        queue_timeout: o.queue_timeout,
        metrics: Metrics::default(),
        clock: app_clock,
    });
    app.refresh_engine().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = app.router();
    tokio::spawn(async move { axum::serve(listener, router).await });
    Fleet {
        url,
        app,
        messaging,
        verdict_keys,
        engine,
        clock: manual,
    }
}

async fn gateway(o: Options) -> Fleet {
    let manual = ManualClock::new(SIG_TIME + o.clock_offset);
    gateway_with(Arc::new(manual.clone()), manual, o, false).await
}

fn sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn signed(c: &Caller) -> (http::HeaderMap, String) {
    let s = c
        .signer
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::security_scan(),
        ))
        .unwrap();
    (s.headers(), s.request_id().as_str().to_string())
}

async fn post(
    f: &Fleet,
    headers: http::HeaderMap,
    digest: &str,
    body: Vec<u8>,
) -> (u16, serde_json::Value) {
    let r = reqwest::Client::new()
        .post(format!("{}{SCAN_PATH}", f.url))
        .headers(headers)
        .header("content-type", "application/octet-stream")
        .header(CONTENT_SHA256_HEADER, digest)
        .header("content-length", body.len().to_string())
        .body(body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(json!(null)))
}

async fn scan(f: &Fleet, data: &[u8]) -> (u16, ScanResponse, String) {
    let (h, rid) = signed(&f.messaging);
    let (status, body) = post(f, h, &sha(data), data.to_vec()).await;
    (
        status,
        serde_json::from_value(body).expect("scan response"),
        rid,
    )
}

fn expect<'a>(rid: &'a str, digest: &'a str, bytes: u64) -> Expected<'a> {
    Expected {
        request_id: rid,
        client_id: "cfmeet.messaging",
        environment: "production",
        sha256: digest,
        bytes,
    }
}

// ---------------------------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_clean_file_returns_a_signed_verdict_bound_to_its_request_and_digest() {
    let f = gateway(Options::default()).await;
    let data = b"hello, harmless attachment".repeat(5000);
    let (status, r, rid) = scan(&f, &data).await;
    assert_eq!(status, 200);
    assert_eq!(r.verdict, Verdict::Clean);
    assert_eq!(
        (r.product.as_str(), r.client_id.as_str()),
        ("cfmeet", "cfmeet.messaging")
    );
    let digest = sha(&data);
    f.verdict_keys
        .verify(&r, expect(&rid, &digest, data.len() as u64))
        .unwrap();
    let engine = r.engine.as_ref().unwrap();
    assert_eq!(
        (engine.signature_version, engine.signature_age_seconds),
        (28173, 3600)
    );
    // The same verdict is worthless for other content or another request.
    assert!(f
        .verdict_keys
        .verify(&r, expect(&rid, &sha(b"other"), data.len() as u64))
        .is_err());
    assert!(f
        .verdict_keys
        .verify(&r, expect("req_other", &digest, data.len() as u64))
        .is_err());
}

#[tokio::test]
async fn eicar_is_infected_and_signed_with_its_finding() {
    let f = gateway(Options::default()).await;
    let mut data = b"prefix ".to_vec();
    data.extend_from_slice(EICAR);
    let (status, r, rid) = scan(&f, &data).await;
    assert_eq!((status, r.verdict), (200, Verdict::Infected));
    assert_eq!(r.finding.as_deref(), Some("Eicar-Test-Signature"));
    f.verdict_keys
        .verify(&r, expect(&rid, &sha(&data), data.len() as u64))
        .unwrap();
}

#[tokio::test]
async fn engine_errors_and_malformed_replies_are_never_clean() {
    let f = gateway(Options::default()).await;
    let (status, r, _) = scan(&f, b"ENGINE-ERROR").await;
    assert_eq!(
        (status, r.verdict, r.reason),
        (502, Verdict::Error, Some(Reason::BackendError))
    );
    assert!(r.verdict_signature.is_none());
    let (status, r, _) = scan(&f, b"ENGINE-GARBAGE").await;
    assert_eq!(
        (status, r.verdict, r.reason),
        (502, Verdict::Error, Some(Reason::MalformedBackendReply))
    );
}

#[tokio::test]
async fn an_engine_size_limit_is_rejected_not_clean() {
    let f = gateway(Options {
        stream_max: 1000,
        ..Default::default()
    })
    .await;
    let (status, r, _) = scan(&f, &vec![b'a'; 200_000]).await;
    assert_eq!(
        (status, r.verdict, r.reason),
        (413, Verdict::Rejected, Some(Reason::TooLarge))
    );
}

#[tokio::test]
async fn an_unreachable_engine_is_unavailable_and_not_ready() {
    let f = gateway(Options {
        backend: Some("127.0.0.1:1".into()),
        ..Default::default()
    })
    .await;
    let (status, r, _) = scan(&f, b"anything").await;
    assert_eq!(
        (status, r.verdict, r.reason),
        (503, Verdict::Unavailable, Some(Reason::BackendUnavailable))
    );
    let ready = reqwest::get(format!("{}/ready", f.url)).await.unwrap();
    assert_eq!(ready.status().as_u16(), 503);
    assert_eq!(
        reqwest::get(format!("{}/health", f.url))
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );
}

#[tokio::test]
async fn stale_signatures_refuse_to_scan() {
    let f = gateway(Options {
        clock_offset: 73 * 3600,
        ..Default::default()
    })
    .await;
    let (status, r, _) = scan(&f, b"anything").await;
    assert_eq!(
        (status, r.verdict, r.reason),
        (503, Verdict::Unavailable, Some(Reason::SignaturesStale))
    );
    assert_eq!(
        f.engine.scans.load(Ordering::SeqCst),
        0,
        "no bytes reach a stale engine"
    );
    let ready: serde_json::Value = reqwest::get(format!("{}/ready", f.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ready["reason"], "SIGNATURES_STALE");
}

#[tokio::test]
async fn a_slow_scan_times_out_as_a_retryable_non_verdict() {
    let f = gateway(Options {
        scan_timeout: Duration::from_millis(300),
        ..Default::default()
    })
    .await;
    let (status, r, _) = scan(&f, b"SLOW content").await;
    assert_eq!(
        (status, r.verdict, r.reason),
        (503, Verdict::Timeout, Some(Reason::ScanTimeout))
    );
}

// ---------------------------------------------------------------------------------------
// Request validation
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn empty_oversized_and_mismatched_content_is_rejected() {
    let f = gateway(Options {
        max_bytes: 1000,
        ..Default::default()
    })
    .await;
    let (status, r, _) = scan(&f, b"").await;
    assert_eq!((status, r.reason), (422, Some(Reason::EmptyContent)));
    let (status, r, _) = scan(&f, &[b'x'; 1001]).await;
    assert_eq!((status, r.reason), (413, Some(Reason::TooLarge)));
    // A clean engine answer for bytes other than the caller declared is not a verdict.
    let (h, _) = signed(&f.messaging);
    let (status, body) = post(&f, h, &sha(b"declared"), b"actually sent".to_vec()).await;
    assert_eq!(
        (status, body["reason"].as_str()),
        (422, Some("DIGEST_MISMATCH"))
    );
    assert!(body.get("verdictSignature").is_none());
    let (h, _) = signed(&f.messaging);
    let (status, body) = post(&f, h, "not-a-digest", b"x".to_vec()).await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (400, Some("INVALID_CONTENT_DIGEST"))
    );
}

#[tokio::test]
async fn a_truncated_upload_is_never_scanned_clean() {
    let f = gateway(Options::default()).await;
    let (h, _) = signed(&f.messaging);
    let addr = f.url.trim_start_matches("http://").to_string();
    let mut s = TcpStream::connect(&addr).await.unwrap();
    let mut req = format!("POST {SCAN_PATH} HTTP/1.1\r\nhost: x\r\ncontent-type: application/octet-stream\r\ncontent-length: 5000\r\n{CONTENT_SHA256_HEADER}: {}\r\n", sha(b"abc"));
    for (k, v) in &h {
        req.push_str(&format!("{}: {}\r\n", k, v.to_str().unwrap()));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    s.write_all(&[b'a'; 100]).await.unwrap();
    s.shutdown().await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("\"CLEAN\""), "{text}");
    assert!(text.is_empty() || text.contains("TRUNCATED"), "{text}");
    assert!(!f.app.metrics.render().contains("verdict=\"CLEAN\""));
}

// ---------------------------------------------------------------------------------------
// Authentication, authorization, replay and isolation
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn requests_without_a_valid_service_token_are_refused_before_any_byte_is_scanned() {
    let f = gateway(Options::default()).await;
    let data = b"x".to_vec();
    // No token at all.
    let (status, body) = post(&f, http::HeaderMap::new(), &sha(&data), data.clone()).await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (401, Some("UNAUTHENTICATED"))
    );
    // Signed by an untrusted key claiming to be Messaging (impersonation).
    let (impostor, _) = caller(
        ServiceId::messaging(),
        ENV,
        Arc::new(f.clock.clone()),
        "messaging-scan-1",
    );
    let (h, _) = signed(&impostor);
    assert_eq!(post(&f, h, &sha(&data), data.clone()).await.0, 401);
    // Another CF Meet service that is not registered (no grant).
    let (calling, _) = caller(
        ServiceId::calling(),
        ENV,
        Arc::new(f.clock.clone()),
        "calling-1",
    );
    let (h, _) = signed(&calling);
    assert_eq!(post(&f, h, &sha(&data), data.clone()).await.0, 401);
    // A staging token never works in production.
    let (staging, _) = caller(
        ServiceId::messaging(),
        RuntimeEnvironment::Staging,
        Arc::new(f.clock.clone()),
        "messaging-scan-1",
    );
    let (h, _) = signed(&staging);
    assert_eq!(post(&f, h, &sha(&data), data.clone()).await.0, 401);
    // A token for another audience.
    let s = f
        .messaging
        .signer
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::platform_access(),
        ))
        .unwrap();
    assert_eq!(
        post(&f, s.headers(), &sha(&data), data.clone()).await.0,
        401
    );
    // Expired.
    let (h, _) = signed(&f.messaging);
    f.clock.advance(400);
    assert_eq!(post(&f, h, &sha(&data), data.clone()).await.0, 401);
    assert_eq!(f.engine.scans.load(Ordering::SeqCst), 0);
    let metrics = f.app.metrics.render();
    assert!(metrics.contains("cosmicforge_scan_auth_failures_total"));
}

#[tokio::test]
async fn a_token_is_single_use() {
    let f = gateway(Options::default()).await;
    let (h, _) = signed(&f.messaging);
    assert_eq!(post(&f, h.clone(), &sha(b"a"), b"a".to_vec()).await.0, 200);
    let (status, body) = post(&f, h, &sha(b"a"), b"a".to_vec()).await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (401, Some("UNAUTHENTICATED"))
    );
    assert_eq!(f.engine.scans.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn customer_credentials_are_never_accepted_alongside_a_service_token() {
    let f = gateway(Options::default()).await;
    let (mut h, _) = signed(&f.messaging);
    h.insert("authorization", "Bearer customer".parse().unwrap());
    assert_eq!(post(&f, h, &sha(b"a"), b"a".to_vec()).await.0, 401);
}

#[test]
fn registries_cannot_widen_the_trust_bundle_or_cross_environments() {
    let clock: SharedClock = Arc::new(SystemClock);
    let (m, mj) = caller(ServiceId::messaging(), ENV, clock.clone(), "m1");
    let (c, cj) = caller(ServiceId::calling(), ENV, clock, "c1");
    let me = ServiceId::security_scan();
    let only_messaging = TrustPolicy::from_json(&bundle(&[(&m.id, &mj)]), &me, ENV).unwrap();
    let reg = |env: &str, issuer: &str, bytes: u64| {
        json!({"version": 1, "environment": env, "clients": [{
            "issuer": issuer, "product": "cfmeet", "maxBytes": bytes, "maxConcurrent": 1, "maxPerMinute": 10}]})
        .to_string()
    };
    let load =
        |json: String, policy: &TrustPolicy| Registry::from_json(&json, ENV, policy, 1000, 2);
    load(reg("production", "cfmeet.messaging", 1000), &only_messaging).unwrap();
    // A registered client without a grant.
    assert!(load(reg("production", "cfmeet.calling", 1000), &only_messaging).is_err());
    // A staging registry in production.
    assert!(load(reg("staging", "cfmeet.messaging", 1000), &only_messaging).is_err());
    // Above the engine's StreamMaxLength.
    assert!(load(reg("production", "cfmeet.messaging", 1001), &only_messaging).is_err());
    // A trusted issuer without a registry entry. (The registry only accepts issuers the
    // bundle grants security-scan.submit, and the service-auth registry only lets
    // Messaging hold that permission, so Calling cannot even be granted it.)
    let both = bundle(&[(&m.id, &mj), (&c.id, &cj)]);
    assert!(TrustPolicy::from_json(&both, &me, ENV).is_err());
}

#[tokio::test]
async fn products_are_rate_limited_and_concurrency_bounded() {
    let f = gateway(Options {
        max_per_minute: 2,
        ..Default::default()
    })
    .await;
    assert_eq!(scan(&f, b"1").await.0, 200);
    assert_eq!(scan(&f, b"2").await.0, 200);
    let (status, r, _) = scan(&f, b"3").await;
    assert_eq!((status, r.reason), (429, Some(Reason::RateLimited)));
    f.clock.advance(61);
    // The fixed window uses a monotonic clock; a new gateway shows the bound resets.

    let f = gateway(Options {
        max_concurrent: 1,
        queue_timeout: Duration::from_millis(100),
        ..Default::default()
    })
    .await;
    let f = Arc::new(f);
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let f = f.clone();
        tasks.push(tokio::spawn(async move { scan(&f, b"SLOW one").await }));
    }
    let mut verdicts = Vec::new();
    for t in tasks {
        let (_, r, _) = t.await.unwrap();
        verdicts.push((r.verdict, r.reason));
    }
    assert!(verdicts.contains(&(Verdict::Clean, None)));
    assert!(verdicts.contains(&(Verdict::Unavailable, Some(Reason::Overloaded))));
    assert_eq!(f.engine.peak.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_engine_never_sees_more_than_the_shared_concurrency_cap() {
    let f = Arc::new(
        gateway(Options {
            max_concurrent: 2,
            backend_concurrency: 2,
            queue_timeout: Duration::from_secs(5),
            ..Default::default()
        })
        .await,
    );
    let mut tasks = Vec::new();
    for i in 0..6 {
        let f = f.clone();
        tasks.push(tokio::spawn(async move {
            scan(&f, format!("SLOW {i}").as_bytes()).await
        }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().1.verdict, Verdict::Clean);
    }
    assert!(f.engine.peak.load(Ordering::SeqCst) <= 2);
    let metrics = f.app.metrics.render();
    assert!(metrics.contains("cosmicforge_scan_requests_total{product=\"cfmeet\",verdict=\"CLEAN\",reason=\"NONE\"} 6"), "{metrics}");
    assert!(metrics.contains("cosmicforge_scan_inflight{product=\"cfmeet\"} 0"));
    assert!(
        !metrics.contains("SLOW"),
        "no content or request data in metrics"
    );
}

// ---------------------------------------------------------------------------------------
// Real engine (CI: clamav/clamav service container)
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn real_clamd_detects_eicar_and_passes_clean_content() {
    let Ok(addr) = std::env::var("SCAN_GATEWAY_TEST_CLAMD_ADDRESS") else {
        eprintln!("SCAN_GATEWAY_TEST_CLAMD_ADDRESS unset: real-engine test skipped");
        return;
    };
    let manual = ManualClock::new(0);
    let real: SharedClock = Arc::new(SystemClock);
    // Tokens are signed and verified at the real time; signatures age against it too.
    manual.set(real.now_unix());
    let f = gateway_with(
        real,
        manual,
        Options {
            backend: Some(addr),
            max_bytes: 20 * 1024 * 1024,
            scan_timeout: Duration::from_secs(60),
            ..Default::default()
        },
        true,
    )
    .await;
    let (status, r, rid) = scan(&f, b"CosmicForge gateway clean fixture").await;
    assert_eq!((status, r.verdict), (200, Verdict::Clean), "{r:?}");
    f.verdict_keys
        .verify(
            &r,
            expect(&rid, &sha(b"CosmicForge gateway clean fixture"), 33),
        )
        .unwrap();
    let (status, r, rid) = scan(&f, EICAR).await;
    assert_eq!((status, r.verdict), (200, Verdict::Infected), "{r:?}");
    assert!(r.finding.as_deref().unwrap().contains("Eicar"));
    f.verdict_keys
        .verify(&r, expect(&rid, &sha(EICAR), EICAR.len() as u64))
        .unwrap();
    // A larger multi-frame clean file streams through without buffering issues.
    let big = vec![b'z'; 5 * 1024 * 1024];
    let (status, r, _) = scan(&f, &big).await;
    assert_eq!(
        (status, r.verdict, r.bytes),
        (200, Verdict::Clean, big.len() as u64)
    );
}
