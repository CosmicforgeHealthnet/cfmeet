//! HQ simulator (design §16): real TLS 1.3 mutual TLS against the real listener, real
//! HMAC v2 signatures and Ed25519 staff assertions, a test CA from rcgen and the
//! in-memory store. Covers the A1 acceptance: HQ reads; forged, replayed, expired,
//! out-of-scope, unenrolled and unaudited requests are refused.

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use admin_bridge::{
    chain::Bridge,
    config::{SecurityConfig, Switches},
    contract::{catalog::catalog, model::*},
    policy,
    routes::HealthProbe,
    server,
    store::{DatasetDay, MemoryStore, PolicyRow, Principal, PrincipalKind, Store},
    wire,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::{BodyExt, Full};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use serde_json::{json, Value};
use tokio::net::TcpStream;

const WORKLOAD: &str = "cf://staging/product-admin/cfmeet";
const HMAC_KID: &str = "hq-hmac-1";
const HMAC_KEY: &[u8] = b"test-hmac-key-material-at-least-32-bytes-long";
const STAFF_KID: &str = "hq-staff-1";
const TENANT: &str = "cfmeet-first-party";

struct Pki {
    dir: tempfile::TempDir,
    ca_pem: String,
    client_cert_der: Vec<u8>,
    client_cert_pem: String,
    client_key_pem: String,
    other_cert_pem: String,
    other_key_pem: String,
}

fn pki() -> Pki {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "test HQ CA");
    let ca = ca_params.self_signed(&ca_key).unwrap();

    let server_key = KeyPair::generate().unwrap();
    let mut sp = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    sp.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_cert = sp.signed_by(&server_key, &ca, &ca_key).unwrap();

    let client = |uri: &str| {
        let key = KeyPair::generate().unwrap();
        let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
        p.subject_alt_names = vec![SanType::URI(uri.try_into().unwrap())];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let cert = p.signed_by(&key, &ca, &ca_key).unwrap();
        (cert, key)
    };
    let (client_cert, client_key) = client(WORKLOAD);
    let (other_cert, other_key) = client(WORKLOAD); // same CA and URI, never enrolled

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("server.pem"), server_cert.pem()).unwrap();
    std::fs::write(dir.path().join("server.key"), server_key.serialize_pem()).unwrap();
    std::fs::write(dir.path().join("ca.pem"), ca.pem()).unwrap();
    std::fs::create_dir(dir.path().join("hmac")).unwrap();
    std::fs::write(
        dir.path().join("hmac").join(format!("{HMAC_KID}.key")),
        HMAC_KEY,
    )
    .unwrap();
    Pki {
        ca_pem: ca.pem(),
        client_cert_der: client_cert.der().to_vec(),
        client_cert_pem: client_cert.pem(),
        client_key_pem: client_key.serialize_pem(),
        other_cert_pem: other_cert.pem(),
        other_key_pem: other_key.serialize_pem(),
        dir,
    }
}

fn staff_key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

fn policy_key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

struct Harness {
    pki: Pki,
    store: Arc<MemoryStore>,
    addr: std::net::SocketAddr,
}

struct Options {
    enabled: bool,
    policy: bool,
    catalog: Option<Vec<Capability>>,
    switches_all_on: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            enabled: true,
            policy: true,
            catalog: None,
            switches_all_on: false,
        }
    }
}

