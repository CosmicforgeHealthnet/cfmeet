//! `security-scan-gateway-probe`: on-host acceptance of a deployed gateway.
//!
//! Signs as a REGISTERED caller with that caller's own key (mounted read-only by the
//! operator tooling) and checks, against every target URL:
//!
//! * unauthenticated, other-environment and replayed tokens are refused;
//! * a harmless clean fixture is CLEAN and the EICAR test file is INFECTED, both with
//!   verdict signatures that verify and are bound to request, caller, digest and length;
//! * empty, oversized and digest-mismatched requests are rejected;
//! * latency of sequential scans, a parallel burst (never a wrong verdict, only
//!   CLEAN or OVERLOADED) and one large file;
//! * the engine's raw TCP port is NOT reachable from the caller's network.
//!
//! Environment: `PROBE_SIGNER_KEY_FILE`, `PROBE_SIGNER_KID`, `PROBE_ISSUER`,
//! `PROBE_ENVIRONMENT`, `PROBE_VERDICT_KEYS_FILE`, `PROBE_TARGETS` (comma-separated base
//! URLs), `PROBE_FORBIDDEN_ADDRESSES` (comma-separated host:port), `PROBE_MAX_BYTES`,
//! `PROBE_BURST` (default 6). Prints one JSON evidence document; exit 1 on any failure.
//! Only fixed synthetic bytes are ever sent.

use std::{sync::Arc, time::Duration};

use cf_scan_gateway_contract::{
    Expected, ScanResponse, Verdict, VerdictKeys, BODY_CONTENT_TYPE, CONTENT_SHA256_HEADER,
    SCAN_PATH,
};
use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, SystemClock, TokenTtl,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn var(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| {
        eprintln!("{k} is required");
        std::process::exit(64)
    })
}

fn eicar() -> Vec<u8> {
    [
        "X5O!P%@AP[4\\PZX54(P^)7CC)7}",
        "$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*",
    ]
    .concat()
    .into_bytes()
}

struct Probe {
    http: reqwest::Client,
    signer: Arc<ServiceTokenSigner>,
    other_env: Arc<ServiceTokenSigner>,
    keys: VerdictKeys,
    environment: String,
    issuer: String,
}

struct Answer {
    status: u16,
    body: Value,
    request_id: String,
    ms: u128,
}

impl Probe {
    async fn send(
        &self,
        base: &str,
        signer: Option<&ServiceTokenSigner>,
        reuse: Option<&http::HeaderMap>,
        data: Vec<u8>,
        digest: Option<String>,
    ) -> (Answer, http::HeaderMap) {
        let (headers, request_id) = match (reuse, signer) {
            (Some(h), _) => (h.clone(), String::new()),
            (None, Some(s)) => {
                let signed = s
                    .sign(&OutboundServiceRequest::new_operation(
                        ServiceId::security_scan(),
                    ))
                    .expect("signed");
                (signed.headers(), signed.request_id().as_str().to_string())
            }
            (None, None) => (http::HeaderMap::new(), String::new()),
        };
        let digest = digest.unwrap_or_else(|| hex::encode(Sha256::digest(&data)));
        let started = std::time::Instant::now();
        let r = self
            .http
            .post(format!("{base}{SCAN_PATH}"))
            .headers(headers.clone())
            .header("content-type", BODY_CONTENT_TYPE)
            .header(CONTENT_SHA256_HEADER, digest)
            .header("content-length", data.len().to_string())
            .body(data)
            .send()
            .await;
        let (status, body) = match r {
            Ok(r) => {
                let s = r.status().as_u16();
                (s, r.json().await.unwrap_or(Value::Null))
            }
            Err(_) => (0, Value::Null),
        };
        (
            Answer {
                status,
                body,
                request_id,
                ms: started.elapsed().as_millis(),
            },
            headers,
        )
    }

    async fn scan(&self, base: &str, data: Vec<u8>) -> Answer {
        self.send(base, Some(&self.signer), None, data, None)
            .await
            .0
    }

