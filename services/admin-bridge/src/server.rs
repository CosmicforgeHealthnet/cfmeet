//! Listeners. HQ: private TLS 1.3 mutual-TLS port (client certificate required and
//! verified against the configured CA; its fingerprint and URI SANs are handed to the
//! security chain, which checks enrolment on every request). Ops: loopback-only
//! `/health`, `/ready`, `/metrics`.

use std::{convert::Infallible, path::Path, sync::Arc, time::Duration};

use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Incoming, Request, Response};
use hyper_util::rt::TokioIo;
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
    RootCertStore, ServerConfig,
};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::chain::{request::MAX_BODY_BYTES, Bridge, Inbound, PeerCert};
use crate::errors::ErrorCode;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum TlsSetupError {
    #[error("cannot read {0}")]
    Read(&'static str),
    #[error("{0} holds no usable PEM item")]
    Empty(&'static str),
    #[error("TLS configuration rejected: {0}")]
    Rustls(String),
}

fn certs(path: &Path, what: &'static str) -> Result<Vec<CertificateDer<'static>>, TlsSetupError> {
    let pem = std::fs::read(path).map_err(|_| TlsSetupError::Read(what))?;
    let certs: Vec<_> = rustls_pemfile::certs(&mut pem.as_slice())
        .collect::<Result<_, _>>()
        .map_err(|_| TlsSetupError::Empty(what))?;
    if certs.is_empty() {
        return Err(TlsSetupError::Empty(what));
    }
    Ok(certs)
}

fn key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsSetupError> {
    let pem = std::fs::read(path).map_err(|_| TlsSetupError::Read("ADMIN_BRIDGE_TLS_KEY_FILE"))?;
    rustls_pemfile::private_key(&mut pem.as_slice())
        .ok()
        .flatten()
        .ok_or(TlsSetupError::Empty("ADMIN_BRIDGE_TLS_KEY_FILE"))
}

/// TLS 1.3 only; client certificate mandatory and chained to `client_ca`.
pub fn tls_acceptor(
    cert: &Path,
    key_path: &Path,
    client_ca: &Path,
) -> Result<TlsAcceptor, TlsSetupError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    for c in certs(client_ca, "ADMIN_BRIDGE_CLIENT_CA_FILE")? {
        roots
            .add(c)
            .map_err(|e| TlsSetupError::Rustls(e.to_string()))?;
    }
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|e| TlsSetupError::Rustls(e.to_string()))?;
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TlsSetupError::Rustls(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs(cert, "ADMIN_BRIDGE_TLS_CERT_FILE")?, key(key_path)?)
        .map_err(|e| TlsSetupError::Rustls(e.to_string()))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Fingerprint and URI SANs of the leaf client certificate.
pub fn peer_cert(der: &[u8]) -> PeerCert {
    let uri_sans = x509_parser::parse_x509_certificate(der)
        .ok()
        .and_then(|(_, cert)| {
            cert.subject_alternative_name()
                .ok()
                .flatten()
                .map(|ext| ext.value.clone())
        })
        .map(|san| {
            san.general_names
                .iter()
                .filter_map(|n| match n {
                    x509_parser::extensions::GeneralName::URI(u) => Some(u.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    PeerCert {
        fingerprint: crate::wire::sha256_hex(der),
        uri_sans,
    }
}

fn json_response(
    status: u16,
    correlation: Option<String>,
    body: Vec<u8>,
) -> Response<Full<bytes::Bytes>> {
    let mut b = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .header("x-content-type-options", "nosniff");
    if let Some(c) = correlation {
        b = b.header("x-correlation-id", c);
    }
    b.body(Full::new(bytes::Bytes::from(body)))
        .expect("valid response")
}

async fn serve_hq(
    bridge: Arc<Bridge>,
    peer: Option<PeerCert>,
    req: Request<Incoming>,
) -> Result<Response<Full<bytes::Bytes>>, Infallible> {
    let (parts, body) = req.into_parts();
    let refuse = |code: ErrorCode| {
        let id = uuid::Uuid::new_v4();
        let body = crate::chain::envelope(id, (bridge.clock)(), "UNAVAILABLE", None, Some(code));
        Ok(json_response(code.status(), Some(id.to_string()), body))
    };
    let declared = parts
        .headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_BODY_BYTES) {
        return refuse(ErrorCode::BodyTooLarge);
    }
    let body = match Limited::new(body, MAX_BODY_BYTES).collect().await {
        Ok(b) => b.to_bytes().to_vec(),
        Err(_) => return refuse(ErrorCode::BodyTooLarge),
    };
    let inbound = Inbound {
        method: parts.method.as_str().to_string(),
        path: parts.uri.path().to_string(),
        query: parts.uri.query().map(str::to_string),
        headers: parts.headers,
        body,
        peer,
    };
    let out = bridge.handle(inbound).await;
    Ok(json_response(
        out.status,
        Some(out.correlation_id.to_string()),
        out.body,
    ))
}

/// Accept loop of the HQ listener. Returns when the listener fails.
pub async fn run_hq(bridge: Arc<Bridge>, listener: TcpListener, acceptor: TlsAcceptor) {
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            continue;
        };
        let (bridge, acceptor) = (bridge.clone(), acceptor.clone());
        tokio::spawn(async move {
            let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await
            else {
                return; // handshake failed: no client certificate, wrong CA, not TLS 1.3
            };
            let peer = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|c| c.first())
                .map(|leaf| peer_cert(leaf.as_ref()));
            let service =
                hyper::service::service_fn(move |req| serve_hq(bridge.clone(), peer.clone(), req));
            let _ = hyper::server::conn::http1::Builder::new()
                .max_buf_size(MAX_BODY_BYTES + 16 * 1024)
                .serve_connection(TokioIo::new(tls), service)
                .await;
        });
    }
}

async fn serve_ops(
    bridge: Arc<Bridge>,
    req: Request<Incoming>,
) -> Result<Response<Full<bytes::Bytes>>, Infallible> {
    let policy_revision = bridge
        .policy
        .read()
        .expect("policy lock")
        .as_ref()
        .map(|p| p.revision);
    Ok(match (req.method().as_str(), req.uri().path()) {
        ("GET", "/health") => json_response(200, None, br#"{"status":"ok"}"#.to_vec()),
        ("GET", "/ready") => {
            let db = bridge.store.ping().await.is_ok();
            let ready = !bridge.switches.enabled || (db && policy_revision.is_some());
            let body = json!({
                "ready": ready,
                "enabled": bridge.switches.enabled,
                "database": if !bridge.database_configured { "NOT_CONFIGURED" } else if db { "OK" } else { "UNAVAILABLE" },
                "contract_version": crate::contract::model::CONTRACT_VERSION,
                "openapi_sha256": bridge.openapi_sha256,
                "policy_revision": policy_revision,
            });
            json_response(
                if ready { 200 } else { 503 },
                None,
                serde_json::to_vec(&body).expect("json"),
            )
        }
        ("GET", "/metrics") => Response::builder()
            .status(200)
            .header("content-type", "text/plain; version=0.0.4")
            .body(Full::new(bytes::Bytes::from(
                bridge
                    .metrics
                    .render(policy_revision.is_some(), bridge.switches.enabled),
            )))
            .expect("valid response"),
        _ => json_response(404, None, br#"{"error":"not found"}"#.to_vec()),
    })
}

/// Plain-HTTP ops listener; the address is loopback (enforced by configuration).
pub async fn run_ops(bridge: Arc<Bridge>, listener: TcpListener) {
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            continue;
        };
        let bridge = bridge.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req| serve_ops(bridge.clone(), req));
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(tcp), service)
                .await;
        });
    }
}

/// Maintenance: claim / rate-bucket cleanup, policy re-verification and the settlement of
/// open commands, every minute.
pub async fn run_maintenance(bridge: Arc<Bridge>) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        if let Err(e) = bridge.store.cleanup((bridge.clock)()).await {
            tracing::warn!(error = %e, "cleanup failed");
        }
        if bridge.switches.enabled {
            if let Err(e) = bridge.load_policy().await {
                tracing::error!(error = %e, "access policy not verified: refusing every request with POLICY_UNAVAILABLE");
            }
            // Open commands are settled from the owners' records, never resent.
            let settled = crate::routes::commands::sweep(&bridge).await;
            if settled > 0 {
                tracing::info!(settled, "operations settled");
            }
        }
    }
}