async fn start(opts: Options) -> Harness {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pki = pki();
    let now = Utc::now();
    let store = Arc::new(MemoryStore::new());
    let principal =
        |kind, key_id: &str, material: String, issuer: Option<&str>, audience: Option<&str>| {
            Principal {
                principal: HQ_PRINCIPAL.into(),
                kind,
                key_id: key_id.into(),
                material,
                issuer: issuer.map(Into::into),
                audience: audience.map(Into::into),
                not_before: now - Duration::hours(1),
                expires_at: now + Duration::days(30),
                revoked_at: None,
            }
        };
    store.enroll(principal(
        PrincipalKind::ClientCert,
        &wire::sha256_hex(&pki.client_cert_der),
        wire::sha256_hex(&pki.client_cert_der),
        None,
        None,
    ));
    store.enroll(principal(
        PrincipalKind::HmacKey,
        HMAC_KID,
        wire::sha256_hex(HMAC_KEY),
        None,
        None,
    ));
    store.enroll(principal(
        PrincipalKind::StaffKey,
        STAFF_KID,
        hex::encode(staff_key().verifying_key().to_bytes()),
        Some("cosmicforge-hq"),
        Some("cfmeet-admin-bridge"),
    ));
    if opts.policy {
        let doc = serde_json::to_vec(&policy::ceiling_document(1, "2026-11-19T00:00:00Z")).unwrap();
        store.put_policy(PolicyRow {
            revision: 1,
            signature: policy::sign(&policy_key(), &doc),
            document: doc,
            key_id: "policy-1".into(),
        });
    }
    let security = SecurityConfig {
        hq_workload_uri: WORKLOAD.into(),
        hmac_key_dir: pki.dir.path().join("hmac"),
        policy_key: policy_key().verifying_key(),
        policy_key_id: "policy-1".into(),
        policy_revision: 1,
        strong_acr: HashSet::from(["urn:cosmicforge:acr:mfa".to_string()]),
        workload_acr: "urn:cosmicforge:acr:workload".into(),
        enrolled_markets: HashSet::from(["GLOBAL".to_string()]),
        enrolled_tenants: HashSet::from([TENANT.to_string()]),
        rate_limit_per_minute: 5,
    };
    let switches = Switches {
        enabled: opts.enabled,
        writes_enabled: opts.switches_all_on,
        modules: if opts.switches_all_on {
            Module::ALL.iter().copied().collect()
        } else {
            Default::default()
        },
    };
    let mut bridge = Bridge::new(
        switches,
        Some(security),
        store.clone() as Arc<dyn Store>,
        Arc::new(Utc::now),
        HealthProbe::new(Default::default()),
    );
    if let Some(c) = opts.catalog {
        bridge = bridge.with_catalog(c);
    }
    let bridge = Arc::new(bridge);
    if opts.policy {
        bridge.load_policy().await.unwrap();
    }
    let acceptor = server::tls_acceptor(
        &pki.dir.path().join("server.pem"),
        &pki.dir.path().join("server.key"),
        &pki.dir.path().join("ca.pem"),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server::run_hq(bridge, listener, acceptor));
    Harness { pki, store, addr }
}

// ---- HQ client side (mirrors karryam-admin signing.js with cfmeet values) ---------

#[derive(Clone)]
struct Call {
    method: &'static str,
    uri: String,
    body: Vec<u8>,
    sub: String,
    session: String,
    roles: Vec<&'static str>,
    permissions: Vec<String>,
    acr: &'static str,
    tenant_scopes: Vec<&'static str>,
    lifetime: i64,
    iat_offset: i64,
    auth_age: i64,
    hmac_ts_offset: i64,
    jti: String,
    nonce: String,
    approval: Option<Value>,
    digest_uri: Option<String>,
    hmac_key: Vec<u8>,
    idempotency_key: Option<String>,
}

fn scoped(purpose: &str) -> String {
    format!("market=GLOBAL&organization=cfmeet&zone=production&tenant={TENANT}&purpose={purpose}")
}