    fn verified(&self, a: &Answer, data: &[u8]) -> Option<ScanResponse> {
        let r: ScanResponse = serde_json::from_value(a.body.clone()).ok()?;
        let digest = hex::encode(Sha256::digest(data));
        self.keys
            .verify(
                &r,
                Expected {
                    request_id: &a.request_id,
                    client_id: &self.issuer,
                    environment: &self.environment,
                    sha256: &digest,
                    bytes: data.len() as u64,
                },
            )
            .ok()?;
        Some(r)
    }
}

fn percentile(mut xs: Vec<u128>, p: f64) -> u128 {
    if xs.is_empty() {
        return 0;
    }
    xs.sort();
    xs[((xs.len() as f64 - 1.0) * p).round() as usize]
}

#[tokio::main]
async fn main() {
    let environment = RuntimeEnvironment::parse(&var("PROBE_ENVIRONMENT")).expect("environment");
    let issuer = ServiceId::parse(&var("PROBE_ISSUER")).expect("issuer");
    let kid = KeyId::parse(&var("PROBE_SIGNER_KID")).expect("kid");
    let key = || {
        ServiceSigningKey::from_pkcs8_pem_file(
            kid.clone(),
            std::path::Path::new(&var("PROBE_SIGNER_KEY_FILE")),
        )
        .expect("signer key")
    };
    let other = if environment == RuntimeEnvironment::Production {
        RuntimeEnvironment::Staging
    } else {
        RuntimeEnvironment::Production
    };
    let mk = |env| {
        Arc::new(ServiceTokenSigner::new(
            issuer.clone(),
            env,
            key(),
            TokenTtl::default(),
            Arc::new(SystemClock),
        ))
    };
    let probe = Probe {
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("client"),
        signer: mk(environment),
        other_env: mk(other),
        keys: VerdictKeys::from_json(
            &std::fs::read_to_string(var("PROBE_VERDICT_KEYS_FILE")).expect("keys"),
        )
        .expect("verdict keys"),
        environment: environment.as_str().to_string(),
        issuer: issuer.as_str().to_string(),
    };
    let max_bytes: usize = var("PROBE_MAX_BYTES").parse().expect("max bytes");
    let burst: usize = std::env::var("PROBE_BURST")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    let mut evidence = json!({});
    let mut failures: Vec<String> = Vec::new();
    let mut check = |name: String, ok: bool, detail: Value, ev: &mut Value| {
        ev[&name] = json!({"pass": ok, "detail": detail});
        if !ok {
            failures.push(name);
        }
    };

    for base in var("PROBE_TARGETS").split(',').filter(|s| !s.is_empty()) {
        let label = base.to_string();
        let clean = b"CosmicForge shared scan gateway acceptance fixture (harmless)".to_vec();

        let (a, _) = probe.send(base, None, None, clean.clone(), None).await;
        check(
            format!("{label} unauthenticated_refused"),
            a.status == 401,
            json!(a.status),
            &mut evidence,
        );
        let (a, _) = probe
            .send(base, Some(&probe.other_env), None, clean.clone(), None)
            .await;
        check(
            format!("{label} other_environment_token_refused"),
            a.status == 401,
            json!(a.status),
            &mut evidence,
        );

        let (a, headers) = probe
            .send(base, Some(&probe.signer), None, clean.clone(), None)
            .await;
        let v = probe.verified(&a, &clean);
        check(
            format!("{label} clean_signed_and_bound"),
            a.status == 200 && v.as_ref().is_some_and(|r| r.verdict == Verdict::Clean),
            json!({"status": a.status, "ms": a.ms, "engine": a.body.get("engine")}),
            &mut evidence,
        );
        let (a, _) = probe
            .send(base, None, Some(&headers), clean.clone(), None)
            .await;
        check(
            format!("{label} replayed_token_refused"),
            a.status == 401,
            json!(a.status),
            &mut evidence,
        );

        let e = eicar();
        let a = probe.scan(base, e.clone()).await;
        let v = probe.verified(&a, &e);
        check(
            format!("{label} eicar_infected_signed_and_bound"),
            a.status == 200 && v.as_ref().is_some_and(|r| r.verdict == Verdict::Infected),
            json!({"status": a.status, "finding": a.body.get("finding"), "ms": a.ms}),
            &mut evidence,
        );

        let (a, _) = probe
            .send(
                base,
                Some(&probe.signer),
                None,
                clean.clone(),
                Some("0".repeat(64)),
            )
            .await;
        check(
            format!("{label} digest_mismatch_rejected"),
            a.status == 422
                && a.body["reason"] == "DIGEST_MISMATCH"
                && a.body.get("verdictSignature").is_none(),
            json!(a.status),
            &mut evidence,
        );
        let a = probe.scan(base, Vec::new()).await;
        check(
            format!("{label} empty_rejected"),
            a.status == 422,
            json!(a.status),
            &mut evidence,
        );
        let a = probe.scan(base, vec![0u8; max_bytes + 1]).await;
        check(
            format!("{label} oversized_rejected"),
            a.status == 413,
            json!(a.status),
            &mut evidence,
        );

        // Sequential latency of a small file.
        let mut ms = Vec::new();
        let mut wrong = 0;
        for i in 0..10 {
            let data = format!("sequential latency fixture {i}").into_bytes();
            let a = probe.scan(base, data.clone()).await;
            if probe.verified(&a, &data).map(|r| r.verdict) != Some(Verdict::Clean) {
                wrong += 1;
            }
            ms.push(a.ms);
        }
        check(
            format!("{label} sequential_small_scans"),
            wrong == 0,
            json!({"p50_ms": percentile(ms.clone(), 0.5), "p95_ms": percentile(ms, 0.95), "not_clean": wrong}),
            &mut evidence,
        );

        // Parallel burst: never a wrong verdict; extra load is refused, not misjudged.
        let probe_ref = &probe;
        let futures: Vec<_> = (0..burst)
            .map(|i| async move {
                let data = vec![b'a' + (i % 26) as u8; 512 * 1024];
                let a = probe_ref.scan(base, data.clone()).await;
                (
                    probe_ref.verified(&a, &data).map(|r| r.verdict),
                    a.body["reason"].clone(),
                    a.ms,
                )
            })
            .collect();
        let results = futures_util::future::join_all(futures).await;
        let clean_n = results
            .iter()
            .filter(|r| r.0 == Some(Verdict::Clean))
            .count();
        let overloaded = results.iter().filter(|r| r.1 == "OVERLOADED").count();
        check(
            format!("{label} parallel_burst_bounded"),
            clean_n >= 1 && clean_n + overloaded == burst,
            json!({"burst": burst, "clean": clean_n, "overloaded": overloaded,
                   "max_ms": results.iter().map(|r| r.2).max()}),
            &mut evidence,
        );

        // One large file just under the engine limit.
        let large = vec![b'L'; max_bytes - 1024];
        let a = probe.scan(base, large.clone()).await;
        check(
            format!("{label} large_file_streamed"),
            probe.verified(&a, &large).map(|r| r.verdict) == Some(Verdict::Clean),
            json!({"bytes": large.len(), "ms": a.ms, "status": a.status}),
            &mut evidence,
        );
    }

    for address in std::env::var("PROBE_FORBIDDEN_ADDRESSES")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
    {
        let reachable = matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                tokio::net::TcpStream::connect(address)
            )
            .await,
            Ok(Ok(_))
        );
        check(
            format!("engine_not_reachable_from_caller_network {address}"),
            !reachable,
            json!(reachable),
            &mut evidence,
        );
    }

    let pass = failures.is_empty();
    println!(
        "{}",
        json!({"scanGatewayAcceptance": evidence, "pass": pass, "failures": failures})
    );
    if !pass {
        std::process::exit(1);
    }
}
