//! Shared HQ simulator harness: real TLS 1.3 mTLS listener, real HMAC v2 and Ed25519
//! staff assertions, in-memory store, optional fake owners.
#![allow(dead_code, unused_imports)]

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

pub const WORKLOAD: &str = "cf://staging/product-admin/cfmeet";
pub const HMAC_KID: &str = "hq-hmac-1";
pub const HMAC_KEY: &[u8] = b"test-hmac-key-material-at-least-32-bytes-long";
pub const STAFF_KID: &str = "hq-staff-1";
pub const TENANT: &str = "cfmeet-first-party";
/// A Platform Access tenant id (owners that key data by tenant, e.g. Calling, need a UUID).
pub const TENANT_ID: &str = "4d1f9a2e-7b3c-4e5d-8f6a-9b0c1d2e3f4a";

pub struct Pki {
    pub dir: tempfile::TempDir,
    pub ca_pem: String,
    pub client_cert_der: Vec<u8>,
    pub client_cert_pem: String,
    pub client_key_pem: String,
    pub other_cert_pem: String,
    pub other_key_pem: String,
}

pub fn pki() -> Pki {
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
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, HMAC_KEY),
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

pub fn staff_key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

pub fn policy_key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

pub struct Harness {
    pub pki: Pki,
    pub store: Arc<MemoryStore>,
    pub addr: std::net::SocketAddr,
    pub bridge: Arc<Bridge>,
}

pub struct Options {
    pub owners: Option<Arc<dyn admin_bridge::owners::OwnerPort>>,
    pub enabled: bool,
    pub policy: bool,
    pub catalog: Option<Vec<Capability>>,
    pub switches_all_on: bool,
    pub test_commands: Vec<(&'static str, Owner, &'static str)>,
    pub releases: Option<Arc<admin_bridge::releases::ReleaseWorkflow>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            owners: None,
            enabled: true,
            policy: true,
            catalog: None,
            switches_all_on: false,
            test_commands: Vec::new(),
            releases: None,
        }
    }
}

pub async fn start(opts: Options) -> Harness {
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
        enrolled_tenants: HashSet::from([TENANT.to_string(), TENANT_ID.to_string()]),
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
    if let Some(o) = opts.owners {
        bridge = bridge.with_owners(o);
    }
    if let Some(r) = opts.releases {
        bridge = bridge.with_releases(r);
    }
    if !opts.test_commands.is_empty() {
        bridge = bridge.with_test_commands(opts.test_commands);
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
    tokio::spawn(server::run_hq(bridge.clone(), listener, acceptor));
    Harness {
        pki,
        store,
        addr,
        bridge,
    }
}

// ---- HQ client side (mirrors karryam-admin signing.js with cfmeet values) ---------

#[derive(Clone)]
pub struct Call {
    pub method: &'static str,
    pub uri: String,
    pub body: Vec<u8>,
    pub sub: String,
    pub session: String,
    pub roles: Vec<&'static str>,
    pub permissions: Vec<String>,
    pub acr: &'static str,
    pub tenant_scopes: Vec<&'static str>,
    pub lifetime: i64,
    pub iat_offset: i64,
    pub auth_age: i64,
    pub hmac_ts_offset: i64,
    pub jti: String,
    pub nonce: String,
    pub approval: Option<Value>,
    pub digest_uri: Option<String>,
    pub hmac_key: Vec<u8>,
    pub idempotency_key: Option<String>,
}

pub fn scoped(purpose: &str) -> String {
    format!("market=GLOBAL&organization=cfmeet&zone=production&tenant={TENANT}&purpose={purpose}")
}

pub fn call(path: &str, query: &str, permission: &str, roles: &[&'static str]) -> Call {
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

pub fn rand_bytes() -> [u8; 16] {
    *uuid::Uuid::new_v4().as_bytes()
}

pub fn discovery_call() -> Call {
    call(
        "/capabilities",
        &scoped("admin-ui-capabilities"),
        "bridge.capabilities.read",
        &["cfmeet.viewer"],
    )
}

pub fn window(days: i64) -> String {
    let end = (Utc::now() - Duration::minutes(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let start = (Utc::now() - Duration::days(days))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    format!("&limit=31&start={start}&end={end}")
}

pub fn headers(c: &Call) -> Vec<(String, String)> {
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

pub fn client_config(
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

pub async fn send_with(
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

pub async fn send(h: &Harness, c: &Call) -> (u16, Value) {
    let tls = client_config(
        &h.pki,
        Some((&h.pki.client_cert_pem, &h.pki.client_key_pem)),
        false,
    );
    send_with(h, c, tls, None).await.expect("request completes")
}

pub fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("<none>")
}

pub fn assert_envelope(v: &Value, ok: bool) {
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