fn call(path: &str, query: &str, permission: &str, roles: &[&'static str]) -> Call {
    Call {
        method: "GET",
        uri: format!("{BRIDGE_BASE}{path}?{query}"),
        body: Vec::new(),
        sub: "staff-0001".into(),
        session: "sess-0001".into(),
        roles: roles.to_vec(),
        permissions: vec![permission.to_string()],
        acr: "urn:cosmicforge:acr:mfa",
        tenant_scopes: vec![TENANT],
        lifetime: 20,
        iat_offset: 0,
        auth_age: 30,
        hmac_ts_offset: 0,
        jti: uuid::Uuid::new_v4().to_string(),
        nonce: hex::encode(rand_bytes()),
        approval: None,
        digest_uri: None,
        hmac_key: HMAC_KEY.to_vec(),
        idempotency_key: None,
    }
}

fn rand_bytes() -> [u8; 16] {
    *uuid::Uuid::new_v4().as_bytes()
}

fn discovery_call() -> Call {
    call(
        "/capabilities",
        &scoped("admin-ui-capabilities"),
        "bridge.capabilities.read",
        &["cfmeet.viewer"],
    )
}

fn window(days: i64) -> String {
    let end = (Utc::now() - Duration::minutes(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let start = (Utc::now() - Duration::days(days))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    format!("&limit=31&start={start}&end={end}")
}

fn headers(c: &Call) -> Vec<(String, String)> {
    let now = Utc::now().timestamp();
    let ts = now + c.hmac_ts_offset;
    let iat = now + c.iat_offset;
    let mut claims = json!({
        "iss": "cosmicforge-hq", "sub": c.sub, "aud": "cfmeet-admin-bridge", "product": "cfmeet",
        "roles": c.roles, "permissions": c.permissions,
        "market_scopes": ["GLOBAL"], "organization_scopes": ["cfmeet"], "zone_scopes": ["production"], "tenant_scopes": c.tenant_scopes,
        "session_id": c.session, "auth_time": now - c.auth_age, "acr": c.acr, "jti": c.jti,
        "iat": iat, "nbf": iat, "exp": iat + c.lifetime,
        "request_digest": wire::request_digest(c.method, c.digest_uri.as_deref().unwrap_or(&c.uri), &c.body),
    });
    if let Some(a) = &c.approval {
        claims["approval"] = a.clone();
    }
    let header = B64.encode(
        serde_json::to_vec(&json!({"alg": "EdDSA", "typ": "JWT", "kid": STAFF_KID})).unwrap(),
    );
    let payload = B64.encode(serde_json::to_vec(&claims).unwrap());
    let input = format!("{header}.{payload}");
    let jws = format!(
        "{input}.{}",
        B64.encode(staff_key().sign(input.as_bytes()).to_bytes())
    );
    let mut h = vec![
        ("x-hq-key-id".into(), HMAC_KID.into()),
        ("x-service-name".into(), HQ_PRINCIPAL.into()),
        ("x-service-timestamp".into(), ts.to_string()),
        ("x-service-nonce".into(), c.nonce.clone()),
        ("x-service-signature-version".into(), "2".into()),
        (
            "x-service-signature".into(),
            wire::hmac_v2_signature(&c.hmac_key, c.method, &c.uri, ts, &c.nonce, &c.body),
        ),
        ("authorization".into(), format!("Bearer {jws}")),
    ];
    if let Some(k) = &c.idempotency_key {
        h.push(("idempotency-key".into(), k.clone()));
    }
    h
}

fn client_config(
    pki: &Pki,
    cert_pem: Option<(&str, &str)>,
    tls12_only: bool,
) -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    for c in rustls_pemfile::certs(&mut pki.ca_pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let versions: &[&rustls::SupportedProtocolVersion] = if tls12_only {
        &[&rustls::version::TLS12]
    } else {
        &[&rustls::version::TLS13]
    };
    let b = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .unwrap()
        .with_root_certificates(roots);
    match cert_pem {
        Some((cert, key)) => {
            let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert.as_bytes())
                .map(|c| c.unwrap())
                .collect();
            let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key.as_bytes())
                .unwrap()
                .unwrap();
            b.with_client_auth_cert(certs, key).unwrap()
        }
        None => b.with_no_client_auth(),
    }
}

async fn send_with(
    h: &Harness,
    c: &Call,
    tls: rustls::ClientConfig,
    raw_headers: Option<Vec<(String, String)>>,
) -> Result<(u16, Value), String> {
    let tcp = TcpStream::connect(h.addr)
        .await
        .map_err(|e| e.to_string())?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
    let stream = connector
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .map_err(|e| format!("tls: {e}"))?;
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .map_err(|e| e.to_string())?;
    tokio::spawn(conn);
    let mut req = hyper::Request::builder()
        .method(c.method)
        .uri(&c.uri)
        .header("host", "localhost");
    for (k, v) in raw_headers.unwrap_or_else(|| headers(c)) {
        req = req.header(k, v);
    }
    let resp = sender
        .send_request(
            req.body(Full::new(bytes::Bytes::from(c.body.clone())))
                .unwrap(),
        )
        .await
        .map_err(|e| format!("http: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

async fn send(h: &Harness, c: &Call) -> (u16, Value) {
    let tls = client_config(
        &h.pki,
        Some((&h.pki.client_cert_pem, &h.pki.client_key_pem)),
        false,
    );
    send_with(h, c, tls, None).await.expect("request completes")
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("<none>")
}

fn assert_envelope(v: &Value, ok: bool) {
    assert_eq!(v["contract_version"], "1.0");
    assert_eq!(v["product"], "cfmeet");
    assert!(uuid::Uuid::parse_str(v["correlation_id"].as_str().unwrap()).is_ok());
    assert!(DateTime::parse_from_rfc3339(v["as_of"].as_str().unwrap()).is_ok());
    if ok {
        assert!(v["error"].is_null() && v.get("result").is_some());
    } else {
        assert!(
            v.get("result").is_none(),
            "an error answer carries no result"
        );
    }
}

// ---- Acceptance: reads -------------------------------------------------------------

#[tokio::test]
async fn discovery_reports_runtime_states_and_the_openapi_hash() {
    let h = start(Options::default()).await;
    let (status, v) = send(&h, &discovery_call()).await;
    assert_eq!(status, 200, "{v}");
    assert_envelope(&v, true);
    let r = &v["result"];
    assert_eq!(r["contract_version"], CONTRACT_VERSION);
    let committed =
        std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/contract/openapi.json"))
            .unwrap();
    assert_eq!(
        r["openapi_sha256"],
        wire::sha256_hex(&committed),
        "runtime hash must equal the committed openapi.json"
    );
    assert_eq!(r["policy_revision"], 1);
    let caps = r["capabilities"].as_array().unwrap();
    assert_eq!(caps.len(), catalog().len());
    let state = |id: &str| {
        caps.iter().find(|c| c["id"] == id).unwrap()["state"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(state("bridge.datasets.read"), "READ_ONLY");
    assert_eq!(state("accounts.users.search"), "BLOCKED_DEPENDENCY");
    assert_eq!(state("verification.partner-go-live.approve"), "DISABLED");
    // audited before the answer
    let audit = h.store.audit_entries();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].0.decision, "ALLOWED");
    assert_eq!(audit[0].0.actor, "hq:staff-0001");
    assert_eq!(
        audit[0].0.capability.as_deref(),
        Some("bridge.capabilities.read")
    );
}

#[tokio::test]
async fn datasets_are_null_not_zero_until_sources_report() {
    let h = start(Options::default()).await;
    let c = call(
        "/datasets/cfmeet.engagement_daily",
        &format!("{}{}", scoped("metrics-read"), window(7)),
        "bridge.datasets.read",
        &["cfmeet.viewer"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"];
    assert_eq!(r["coverage"], "NONE");
    assert_eq!(v["freshness"], "UNAVAILABLE");
    let series = r["series"].as_array().unwrap();
    assert!(series.len() >= 7);
    for day in series {
        for m in ["dau", "mau", "signups"] {
            assert!(day["metrics"][m].is_null(), "{m} must be null, never zero");
        }
    }
    // one day fully reported -> PARTIAL with that day's values
    let today = (Utc::now() - Duration::minutes(1)).date_naive();
    let mut metrics = serde_json::Map::new();
    metrics.insert("dau".into(), json!(12));
    metrics.insert("mau".into(), json!(40));
    metrics.insert("signups".into(), json!(0));
    h.store.put_dataset_day(DatasetDay {
        dataset_id: "cfmeet.engagement_daily".into(),
        day: today,
        metrics,
        sources_reported: vec!["meeting-service:PULL".into()],
        updated_at: Utc::now(),
    });
    let mut c2 = c.clone();
    c2.jti = uuid::Uuid::new_v4().to_string();
    c2.nonce = hex::encode(rand_bytes());
    let (_, v) = send(&h, &c2).await;
    assert_eq!(v["result"]["coverage"], "PARTIAL");
    let last = v["result"]["series"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["metrics"]["dau"], 12);
    assert_eq!(last["metrics"]["signups"], 0, "a reported zero stays zero");
}

#[tokio::test]
async fn worker_identity_reads_datasets_with_the_workload_acr() {
    let h = start(Options::default()).await;
    let mut c = call(
        "/datasets/cfmeet.meetings_daily",
        &format!("{}{}", scoped("metrics-read"), window(31)),
        "bridge.datasets.read",
        &["cfmeet.source_sync"],
    );
    c.acr = "urn:cosmicforge:acr:workload";
    assert_eq!(send(&h, &c).await.0, 200);
    // the workload acr never belongs to a staff role ...
    let mut staff = discovery_call();
    staff.acr = "urn:cosmicforge:acr:workload";
    assert_eq!(code(&send(&h, &staff).await.1), "STAFF_ASSERTION_INVALID");
    // ... and the worker never uses the staff acr
    let mut w = c.clone();
    w.acr = "urn:cosmicforge:acr:mfa";
    w.jti = uuid::Uuid::new_v4().to_string();
    w.nonce = hex::encode(rand_bytes());
    assert_eq!(code(&send(&h, &w).await.1), "STAFF_ASSERTION_INVALID");
    // the worker cannot read discovery
    let mut d = call(
        "/capabilities",
        &scoped("admin-ui-capabilities"),
        "bridge.capabilities.read",
        &["cfmeet.source_sync"],
    );
    d.acr = "urn:cosmicforge:acr:workload";
    assert_eq!(code(&send(&h, &d).await.1), "PERMISSION_DENIED");
}

#[tokio::test]
async fn health_reports_unconfigured_owners_honestly() {
    let h = start(Options::default()).await;
    let c = call(
        "/health/aggregate",
        &scoped("metrics-read"),
        "bridge.health.read",
        &["cfmeet.viewer"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"];
    assert!(DateTime::parse_from_rfc3339(r["as_of"].as_str().unwrap()).is_ok());
    // No owner probe is configured: the bridge vouches only for itself.
    assert_eq!(r["status"], "UNKNOWN");
    let services = r["services"].as_array().unwrap();
    assert_eq!(
        services.len(),
        1,
        "owners without a probe are omitted, never OK"
    );
    assert_eq!(services[0]["service"], "admin-bridge");
    assert_eq!(services[0]["status"], "OK");
    assert!(services[0]["checked_at"].is_string());
}

#[tokio::test]
async fn projection_status_is_unavailable_until_owner_events() {
    let h = start(Options::default()).await;
    let c = call(
        "/hq/projections/status",
        &scoped("ops-incident"),
        "hq.projections.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let list = v["result"]["projections"].as_array().unwrap();
    assert_eq!(list.len(), 9);
    assert!(list
        .iter()
        .all(|p| p["freshness"] == "UNAVAILABLE" && p["open_gaps"].is_null()));
}

// ---- Acceptance: refusals ----------------------------------------------------------

#[tokio::test]
async fn forged_signature_is_refused_and_audited() {
    let h = start(Options::default()).await;
    let mut c = discovery_call();
    c.hmac_key = b"not-the-enrolled-key-but-long-enough-0000".to_vec();
    let (status, v) = send(&h, &c).await;
    assert_eq!((status, code(&v)), (401, "SIGNATURE_INVALID"));
    assert_envelope(&v, false);
    assert_eq!(
        h.store.audit_entries().last().unwrap().0.decision,
        "SIGNATURE_INVALID"
    );
}

#[tokio::test]
async fn tampered_query_breaks_the_signature() {
    let h = start(Options::default()).await;
    let c = discovery_call();
    let mut hs = headers(&c);
    let mut tampered = c.clone();
    tampered.uri = c.uri.replace("GLOBAL", "NG");
    hs.retain(|_| true);
    let tls = client_config(
        &h.pki,
        Some((&h.pki.client_cert_pem, &h.pki.client_key_pem)),
        false,
    );
    let (status, v) = send_with(&h, &tampered, tls, Some(hs)).await.unwrap();
    assert_eq!((status, code(&v)), (401, "SIGNATURE_INVALID"));
}

#[tokio::test]
async fn replayed_request_is_refused() {
    let h = start(Options::default()).await;
    let c = discovery_call();
    let hs = headers(&c);
    let tls = || {
        client_config(
            &h.pki,
            Some((&h.pki.client_cert_pem, &h.pki.client_key_pem)),
            false,
        )
    };
    assert_eq!(
        send_with(&h, &c, tls(), Some(hs.clone())).await.unwrap().0,
        200
    );
    let (status, v) = send_with(&h, &c, tls(), Some(hs)).await.unwrap();
    assert_eq!((status, code(&v)), (409, "REPLAY"));
    // a new nonce with the same jti is a replay too
    let mut same_jti = c.clone();
    same_jti.nonce = hex::encode(rand_bytes());
    assert_eq!(code(&send(&h, &same_jti).await.1), "REPLAY");
}

#[tokio::test]
async fn expired_or_long_lived_assertions_are_refused() {
    let h = start(Options::default()).await;
    let mut expired = discovery_call();
    expired.iat_offset = -120;
    assert_eq!(code(&send(&h, &expired).await.1), "STAFF_ASSERTION_EXPIRED");
    let mut long = discovery_call();
    long.lifetime = 61;
    assert_eq!(code(&send(&h, &long).await.1), "STAFF_ASSERTION_INVALID");
    let mut skew = discovery_call();
    skew.hmac_ts_offset = -31;
    assert_eq!(code(&send(&h, &skew).await.1), "TIMESTAMP_SKEW");
}

#[tokio::test]
async fn digest_mismatch_is_refused() {
    let h = start(Options::default()).await;
    let mut c = discovery_call();
    c.digest_uri = Some(c.uri.replace("admin-ui-capabilities", "metrics-read"));
    assert_eq!(code(&send(&h, &c).await.1), "REQUEST_DIGEST_MISMATCH");
}

#[tokio::test]
async fn scopes_outside_the_assertion_or_the_enrolment_are_refused() {
    let h = start(Options::default()).await;
    // tenant in the request but not in the assertion
    let mut c = discovery_call();
    c.tenant_scopes = vec!["partner-tenant"];
    assert_eq!(code(&send(&h, &c).await.1), "SCOPE_DENIED");
    // tenant in the assertion but not enrolled for HQ
    let mut c2 = call(
        "/capabilities",
        "market=GLOBAL&organization=cfmeet&zone=production&tenant=partner-tenant&purpose=admin-ui-capabilities",
        "bridge.capabilities.read",
        &["cfmeet.viewer"],
    );
    c2.tenant_scopes = vec!["partner-tenant"];
    assert_eq!(code(&send(&h, &c2).await.1), "SCOPE_DENIED");
}

#[tokio::test]
async fn permission_role_and_purpose_must_all_be_granted() {
    let h = start(Options::default()).await;
    let mut no_perm = discovery_call();
    no_perm.permissions = vec!["bridge.health.read".into()];
    assert_eq!(code(&send(&h, &no_perm).await.1), "PERMISSION_DENIED");
    // finance may not read health at all
    let c = call(
        "/health/aggregate",
        &scoped("metrics-read"),
        "bridge.health.read",
        &["cfmeet.finance"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "PERMISSION_DENIED");
    // a purpose the capability does not list
    let c = call(
        "/capabilities",
        &scoped("metrics-read"),
        "bridge.capabilities.read",
        &["cfmeet.viewer"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "PURPOSE_NOT_ALLOWED");
    // unknown role strings are refused outright
    let c = call(
        "/capabilities",
        &scoped("admin-ui-capabilities"),
        "bridge.capabilities.read",
        &["cfmeet.root"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "STAFF_ASSERTION_INVALID");
}

#[tokio::test]
async fn revoked_sessions_and_staff_are_refused() {
    let h = start(Options::default()).await;
    h.store
        .revoke_session("sess-0001", Utc::now() - Duration::seconds(1));
    assert_eq!(code(&send(&h, &discovery_call()).await.1), "REVOKED");
    let mut other = discovery_call();
    other.session = "sess-0002".into();
    assert_eq!(send(&h, &other).await.0, 200);
    h.store
        .revoke_sub("staff-0001", Utc::now() - Duration::seconds(1));
    let mut again = discovery_call();
    again.session = "sess-0003".into();
    assert_eq!(code(&send(&h, &again).await.1), "REVOKED");
}

#[tokio::test]
async fn audit_failure_answers_503_with_no_data() {
    let h = start(Options::default()).await;
    h.store.fail_audit(true);
    let (status, v) = send(&h, &discovery_call()).await;
    assert_eq!((status, code(&v)), (503, "AUDIT_UNAVAILABLE"));
    assert_envelope(&v, false);
}

#[tokio::test]
async fn client_certificates_must_be_present_tls13_and_enrolled() {
    let h = start(Options::default()).await;
    // no client certificate: the handshake (or first read) fails
    let none = client_config(&h.pki, None, false);
    assert!(send_with(&h, &discovery_call(), none, None).await.is_err());
    // TLS 1.2 is refused
    let tls12 = client_config(
        &h.pki,
        Some((&h.pki.client_cert_pem, &h.pki.client_key_pem)),
        true,
    );
    assert!(send_with(&h, &discovery_call(), tls12, None).await.is_err());
    // same CA and URI SAN, but not enrolled
    let other = client_config(
        &h.pki,
        Some((&h.pki.other_cert_pem, &h.pki.other_key_pem)),
        false,
    );
    let (status, v) = send_with(&h, &discovery_call(), other, None).await.unwrap();
    assert_eq!((status, code(&v)), (401, "CLIENT_CERTIFICATE_REFUSED"));
    // unauthenticated refusals are not audited
    assert!(h.store.audit_entries().is_empty());
}

#[tokio::test]
async fn unknown_routes_404_and_unserved_capabilities_503() {
    let h = start(Options::default()).await;
    let c = call(
        "/nope",
        &scoped("support-review"),
        "x.read",
        &["cfmeet.support"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "CAPABILITY_NOT_FOUND");
    let c = call(
        "/accounts/users",
        &format!("{}&q=ada", scoped("support-review")),
        "accounts.users.read",
        &["cfmeet.support"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "CAPABILITY_DISABLED");
}

#[tokio::test]
async fn disabled_bridge_and_missing_policy_refuse_everything() {
    let h = start(Options {
        enabled: false,
        ..Default::default()
    })
    .await;
    assert_eq!(
        code(&send(&h, &discovery_call()).await.1),
        "BRIDGE_DISABLED"
    );
    let h = start(Options {
        policy: false,
        ..Default::default()
    })
    .await;
    assert_eq!(
        code(&send(&h, &discovery_call()).await.1),
        "POLICY_UNAVAILABLE"
    );
}

#[tokio::test]
async fn rate_limit_applies_per_staff_permission_and_scope() {
    let h = start(Options::default()).await;
    for _ in 0..5 {
        assert_eq!(send(&h, &discovery_call()).await.0, 200);
    }
    assert_eq!(code(&send(&h, &discovery_call()).await.1), "RATE_LIMITED");
    let mut other = discovery_call();
    other.sub = "staff-0002".into();
    assert_eq!(send(&h, &other).await.0, 200);
}

// ---- Approval and MFA paths (on a test catalog where a command is served) ----------

fn served_command_catalog() -> Vec<Capability> {
    catalog()
        .into_iter()
        .map(|mut c| {
            if c.id == "verification.partner-go-live.approve"
                || c.id == "partners.webhook-delivery.retry"
            {
                c.served = true;
            }
            c
        })
        .collect()
}

const APPROVE: &str = "verification.partner-go-live.approve";

fn command(path: &str, permission: &str, purpose: &str, role: &'static str) -> Call {
    let mut c = call(path, &scoped(purpose), permission, &[role]);
    c.method = "POST";
    c.body = br#"{"expected_version":3}"#.to_vec();
    c.idempotency_key = Some(uuid::Uuid::new_v4().to_string());
    c
}

fn approval(
    capability: &str,
    requester: &str,
    approver: &str,
    decided: DateTime<Utc>,
    expires: DateTime<Utc>,
) -> Value {
    json!({
        "approval_id": uuid::Uuid::new_v4().to_string(),
        "capability": capability,
        "requester_ref": requester,
        "approver_ref": approver,
        "intent_digest": wire::sha256_hex(b"intent"),
        "decided_at": decided.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "expires_at": expires.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

fn approve_call() -> Call {
    command(
        "/verification/cases/case-1/partner-go-live/approve",
        APPROVE,
        "verification-review",
        "cfmeet.verification.reviewer",
    )
}

async fn allowed(h: &Harness, c: &Call) -> bool {
    let before = h.store.audit_entries().len();
    let _ = send(h, c).await;
    h.store
        .audit_entries()
        .get(before)
        .is_some_and(|e| e.0.decision == "ALLOWED")
}

#[tokio::test]
async fn two_person_commands_need_a_valid_single_use_approval() {
    let h = start(Options {
        catalog: Some(served_command_catalog()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    let now = Utc::now();
    assert_eq!(
        code(&send(&h, &approve_call()).await.1),
        "APPROVAL_REQUIRED"
    );

    let mut wrong_cap = approve_call();
    wrong_cap.approval = Some(approval(
        "verification.app-review.approve",
        "staff-0001",
        "staff-0002",
        now,
        now + Duration::hours(12),
    ));
    assert_eq!(code(&send(&h, &wrong_cap).await.1), "APPROVAL_INVALID");

    let mut self_approved = approve_call();
    self_approved.approval = Some(approval(
        APPROVE,
        "staff-0001",
        "staff-0001",
        now,
        now + Duration::hours(1),
    ));
    assert_eq!(code(&send(&h, &self_approved).await.1), "APPROVAL_INVALID");

    let mut not_requester = approve_call();
    not_requester.approval = Some(approval(
        APPROVE,
        "staff-0009",
        "staff-0002",
        now,
        now + Duration::hours(1),
    ));
    assert_eq!(code(&send(&h, &not_requester).await.1), "APPROVAL_INVALID");

    let mut too_long = approve_call();
    too_long.approval = Some(approval(
        APPROVE,
        "staff-0001",
        "staff-0002",
        now - Duration::minutes(1),
        now + Duration::hours(12),
    ));
    assert_eq!(code(&send(&h, &too_long).await.1), "APPROVAL_INVALID");

    let mut expired = approve_call();
    expired.approval = Some(approval(
        APPROVE,
        "staff-0001",
        "staff-0002",
        now - Duration::hours(13),
        now - Duration::hours(1),
    ));
    assert_eq!(code(&send(&h, &expired).await.1), "APPROVAL_EXPIRED");

    let good = approval(
        APPROVE,
        "staff-0001",
        "staff-0002",
        now - Duration::minutes(5),
        now + Duration::hours(11),
    );
    let mut first = approve_call();
    first.approval = Some(good.clone());
    assert!(
        allowed(&h, &first).await,
        "a valid approval passes the chain"
    );
    let entry = h.store.audit_entries().last().unwrap().0.clone();
    assert_eq!(entry.approver.as_deref(), Some("hq:staff-0002"));
    assert_eq!(entry.approval_id.as_deref(), good["approval_id"].as_str());
    let mut reuse = approve_call();
    reuse.approval = Some(good);
    assert_eq!(code(&send(&h, &reuse).await.1), "APPROVAL_REUSED");
}

#[tokio::test]
async fn risky_commands_need_fresh_mfa_idempotency_and_switches() {
    let h = start(Options {
        catalog: Some(served_command_catalog()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    // R2 command: MFA older than 300 s is refused
    let retry = || {
        command(
            "/partners/workspaces/ws-1/webhook-deliveries/d-1/retry",
            "partners.webhook-delivery.retry",
            "ops-incident",
            "cfmeet.partner_ops",
        )
    };
    let mut stale = retry();
    stale.auth_age = 301;
    assert_eq!(code(&send(&h, &stale).await.1), "RECENT_MFA_REQUIRED");
    let mut no_key = retry();
    no_key.idempotency_key = None;
    assert_eq!(code(&send(&h, &no_key).await.1), "IDEMPOTENCY_KEY_REQUIRED");
    assert!(allowed(&h, &retry()).await);

    // writes off: the same command is disabled
    let off = start(Options {
        catalog: Some(served_command_catalog()),
        switches_all_on: false,
        ..Default::default()
    })
    .await;
    assert_eq!(code(&send(&off, &retry()).await.1), "CAPABILITY_DISABLED");
}
