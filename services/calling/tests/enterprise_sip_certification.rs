//! Part F enterprise SIP certification (§57 IVR; extended by later stages) across REAL
//! components: Calling (this crate, served on a real port), the real `telephony` binary
//! with its own database and signed internal HTTP, a real Asterisk edge
//! (`infra/telephony/certification`, ARI) and SIPp acting as an enterprise desk phone
//! (RTP + RFC 4733 keypad input) on loopback.
//!
//! Certified: an enterprise device's dialog offered to Calling as ONE canonical Call over
//! ENTERPRISE_INTERNAL, the edge answering it on Calling's command, built-in prompts
//! played at the edge, a valid key routing to an extension, an invalid key with retry
//! routing to a queue, a timeout with retry ending in the HANGUP node (the platform hangs
//! up), an explicit hangup branch, and the caller hanging up (ABANDONED). Part F §60
//! voicemail: an IVR key routes the SAME Call to a mailbox, the edge plays the greeting
//! and records the caller's REAL audio (SIPp RTP) into its transient spool, Telephony
//! delivers it to Calling, Calling stores it in a real S3-compatible private bucket
//! (MinIO in CI), the message is READY and linked to the Call, authorized media access
//! returns the same WAV through a short-lived signed URL, it is marked read, and deletion
//! removes the object; the edge spool copy is gone. Part F §61 recording: a desk phone
//! calls a member's application endpoint, the call is bridged to application media at the
//! edge (SIPp as the media side), recording is DENIED without the configured consent, then
//! ALLOWED with server-issued notice (a tone at the edge, a realtime notice in the app);
//! the edge records the bridge's mixed audio, an explicit stop ends it (CAPTURE_STOP), the
//! artifact lands in the private bucket, retrieval is authorized and audited. No carrier,
//! no public number: this certifies internal enterprise SIP, never real PSTN.
//!
//! Requires `TELEPHONY_BIN`, `TELEPHONY_MIGRATION_BIN` (with `telephony-admin` next to
//! them), `asterisk`, `sipp`, `TEST_DATABASE_URL` and (voicemail) an S3-compatible
//! endpoint in `ENTERPRISE_CERT_S3_ENDPOINT`; prints SKIPPED otherwise — unless
//! `ENTERPRISE_SIP_CERT_REQUIRED=1` (CI), where anything missing FAILS.

mod common;

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use axum::http::{Method, StatusCode};
use calling::{calls::CallState, internal_auth::ServiceAuthRuntime};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use chrono::Utc;
use common::{
    app::{AppWorld, Device},
    enterprise::{id_of, EnterpriseWorld},
    TestDb,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::json;
use tokio::{process::Child, task::JoinHandle};
use uuid::Uuid;

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;
const ARI_URL: &str = "http://127.0.0.1:8088";
const ARI_USER: &str = "cfmeet-cert";
const ARI_PASSWORD: &str = "certification-only-ari-password-0123456789";
const EDGE: &str = "127.0.0.1:5070";
const DEVICE_IP: &str = "127.0.0.5";
const DEVICE_REF: &str = "dev-cert-desk-01";

fn required() -> bool {
    std::env::var("ENTERPRISE_SIP_CERT_REQUIRED").is_ok_and(|v| v == "1")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn cert_dir() -> PathBuf {
    repo_root().join("infra/telephony/certification")
}

fn available(binary: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {binary}")])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn run(bin: &Path, args: &[&str], env: &[(&str, String)]) -> String {
    let mut c = Command::new(bin);
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c
        .output()
        .unwrap_or_else(|e| panic!("{} runs: {e}", bin.display()));
    assert!(
        out.status.success(),
        "{} {args:?}: {}",
        bin.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

struct Asterisk {
    child: std::process::Child,
    run: PathBuf,
}

impl Drop for Asterisk {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Asterisk {
    fn start() -> Self {
        let run = std::env::temp_dir().join(format!("cfmeet-ent-cert-{}", Uuid::new_v4().simple()));
        for d in [
            "etc",
            "varlib",
            "spool",
            "spool/recording",
            "run",
            "log",
            "agi",
            "moh",
        ] {
            std::fs::create_dir_all(run.join(d)).unwrap();
        }
        let tone: Vec<u8> = (0..8000)
            .flat_map(|i| {
                let v = (f64::sin(2.0 * std::f64::consts::PI * 425.0 * f64::from(i) / 8000.0)
                    * 6000.0) as i16;
                v.to_le_bytes()
            })
            .collect();
        std::fs::write(run.join("moh/hold-tone.sln"), tone).unwrap();
        for entry in std::fs::read_dir(cert_dir()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "conf") {
                let text = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("@RUNDIR@", run.to_str().unwrap());
                std::fs::write(run.join("etc").join(path.file_name().unwrap()), text).unwrap();
            }
        }
        let log = std::fs::File::create(run.join("console.log")).unwrap();
        let child = Command::new("asterisk")
            .args([
                "-C",
                run.join("etc/asterisk.conf").to_str().unwrap(),
                "-f",
                "-n",
            ])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("asterisk starts");
        Self { child, run }
    }

    fn console(&self) -> String {
        std::fs::read_to_string(self.run.join("console.log")).unwrap_or_default()
    }

    async fn ready(&self) {
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        for _ in 0..200 {
            if let Ok(r) = http
                .get(format!("{ARI_URL}/ari/asterisk/info"))
                .basic_auth(ARI_USER, Some(ARI_PASSWORD))
                .send()
                .await
            {
                if r.status().is_success() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("asterisk ARI not ready:\n{}", self.console());
    }
}

/// How a device scenario ends.
enum End {
    /// The desk phone hangs up after `ms`.
    CallerHangsUp(u32),
    /// The platform hangs up (BYE from the edge) within `ms`.
    PlatformHangsUp(u32),
    /// After `wait_ms` (a greeting) the desk phone speaks (7 s of real G.711 audio), then
    /// hangs up after `ms`.
    LeavesMessage { wait_ms: u32, ms: u32 },
}

/// The certification's S3-compatible private bucket (voicemail), if configured.
fn cert_store() -> Option<cf_object_storage::S3ObjectStore> {
    let Ok(endpoint) = std::env::var("ENTERPRISE_CERT_S3_ENDPOINT") else {
        assert!(!required(), "ENTERPRISE_CERT_S3_ENDPOINT is required");
        return None;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    Some(
        cf_object_storage::S3ObjectStore::new(cf_object_storage::S3Config {
            endpoint,
            public_endpoint: None,
            region: var("ENTERPRISE_CERT_S3_REGION", "us-east-1"),
            bucket: var("ENTERPRISE_CERT_S3_BUCKET", "cfmeet-cert-voicemail"),
            access_key: var("ENTERPRISE_CERT_S3_ACCESS_KEY", "testing"),
            secret_key: var("ENTERPRISE_CERT_S3_SECRET_KEY", "testing"),
            path_style: true,
            server_side_encryption: std::env::var("ENTERPRISE_CERT_S3_SSE").ok(),
        })
        .unwrap(),
    )
}

struct Sipp {
    child: std::process::Child,
    dir: PathBuf,
}

impl Drop for Sipp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An RFC 4733 key press for one dialog: SIPp's sample event (`dtmf_2833_1.pcap`) with the
/// event code of `key` and the RTP sequence / timestamp of the dialog's `index`-th press.
/// A real phone advances both with every press; the edge ignores an event whose timestamp
/// is not newer than the last one it ended (replaying the stock per-key captures in a
/// dialog, whose timestamps are unordered, would drop keys).
fn dtmf_pcap(dir: &std::path::Path, key: char, index: usize) -> PathBuf {
    let code = match key {
        '0'..='9' => key as u8 - b'0',
        '*' => 10,
        '#' => 11,
        _ => panic!("not a key"),
    };
    let mut data = std::fs::read("/usr/share/sip-tester/dtmf_2833_1.pcap").unwrap();
    const RTP: usize = 14 + 20 + 8; // Ethernet + IPv4 + UDP
    let base_seq = 40_000 + 100 * index as u32;
    let base_ts = 400_000 + 16_000 * index as u32;
    let mut first_seq = None;
    let mut off = 24;
    while off + 16 <= data.len() {
        let len = u32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap()) as usize;
        let rtp = off + 16 + RTP;
        let seq = u16::from_be_bytes([data[rtp + 2], data[rtp + 3]]);
        let first = *first_seq.get_or_insert(seq);
        let new_seq = (base_seq + u32::from(seq.wrapping_sub(first))) as u16;
        data[rtp + 2..rtp + 4].copy_from_slice(&new_seq.to_be_bytes());
        data[rtp + 4..rtp + 8].copy_from_slice(&base_ts.to_be_bytes());
        data[rtp + 12] = code;
        off += 16 + len;
    }
    let path = dir.join(format!("key-{index}.pcap"));
    std::fs::write(&path, data).unwrap();
    path
}

impl Sipp {
    /// The desk phone dials `extension`, waits `before_ms`, presses `keys` (RFC 4733,
    /// 2.5 s apart) and ends as told right after the last key.
    fn dial(extension: &str, before_ms: u32, keys: &[char], end: End, media_port: u16) -> Self {
        let dir = std::env::temp_dir().join(format!("cfmeet-ent-sipp-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_xml: String = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                // Keys 2.5 s apart; after the last one the scenario's end takes over (the
                // platform may hang up right away).
                let pause = if i + 1 < keys.len() {
                    "  <pause milliseconds=\"2500\" />\n"
                } else {
                    ""
                };
                format!(
                    "  <nop><action><exec play_pcap_audio=\"{}\" /></action></nop>\n{pause}",
                    dtmf_pcap(&dir, *k, i).display()
                )
            })
            .collect();
        let end_xml = match end {
            End::CallerHangsUp(ms) => format!(
                "  <pause milliseconds=\"{ms}\" />\n  <send retrans=\"500\"><![CDATA[\nBYE [next_url] SIP/2.0\nVia: SIP/2.0/[transport] [local_ip]:[local_port];branch=[branch]\nFrom: <sip:ent-desk-1@[local_ip]:[local_port]>;tag=[pid]SIPpTag09[call_number]\nTo: <sip:[called]@[remote_ip]:[remote_port]>[peer_tag_param]\nCall-ID: [call_id]\nCSeq: 2 BYE\nContact: <sip:ent-desk-1@[local_ip]:[local_port]>\nMax-Forwards: 70\nContent-Length: 0\n\n]]></send>\n  <recv response=\"200\" />\n"
            ),
            End::PlatformHangsUp(ms) => format!(
                "  <recv request=\"BYE\" timeout=\"{ms}\" />\n  <send><![CDATA[\nSIP/2.0 200 OK\n[last_Via:]\n[last_From:]\n[last_To:]\n[last_Call-ID:]\n[last_CSeq:]\nContent-Length: 0\n\n]]></send>\n"
            ),
            End::LeavesMessage { wait_ms, ms } => format!(
                "  <pause milliseconds=\"{wait_ms}\" />\n  <nop><action><exec play_pcap_audio=\"/usr/share/sip-tester/g711a.pcap\" /></action></nop>\n  <pause milliseconds=\"{ms}\" />\n  <send retrans=\"500\"><![CDATA[\nBYE [next_url] SIP/2.0\nVia: SIP/2.0/[transport] [local_ip]:[local_port];branch=[branch]\nFrom: <sip:ent-desk-1@[local_ip]:[local_port]>;tag=[pid]SIPpTag09[call_number]\nTo: <sip:[called]@[remote_ip]:[remote_port]>[peer_tag_param]\nCall-ID: [call_id]\nCSeq: 2 BYE\nContact: <sip:ent-desk-1@[local_ip]:[local_port]>\nMax-Forwards: 70\nContent-Length: 0\n\n]]></send>\n  <recv response=\"200\" />\n"
            ),
        };
        let template =
            std::fs::read_to_string(cert_dir().join("sipp/enterprise_device_ivr.xml")).unwrap();
        let scenario = template
            .replace("@BEFORE_MS@", &before_ms.to_string())
            .replace("@KEYS@", &key_xml)
            .replace("@END@", &end_xml);
        std::fs::write(dir.join("scenario.xml"), scenario).unwrap();
        let child = Command::new("sipp")
            .arg(EDGE)
            .args([
                "-sf",
                dir.join("scenario.xml").to_str().unwrap(),
                "-s",
                extension,
                "-i",
                DEVICE_IP,
                "-p",
                "5080",
                "-mi",
                DEVICE_IP,
                "-min_rtp_port",
                &media_port.to_string(),
                "-max_rtp_port",
                &(media_port + 98).to_string(),
                "-m",
                "1",
                "-l",
                "1",
                "-timeout",
                "90s",
                "-timeout_error",
                "-nostdin",
                "-trace_err",
                "-trace_msg",
                "-key",
                "called",
                extension,
            ])
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("stderr.log")).unwrap())
            .spawn()
            .expect("sipp starts");
        Self { child, dir }
    }

    /// The application media side of an edge bridge (what the RTC media gateway does):
    /// presents the single-use join code from 127.0.0.3, exchanges RTP, waits for BYE.
    fn app_media(code: &str, media_port: u16) -> Self {
        let dir = std::env::temp_dir().join(format!("cfmeet-ent-sipp-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(
            cert_dir().join("sipp/app_media_uac.xml"),
            dir.join("scenario.xml"),
        )
        .unwrap();
        let child = Command::new("sipp")
            .arg(EDGE)
            .args([
                "-sf",
                dir.join("scenario.xml").to_str().unwrap(),
                "-s",
                code,
                "-i",
                "127.0.0.3",
                "-p",
                "5090",
                "-mi",
                "127.0.0.3",
                "-min_rtp_port",
                &media_port.to_string(),
                "-max_rtp_port",
                &(media_port + 98).to_string(),
                "-m",
                "1",
                "-l",
                "1",
                "-timeout",
                "90s",
                "-timeout_error",
                "-nostdin",
                "-trace_err",
                "-trace_msg",
                "-key",
                "code",
                code,
            ])
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("stderr.log")).unwrap())
            .spawn()
            .expect("sipp starts");
        Self { child, dir }
    }

    async fn finished_ok(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.success();
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn diagnostics(&self) -> String {
        let mut out = String::from("--- sipp ---\n");
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().ends_with(".xml") {
                    continue;
                }
                out.push_str(&format!("[{}]\n", e.file_name().to_string_lossy()));
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                let tail: Vec<&str> = text.lines().rev().take(60).collect();
                out.push_str(&tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
                out.push('\n');
            }
        }
        out
    }
}

struct Stack {
    e: EnterpriseWorld,
    asterisk: Asterisk,
    _telephony: Child,
    telephony_db: TestDb,
    tdb: DatabaseConnection,
    dir: PathBuf,
    _server: JoinHandle<()>,
    pump: Arc<AtomicBool>,
    caller: Uuid,
    member: Device,
    member_principal: Uuid,
    flow: Uuid,
    /// Voicemail (None: no S3-compatible store configured — scenario skipped).
    store: Option<Arc<cf_object_storage::S3ObjectStore>>,
    mailbox: Option<Uuid>,
    vm_token: String,
    /// Recording administration (recording.manage / read, config.read).
    rec_token: String,
    /// Application media attachments (room, identity, single-use join code).
    public_media: Arc<common::telephony::FakePublicMedia>,
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.pump.store(false, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Stack {
    async fn start() -> Option<Self> {
        let bins = (
            std::env::var_os("TELEPHONY_BIN").map(PathBuf::from),
            std::env::var_os("TELEPHONY_MIGRATION_BIN").map(PathBuf::from),
        );
        let (Some(telephony_bin), Some(migration_bin)) = bins else {
            assert!(
                !required(),
                "TELEPHONY_BIN / TELEPHONY_MIGRATION_BIN are required"
            );
            println!("SKIPPED enterprise SIP certification: TELEPHONY_BIN / TELEPHONY_MIGRATION_BIN not set");
            return None;
        };
        if !available("asterisk") || !available("sipp") {
            assert!(!required(), "asterisk and sipp are required");
            println!("SKIPPED enterprise SIP certification: asterisk / sipp not installed");
            return None;
        }
        let telephony_db = TestDb::empty().await?;
        let dir = std::env::temp_dir().join(format!("cf-ent-cert-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let calling_key = ServiceSigningKey::generate(KeyId::parse("calling-ent-1").unwrap());
        let telephony_key = ServiceSigningKey::generate(KeyId::parse("telephony-ent-1").unwrap());
        run(
            &migration_bin,
            &["up"],
            &[("DATABASE_URL", telephony_db.url.clone())],
        );
        std::fs::write(
            dir.join("telephony-trust.json"),
            common::trust_bundle_json(
                cf_service_auth::services::TELEPHONY,
                ENV,
                &[(
                    cf_service_auth::services::CALLING,
                    vec![calling_key.public_jwk()],
                    &[
                        "telephony-leg.terminate",
                        "telephony-leg.hold",
                        "telephony-leg.resume",
                        "telephony-leg.media",
                        "telephony-bridge.manage",
                        "telephony-status.read",
                    ],
                )],
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("telephony-key.pem"),
            telephony_key.to_pkcs8_pem().as_bytes(),
        )
        .unwrap();
        std::fs::write(dir.join("ari.password"), ARI_PASSWORD).unwrap();

        // The edge first (Telephony connects its ARI event stream to it).
        let asterisk = Asterisk::start();
        asterisk.ready().await;

        let (telephony_port, calling_port) = (free_port(), free_port());
        let telephony_url = format!("http://127.0.0.1:{telephony_port}");
        let calling_url = format!("http://127.0.0.1:{calling_port}");
        let telephony = tokio::process::Command::new(&telephony_bin)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("DATABASE_URL", &telephony_db.url)
            .env(
                "TELEPHONY_BIND_ADDRESS",
                format!("127.0.0.1:{telephony_port}"),
            )
            .env("TELEPHONY_ENABLED", "true")
            .env("ENTERPRISE_SIP_ENABLED", "true")
            .env("TELEPHONY_WORKER_INTERVAL_MS", "100")
            .env("TELEPHONY_SIP_EDGE", "asterisk-ari")
            .env("TELEPHONY_ARI_URL", ARI_URL)
            .env("TELEPHONY_ARI_USERNAME", ARI_USER)
            .env("TELEPHONY_ARI_PASSWORD_FILE", dir.join("ari.password"))
            .env("TELEPHONY_ARI_APP", "cfmeet-cert")
            .env("CF_RUNTIME_ENVIRONMENT", "test")
            .env("CF_SERVICE_AUTH_KEY_ID", "telephony-ent-1")
            .env(
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                dir.join("telephony-key.pem"),
            )
            .env(
                "CF_SERVICE_TRUST_BUNDLE_FILE",
                dir.join("telephony-trust.json"),
            )
            .env("CALLING_INTERNAL_BASE_URL", &calling_url)
            .env("CF_INTERNAL_HTTP_TIMEOUT_MS", "5000")
            .env("RUST_LOG", "telephony=info")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("telephony.log")).unwrap())
            .kill_on_drop(true)
            .spawn()
            .expect("telephony starts");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut healthy = false;
        for _ in 0..150 {
            if let Ok(r) = http.get(format!("{telephony_url}/health")).send().await {
                if r.status().is_success() {
                    healthy = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            healthy,
            "telephony did not become healthy:\n{}",
            std::fs::read_to_string(dir.join("telephony.log")).unwrap_or_default()
        );

        // Calling: trusts Telephony (events, enterprise offers / keypad).
        let clock: SharedClock = Arc::new(SystemClock);
        let policy = TrustPolicy::from_json(
            &common::trust_bundle_json(
                cf_service_auth::services::CALLING,
                ENV,
                &[(
                    cf_service_auth::services::TELEPHONY,
                    vec![telephony_key.public_jwk()],
                    &[
                        "telephony-event.apply",
                        "telephony-reconciliation.apply",
                        "telephony-enterprise.offer",
                        "telephony-capture.deliver",
                    ],
                )],
            ),
            &ServiceId::calling(),
            ENV,
        )
        .unwrap();
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::calling(),
            ENV,
            calling_key,
            TokenTtl::default(),
            clock.clone(),
        ));
        let mut targets = InternalTargets::new(ENV);
        targets
            .insert(ServiceId::telephony(), &telephony_url)
            .unwrap();
        let client = Arc::new(
            InternalServiceClient::new(signer.clone(), targets, Duration::from_secs(5)).unwrap(),
        );
        let runtime = ServiceAuthRuntime::new(
            signer,
            Arc::new(ServiceTokenVerifier::new(policy, clock)),
            client,
        )
        .unwrap();
        let (_, peers) = common::service_auth_fixture();
        // Protected artifact storage: a real S3-compatible private bucket.
        let store = cert_store().map(Arc::new);
        if let Some(store) = &store {
            store
                .ensure_bucket()
                .await
                .expect("the certification bucket");
        }
        let artifacts = store.clone();
        // The RTC side of an edge bridge: the gateway records the single-use join code;
        // SIPp then plays the application media side with it.
        let public_media = Arc::new(common::telephony::FakePublicMedia::default());
        let gateway = public_media.clone();
        let a = AppWorld::with_service_auth_public_network_configured(
            runtime,
            peers,
            Utc::now(),
            None,
            move |rt| {
                if let Some(s) = artifacts {
                    rt.artifacts = s;
                }
                rt.public_network.media = Some(gateway);
            },
        )
        .await?;
        let e = EnterpriseWorld::around(a).await;
        let scope = e.a.w.scope();
        // Telephony: the desk phone's edge endpoint, bound to this scope.
        let tadmin = telephony_bin.with_file_name("telephony-admin");
        let tenv = [
            ("DATABASE_URL", telephony_db.url.clone()),
            ("CF_RUNTIME_ENVIRONMENT", "test".to_string()),
        ];
        run(
            &tadmin,
            &[
                "enterprise-device",
                "add",
                "--device-ref",
                DEVICE_REF,
                "--edge-endpoint",
                "ent-desk-1",
                "--tenant",
                &scope.tenant_id().as_uuid().to_string(),
                "--application",
                &scope.application_id().as_uuid().to_string(),
                "--environment",
                &scope.environment_id().as_uuid().to_string(),
            ],
            &tenv,
        );
        run(&tadmin, &["enterprise-device", "enable", DEVICE_REF], &tenv);
        // Calling: the environment's owner of calls not created through the API.
        let credential = e.a.w.token.credential.id.as_uuid();
        e.h()
            .conn()
            .execute_unprepared(&format!(
                "INSERT INTO call_inbound_owners (tenant_id, application_id, environment_id, service_account_id, \
                   credential_id, enabled, revision, created_at, updated_at) \
                 SELECT tenant_id, application_id, environment_id, service_account_id, id, true, 1, now(), now() \
                 FROM platform_api_credentials WHERE id = '{credential}'"
            ))
            .await
            .unwrap();
        // The desk phone calls as `caller`; `member` answers application calls.
        let caller = e.member("desk-caller").await;
        let member_p = e.member("app-member").await;
        let member = e.a.device(&member_p, "member-laptop").await;
        let r = e
            .admin(
                Method::POST,
                "/v1/enterprise/sip-devices",
                Some(json!({ "deviceRef": DEVICE_REF, "principalId": caller.id(), "displayLabel": "Cert desk" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let member_ext = e
            .extension("6101", "ENTERPRISE_MEMBER", member_p.id())
            .await;
        // The member's mailbox (voicemail administration needs its own scope).
        let vm = common::enterprise::credential(
            e.h(),
            &e.a.w.consumer,
            e.a.w.kind,
            &[
                calling::platform::scopes::names::ENTERPRISE_CONFIG_READ,
                calling::platform::scopes::names::ENTERPRISE_VOICEMAIL_READ,
                calling::platform::scopes::names::ENTERPRISE_VOICEMAIL_MANAGE,
            ],
        )
        .await;
        let vm_token = vm.secret.expose_once().to_string();
        // Recording administration: its own scopes and the operator-granted capability.
        e.h()
            .state
            .enterprise
            .operator_set_capabilities(
                e.a.w.scope(),
                vec![
                    calling::enterprise::policy::AdminCapability::ManageRouting,
                    calling::enterprise::policy::AdminCapability::ManageVoicemail,
                    calling::enterprise::policy::AdminCapability::ManageRecording,
                ],
            )
            .await
            .unwrap();
        let rec = common::enterprise::credential(
            e.h(),
            &e.a.w.consumer,
            e.a.w.kind,
            &[
                calling::platform::scopes::names::ENTERPRISE_CONFIG_READ,
                calling::platform::scopes::names::ENTERPRISE_RECORDING_READ,
                calling::platform::scopes::names::ENTERPRISE_RECORDING_MANAGE,
            ],
        )
        .await;
        let rec_token = rec.secret.expose_once().to_string();
        let r = e
            .send(
                &vm_token,
                Method::POST,
                "/v1/enterprise/voicemail/mailboxes",
                Some(
                    json!({ "owner": { "type": "PRINCIPAL", "id": member_p.id() },
                             "displayLabel": "Cert member voicemail", "maxMessageSeconds": 60,
                             "retentionDays": 30 }),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let mailbox = id_of(&r.json, "mailboxId");
        let q = e
            .create(
                "/v1/enterprise/queues",
                json!({ "name": "Cert support", "maxWaitSeconds": 600, "offerTimeoutSeconds": 20, "capacity": 10 }),
            )
            .await;
        let flow = e
            .create(
                "/v1/enterprise/ivr-flows",
                json!({ "name": "Cert main menu" }),
            )
            .await;
        let flow = id_of(&flow, "ivrFlowId");
        let r = e
            .admin(
                Method::POST,
                &format!("/v1/enterprise/ivr-flows/{flow}/versions"),
                Some(json!({ "graph": { "startNode": "hello", "nodes": [
                    { "id": "hello", "kind": "PROMPT", "prompt": { "builtin": "WELCOME" }, "next": "main" },
                    { "id": "main", "kind": "MENU", "prompt": { "builtin": "MENU" }, "inputTimeoutSeconds": 6,
                      "maxRetries": 1, "options": [
                        { "digit": "1", "next": "sales" }, { "digit": "2", "next": "support" },
                        { "digit": "7", "next": "voicemail" }, { "digit": "0", "next": "bye" }],
                      "onInvalid": "bye", "onTimeout": "bye" },
                    { "id": "sales", "kind": "ROUTE_EXTENSION", "extensionId": id_of(&member_ext, "extensionId") },
                    { "id": "support", "kind": "ROUTE_QUEUE", "queueId": id_of(&q, "queueId") },
                    { "id": "voicemail", "kind": "ROUTE_VOICEMAIL", "mailboxId": mailbox },
                    { "id": "bye", "kind": "HANGUP", "prompt": { "builtin": "GOODBYE" } } ] } })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let r = e
            .admin(
                Method::PUT,
                &format!("/v1/enterprise/ivr-flows/{flow}/activation"),
                Some(json!({ "expectedVersion": 2, "versionNumber": 1 })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        e.extension("6100", "IVR", flow).await;

        // Serve Calling on its real port (Telephony delivers events, offers and keys).
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", calling_port))
            .await
            .expect("calling port");
        let app = e.h().router();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // Calling's workers on wall-clock time: command dispatch, expiries, IVR timers.
        let pump = Arc::new(AtomicBool::new(true));
        {
            let pump = pump.clone();
            let app = e.h().state.app.clone();
            let clock = e.h().clock.clone();
            tokio::spawn(async move {
                while pump.load(Ordering::SeqCst) {
                    clock.set(Utc::now());
                    let _ = app.dispatch_telephony_commands(50).await;
                    let _ = app.expire_due(50).await;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            });
        }
        let tdb = Database::connect(&telephony_db.url).await.unwrap();
        Some(Self {
            e,
            asterisk,
            _telephony: telephony,
            telephony_db,
            tdb,
            dir,
            _server: server,
            pump,
            caller: caller.id().as_uuid(),
            member,
            member_principal: member_p.id().as_uuid(),
            flow,
            mailbox: store.as_ref().map(|_| mailbox),
            store,
            vm_token,
            rec_token,
            public_media,
        })
    }

    async fn text(&self, sql: &str) -> String {
        common::enterprise::strings(self.e.h().conn(), sql)
            .await
            .join(",")
    }

    async fn ttext(&self, sql: &str) -> String {
        self.tdb
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql.to_string(),
            ))
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get_by_index::<Option<String>>(0).ok().flatten())
            .unwrap_or_default()
    }

    /// The newest Call of the desk phone's principal.
    async fn last_call(&self) -> Option<Uuid> {
        common::enterprise::strings(
            self.e.h().conn(),
            &format!(
                "SELECT id::text FROM calls WHERE initiator_principal_id = '{}' AND origin = 'ENTERPRISE_DEVICE' \
                  ORDER BY created_at DESC LIMIT 1",
                self.caller
            ),
        )
        .await
        .pop()
        .and_then(|s| s.parse().ok())
    }

    async fn diagnostics(&self) -> String {
        format!(
            "telephony commands: {}\ntelephony legs: {}\ncalling commands: {}\nivr: {}\nsteps: {}\n--- telephony.log ---\n{}\n--- asterisk ---\n{}",
            self.ttext("SELECT string_agg(kind || ':' || status || ':' || coalesce(result_code, '-'), ' | ' ORDER BY created_at) FROM telephony_commands").await,
            self.ttext("SELECT string_agg(route_class || ':' || status || ':' || coalesce(end_reason, '-'), ' | ') FROM telephony_legs").await,
            self.text("SELECT kind || ':' || status || ':' || coalesce(last_error_code, '-') FROM call_telephony_commands ORDER BY created_at").await,
            self.text("SELECT state || ':' || current_node || ':' || retries || ':' || coalesce(selected_branch, '-') FROM enterprise_ivr_sessions ORDER BY started_at").await,
            self.text("SELECT decision || ':' || reason FROM enterprise_routing_steps ORDER BY decided_at, step_number").await,
            std::fs::read_to_string(self.dir.join("telephony.log")).unwrap_or_default().lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"),
            self.asterisk.console().lines().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"),
        )
    }

    async fn eventually<F, Fut>(&self, what: &str, within: Duration, mut check: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if check().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        panic!("{what} did not happen\n{}", self.diagnostics().await);
    }

    async fn state(&self, call: Uuid) -> CallState {
        self.e.a.state(call).await.0
    }

    async fn session(&self, call: Uuid) -> String {
        self.text(&format!(
            "SELECT state || ':' || retries || ':' || coalesce(selected_branch, '-') FROM enterprise_ivr_sessions \
              WHERE call_id = '{call}'"
        ))
        .await
    }

    /// Prompts commanded on the device leg of `call` (Calling's durable commands, which
    /// Telephony executed at the edge).
    async fn prompts(&self, call: Uuid) -> Vec<String> {
        common::enterprise::strings(
            self.e.h().conn(),
            &format!(
                "SELECT (payload ->> 'prompt') || ':' || status FROM call_telephony_commands \
                  WHERE call_id = '{call}' AND kind = 'PROMPT' ORDER BY created_at, idempotency_key"
            ),
        )
        .await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enterprise_sip_ivr_certification() {
    let Some(stack) = Stack::start().await else {
        return;
    };
    let mut media_port = 17000;
    let mut next_port = || {
        media_port += 200;
        media_port
    };

    // 1. Prompt, valid key 1 -> extension (the member's application endpoint rings on the
    //    SAME Call); the desk phone then hangs up -> ABANDONED.
    let mut sipp = Sipp::dial("6100", 3500, &['1'], End::CallerHangsUp(5000), next_port());
    stack
        .eventually("the IVR routed key 1", Duration::from_secs(25), || async {
            match stack.last_call().await {
                Some(call) => stack.session(call).await == "ROUTED:0:sales",
                None => false,
            }
        })
        .await;
    let call = stack.last_call().await.unwrap();
    assert!(stack
        .e
        .a
        .invitation_for(call, stack.member.endpoint)
        .await
        .is_some());
    let prompts = stack.prompts(call).await;
    assert!(
        prompts.starts_with(&["WELCOME:ACCEPTED".into(), "MENU:ACCEPTED".into()]),
        "{prompts:?}"
    );
    assert_eq!(
        stack
            .ttext("SELECT string_agg(DISTINCT kind || ':' || status, ',') FROM telephony_commands WHERE kind IN ('ANSWER', 'PROMPT')")
            .await,
        "ANSWER:SUCCEEDED,PROMPT:SUCCEEDED"
    );
    assert!(
        sipp.finished_ok(Duration::from_secs(40)).await,
        "{}",
        sipp.diagnostics()
    );
    stack
        .eventually(
            "the hang-up abandoned the Call",
            Duration::from_secs(15),
            || async { stack.state(call).await == CallState::Cancelled },
        )
        .await;
    assert_eq!(
        stack
            .text(&format!(
                "SELECT outcome FROM enterprise_call_contexts WHERE call_id = '{call}'"
            ))
            .await,
        "ABANDONED"
    );
    assert_eq!(
        stack
            .e
            .count(&format!("SELECT count(*) FROM calls WHERE id = '{call}'"))
            .await,
        1
    );
    println!(
        "enterprise SIP IVR certified: prompt + valid key -> extension, caller hang-up abandoned"
    );

    // 2. Invalid key 9 (retry: INVALID_OPTION + MENU) then key 2 -> queue.
    let mut sipp = Sipp::dial(
        "6100",
        3500,
        &['9', '2'],
        End::CallerHangsUp(4000),
        next_port(),
    );
    stack
        .eventually(
            "the IVR routed key 2 after an invalid key",
            Duration::from_secs(30),
            || async {
                match stack.last_call().await {
                    Some(c) if c != call => stack.session(c).await == "ROUTED:0:support",
                    _ => false,
                }
            },
        )
        .await;
    let call2 = stack.last_call().await.unwrap();
    let prompts = stack.prompts(call2).await;
    assert!(
        prompts.iter().any(|p| p.starts_with("INVALID_OPTION")),
        "{prompts:?}"
    );
    assert_eq!(
        stack
            .text(&format!(
                "SELECT state FROM enterprise_queue_entries WHERE call_id = '{call2}'"
            ))
            .await,
        "WAITING"
    );
    assert!(
        sipp.finished_ok(Duration::from_secs(40)).await,
        "{}",
        sipp.diagnostics()
    );
    stack
        .eventually(
            "the queued caller's hang-up",
            Duration::from_secs(15),
            || async {
                stack
                    .text(&format!(
                        "SELECT state FROM enterprise_queue_entries WHERE call_id = '{call2}'"
                    ))
                    .await
                    == "ABANDONED"
            },
        )
        .await;
    println!("enterprise SIP IVR certified: invalid key + retry, valid key -> queue, abandonment");

    // 3. No key: NO_INPUT + retry, then onTimeout -> HANGUP; the PLATFORM hangs up.
    let mut sipp = Sipp::dial("6100", 1000, &[], End::PlatformHangsUp(40000), next_port());
    assert!(
        sipp.finished_ok(Duration::from_secs(60)).await,
        "{}\n{}",
        sipp.diagnostics(),
        stack.diagnostics().await
    );
    let call3 = stack.last_call().await.unwrap();
    assert_eq!(stack.state(call3).await, CallState::Declined);
    assert!(stack.session(call3).await.starts_with("COMPLETED"));
    let prompts = stack.prompts(call3).await;
    assert!(
        prompts.iter().any(|p| p.starts_with("NO_INPUT")),
        "{prompts:?}"
    );
    assert!(
        prompts.iter().any(|p| p.starts_with("GOODBYE")),
        "{prompts:?}"
    );
    assert_eq!(
        stack
            .text(&format!(
                "SELECT outcome FROM enterprise_call_contexts WHERE call_id = '{call3}'"
            ))
            .await,
        "COMPLETED_IN_IVR"
    );
    println!("enterprise SIP IVR certified: timeout + retry -> hangup by the platform");

    // 4. Explicit hangup branch (key 0): the platform hangs up.
    let mut sipp = Sipp::dial(
        "6100",
        3500,
        &['0'],
        End::PlatformHangsUp(30000),
        next_port(),
    );
    assert!(
        sipp.finished_ok(Duration::from_secs(45)).await,
        "{}\n{}",
        sipp.diagnostics(),
        stack.diagnostics().await
    );
    let call4 = stack.last_call().await.unwrap();
    assert_eq!(stack.state(call4).await, CallState::Declined);
    // GOODBYE played before the platform hung up.
    let prompts = stack.prompts(call4).await;
    assert!(
        prompts.iter().any(|p| p.starts_with("GOODBYE")),
        "{prompts:?}"
    );
    assert_eq!(stack.session(call4).await, "COMPLETED:0:bye");
    println!("enterprise SIP IVR certified: hangup branch");

    // 5. Voicemail (§60): key 7 routes the SAME Call to the member's mailbox; the edge plays
    //    the greeting and records the caller's real audio; Telephony delivers it, Calling
    //    stores it in the private bucket; READY; authorized retrieval; read; deletion.
    if let (Some(store), Some(mailbox)) = (stack.store.clone(), stack.mailbox) {
        voicemail_scenario(&stack, &store, mailbox, next_port()).await;
    } else {
        println!(
            "SKIPPED voicemail certification: no S3-compatible store (ENTERPRISE_CERT_S3_ENDPOINT)"
        );
    }

    // 6. Recording (§61): a desk phone calls the member's application endpoint; the Call is
    //    bridged to application media at the edge; recording is denied without consent,
    //    then recorded (bridge mix) with server-issued notice, stopped explicitly, stored.
    if let Some(store) = stack.store.clone() {
        let (device_port, media_port) = (next_port(), next_port());
        recording_scenario(&stack, &store, device_port, media_port).await;
    } else {
        println!(
            "SKIPPED recording certification: no S3-compatible store (ENTERPRISE_CERT_S3_ENDPOINT)"
        );
    }

    // No telephone number, SIP URI or key sequence reached Calling's database.
    let dump = common::dump_database(stack.e.h().conn()).await;
    assert!(
        !dump.contains("ent-desk-1"),
        "the edge endpoint leaked into Calling"
    );
    assert!(!dump.contains("sip:"), "a SIP URI leaked into Calling");
    let _ = (&stack.telephony_db, stack.flow);
}

async fn voicemail_scenario(
    stack: &Stack,
    store: &cf_object_storage::S3ObjectStore,
    mailbox: Uuid,
    media_port: u16,
) {
    use cf_object_storage::ObjectStore;
    let before = stack.last_call().await;
    let mut sipp = Sipp::dial(
        "6100",
        3500,
        &['7'],
        End::LeavesMessage {
            wait_ms: 8000,
            ms: 8000,
        },
        media_port,
    );
    stack
        .eventually(
            "the IVR routed key 7 to voicemail",
            Duration::from_secs(25),
            || async {
                match stack.last_call().await {
                    Some(c) if Some(c) != before => stack.session(c).await == "ROUTED:0:voicemail",
                    _ => false,
                }
            },
        )
        .await;
    let call = stack.last_call().await.unwrap();
    assert!(
        sipp.finished_ok(Duration::from_secs(60)).await,
        "{}\n{}",
        sipp.diagnostics(),
        stack.diagnostics().await
    );
    stack
        .eventually(
            "the voicemail message became READY",
            Duration::from_secs(40),
            || async {
                stack
                    .text(&format!(
                        "SELECT state FROM voicemail_messages WHERE call_id = '{call}'"
                    ))
                    .await
                    == "READY"
            },
        )
        .await;
    let message: Uuid = stack
        .text(&format!(
            "SELECT id::text FROM voicemail_messages WHERE call_id = '{call}'"
        ))
        .await
        .parse()
        .unwrap();
    // The greeting played, the capture ran at the edge, the audio is real (seconds of it).
    let prompts = stack.prompts(call).await;
    assert!(
        prompts.iter().any(|p| p == "VOICEMAIL_GREETING:ACCEPTED"),
        "{prompts:?}"
    );
    assert_eq!(
        stack
            .text(&format!(
                "SELECT kind || ':' || status FROM call_telephony_commands WHERE call_id = '{call}' AND kind = 'CAPTURE'"
            ))
            .await,
        "CAPTURE:ACCEPTED"
    );
    let duration: i64 = stack
        .text(&format!(
            "SELECT duration_ms::text FROM voicemail_messages WHERE id = '{message}'"
        ))
        .await
        .parse()
        .unwrap();
    assert!(duration >= 3000, "captured {duration} ms");
    assert_eq!(
        stack
            .text(&format!(
                "SELECT capture_source || ':' || content_type FROM voicemail_messages WHERE id = '{message}'"
            ))
            .await,
        "SIP:audio/wav"
    );
    // ONE Call, linked: the enterprise outcome is VOICEMAIL, and its events carry the
    // message. The caller hung up after speaking: CANCELLED when the hang-up reached
    // Calling first, NO_ANSWER when the stored message did (both end the ringing Call).
    let end = stack.state(call).await;
    assert!(
        matches!(end, CallState::Cancelled | CallState::NoAnswer),
        "{end:?}"
    );
    assert_eq!(
        stack
            .text(&format!(
                "SELECT outcome FROM enterprise_call_contexts WHERE call_id = '{call}'"
            ))
            .await,
        "VOICEMAIL"
    );
    assert_eq!(
        stack
            .text(&format!(
                "SELECT event_type FROM call_events WHERE call_id = '{call}' AND event_type LIKE 'VOICEMAIL_%' ORDER BY sequence"
            ))
            .await,
        "VOICEMAIL_RECEIVED,VOICEMAIL_READY"
    );
    // Telephony delivered it and deleted its transient spool copy.
    assert_eq!(
        stack
            .ttext(&format!(
                "SELECT status || ':' || (spool_deleted_at IS NOT NULL)::text FROM telephony_captures WHERE id = '{message}'"
            ))
            .await,
        "DELIVERED:true"
    );
    let spool = stack.asterisk.run.join("spool/recording");
    let leftovers: Vec<String> = std::fs::read_dir(&spool)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "edge spool not cleaned: {leftovers:?}"
    );
    // The object is in the private bucket with the recorded integrity metadata.
    let key = stack
        .text(&format!(
            "SELECT object_key FROM voicemail_messages WHERE id = '{message}'"
        ))
        .await;
    let (size, sha): (String, String) = {
        let v = stack
            .text(&format!(
                "SELECT size_bytes::text || '|' || sha256 FROM voicemail_messages WHERE id = '{message}'"
            ))
            .await;
        let (a, b) = v.split_once('|').unwrap();
        (a.to_string(), b.to_string())
    };
    let meta = store.head(&key).await.unwrap().expect("stored object");
    assert_eq!(meta.size.to_string(), size);
    // Authorized retrieval: the owner (through the application) gets a short-lived signed
    // URL; the bytes are the recorded WAV; the object is not readable unsigned.
    let path = format!("/v1/enterprise/voicemail/messages/{message}");
    let owner = stack.member_principal;
    let r = stack
        .e
        .send(
            &stack.vm_token,
            Method::GET,
            &format!("/v1/enterprise/voicemail/messages?mailboxId={mailbox}&principalId={owner}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["messages"][0]["messageId"], message.to_string());
    assert!(!r.text.contains(&key));
    let r = stack
        .e
        .send(
            &stack.vm_token,
            Method::POST,
            &format!("{path}/media-access"),
            Some(json!({ "principalId": owner })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let url = r.json["url"].as_str().unwrap().to_string();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let audio = http.get(&url).send().await.unwrap();
    assert_eq!(audio.status(), 200);
    let bytes = audio.bytes().await.unwrap().to_vec();
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(cf_object_storage::sha256_hex(&bytes), sha);
    let unsigned = url.split('?').next().unwrap().to_string();
    assert!(http
        .get(&unsigned)
        .send()
        .await
        .unwrap()
        .status()
        .is_client_error());
    // Another principal is not the owner: 404.
    let r = stack
        .e
        .send(
            &stack.vm_token,
            Method::POST,
            &format!("{path}/media-access"),
            Some(json!({ "principalId": stack.caller })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // Read state, then deletion: DELETING -> the object leaves the bucket -> DELETED.
    let version = stack
        .e
        .send(&stack.vm_token, Method::GET, &path, None)
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let r = stack
        .e
        .send(
            &stack.vm_token,
            Method::PUT,
            &format!("{path}/read-state"),
            Some(json!({ "expectedVersion": version, "readState": "READ", "principalId": owner })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = stack
        .e
        .send(
            &stack.vm_token,
            Method::DELETE,
            &format!("{path}?expectedVersion={}&principalId={owner}", version + 1),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    stack
        .eventually(
            "the message was deleted",
            Duration::from_secs(20),
            || async {
                stack
                    .text(&format!(
                        "SELECT state FROM voicemail_messages WHERE id = '{message}'"
                    ))
                    .await
                    == "DELETED"
            },
        )
        .await;
    assert!(store.head(&key).await.unwrap().is_none());
    assert_eq!(
        stack
            .text(&format!(
                "SELECT string_agg(operation, ',' ORDER BY seq) FROM enterprise_admin_audit WHERE resource_id = '{message}'"
            ))
            .await,
        "MEDIA_ACCESS,MARK_READ,DELETE"
    );
    // History (§62): the desk caller's canonical history holds the Call, answered by the
    // mailbox (enterprise outcome VOICEMAIL), never a raw number or object key.
    let r = stack
        .e
        .a
        .get(&format!("/v1/calls?principalId={}&limit=10", stack.caller))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let entry = r.json["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["callId"] == call.to_string())
        .cloned()
        .expect("the voicemail Call is in the caller's history");
    assert_eq!(entry["participation"]["direction"], "OUTBOUND");
    assert_eq!(entry["enterprise"]["outcome"], "VOICEMAIL");
    assert!(!r.text.contains("voicemail/") && !r.text.contains("sip:"));
    println!(
        "enterprise SIP voicemail certified: IVR -> mailbox, real edge capture ({duration} ms), \
         protected upload, READY, authorized retrieval, read, deletion, history"
    );
}

async fn recording_scenario(
    stack: &Stack,
    store: &cf_object_storage::S3ObjectStore,
    device_port: u16,
    media_port: u16,
) {
    use cf_object_storage::ObjectStore;
    let e = &stack.e;
    let rec = |method: Method, path: String, body: Option<serde_json::Value>| {
        let token = stack.rec_token.clone();
        async move { e.send(&token, method, &path, body).await }
    };
    // A policy that requires every participant's consent: the desk phone cannot give it.
    let r = rec(
        Method::POST,
        "/v1/enterprise/recording-policies".into(),
        Some(json!({ "name": "Cert recording", "mode": "MANUAL_ALLOWED", "noticeRequired": true,
                     "consentParticipants": "ALL_PARTICIPANTS", "jurisdictionRef": "CERT-LOOPBACK/v1",
                     "retentionDays": 30, "maxRecordingSeconds": 600, "isDefault": true })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let policy = id_of(&r.json, "recordingPolicyId");
    let before = stack.last_call().await;
    let attached = stack.public_media.attached.lock().unwrap().len();
    // The desk phone dials the member's extension; it speaks twice (real G.711 RTP) and
    // hangs up 16 s after the answer.
    let mut device = Sipp::dial(
        "6101",
        2000,
        &[],
        End::LeavesMessage {
            wait_ms: 4000,
            ms: 10000,
        },
        device_port,
    );
    stack
        .eventually(
            "the member's endpoint rings",
            Duration::from_secs(20),
            || async {
                match stack.last_call().await {
                    Some(c) if Some(c) != before => stack
                        .e
                        .a
                        .invitation_for(c, stack.member.endpoint)
                        .await
                        .is_some(),
                    _ => false,
                }
            },
        )
        .await;
    let call = stack.last_call().await.unwrap();
    let invitation =
        e.a.invitation_for(call, stack.member.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, invitation, &stack.member, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // The RTC side joins the edge bridge with its single-use code (SIPp as the gateway).
    stack
        .eventually(
            "the bridge's join code",
            Duration::from_secs(20),
            || async { stack.public_media.attached.lock().unwrap().len() > attached },
        )
        .await;
    let code = stack
        .public_media
        .attached
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .2
        .clone();
    let mut app_media = Sipp::app_media(&code, media_port);
    let session = e.a.session_on(call, stack.member.endpoint).await.unwrap();
    e.a.connect(call, session).await;
    let deadline = Instant::now() + Duration::from_secs(20);
    while stack.state(call).await != CallState::Connected {
        if Instant::now() > deadline {
            panic!(
                "the Call is CONNECTED (bridged) did not happen: call {:?}; public bridges {}; \
                 telephony bridges {}; bridge members {}; legs {}\n{}\n{}",
                stack.state(call).await,
                stack
                    .text(&format!(
                        "SELECT kind || ':' || status FROM call_public_bridges WHERE call_id = '{call}'"
                    ))
                    .await,
                stack
                    .ttext("SELECT string_agg(kind || ':' || status, ' | ') FROM telephony_bridges")
                    .await,
                stack
                    .ttext("SELECT string_agg(member_kind || ':' || status, ' | ') FROM telephony_bridge_legs")
                    .await,
                stack
                    .text(&format!(
                        "SELECT leg_kind || ':' || state FROM call_legs WHERE call_id = '{call}'"
                    ))
                    .await,
                app_media.diagnostics(),
                stack.diagnostics().await
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Permission to call is not permission to record: no consent from the desk phone.
    let member = stack
        .text(&format!(
            "SELECT principal_id::text FROM call_participants WHERE call_id = '{call}' AND role = 'RECIPIENT'"
        ))
        .await;
    let r = rec(
        Method::POST,
        format!("/v1/calls/{call}/recordings"),
        Some(json!({ "principalId": member })),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "RECORDING_NOT_AUTHORIZED");
    // Compliance configuration changes: notice only. The recording is authorized.
    let r = rec(
        Method::PATCH,
        format!("/v1/enterprise/recording-policies/{policy}"),
        Some(json!({ "expectedVersion": 1, "consentParticipants": "NONE" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = rec(
        Method::POST,
        format!("/v1/calls/{call}/recordings"),
        Some(json!({ "principalId": member })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let recording = id_of(&r.json["recording"], "recordingId");
    assert_eq!(r.json["recording"]["captureSource"], "SIP");
    let decisions = rec(
        Method::GET,
        format!("/v1/calls/{call}/recording-authorizations"),
        None,
    )
    .await;
    let d = decisions.json["authorizations"].as_array().unwrap().clone();
    assert_eq!(d[0]["reason"], "CONSENT_MISSING");
    assert_eq!(d[1]["decision"], "ALLOWED");
    let channels: Vec<String> = d[1]["notice"]["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["channel"].as_str().unwrap().to_string())
        .collect();
    assert!(channels.contains(&"EDGE_TONE".to_string()), "{channels:?}");
    assert!(
        channels.contains(&"APPLICATION_SIGNAL".to_string()),
        "{channels:?}"
    );
    // The edge records the bridge's mixed audio.
    let name = format!("cfm-cap-{}", recording.simple());
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    stack
        .eventually(
            "the edge records the bridge",
            Duration::from_secs(15),
            || async {
                match http
                    .get(format!("{ARI_URL}/ari/recordings/live/{name}"))
                    .basic_auth(ARI_USER, Some(ARI_PASSWORD))
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => r
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|v| v["target_uri"].as_str().map(|t| t.starts_with("bridge:")))
                        .unwrap_or(false),
                    _ => false,
                }
            },
        )
        .await;
    // Both sides speak (the desk phone's second sentence starts 6 s after the answer).
    tokio::time::sleep(Duration::from_secs(9)).await;
    let r = rec(
        Method::POST,
        format!("/v1/calls/{call}/recordings/{recording}/stop"),
        Some(json!({ "principalId": member })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["recording"]["stopReason"], "EXPLICIT");
    stack
        .eventually(
            "the recording is READY",
            Duration::from_secs(30),
            || async {
                stack
                    .text(&format!(
                        "SELECT state FROM call_recordings WHERE id = '{recording}'"
                    ))
                    .await
                    == "READY"
            },
        )
        .await;
    assert_eq!(
        stack
            .ttext("SELECT string_agg(DISTINCT kind || ':' || status, ',') FROM telephony_commands WHERE kind = 'CAPTURE_STOP'")
            .await,
        "CAPTURE_STOP:SUCCEEDED"
    );
    assert_eq!(
        stack
            .ttext(&format!(
                "SELECT status || ':' || (spool_deleted_at IS NOT NULL)::text FROM telephony_captures WHERE id = '{recording}'"
            ))
            .await,
        "DELIVERED:true"
    );
    let duration: i64 = stack
        .text(&format!(
            "SELECT duration_ms::text FROM call_recordings WHERE id = '{recording}'"
        ))
        .await
        .parse()
        .unwrap();
    assert!(duration >= 5000, "recorded {duration} ms");
    let key = stack
        .text(&format!(
            "SELECT object_key FROM call_recordings WHERE id = '{recording}'"
        ))
        .await;
    assert!(store.head(&key).await.unwrap().is_some());
    // Authorized retrieval of the same WAV; audited.
    let r = rec(
        Method::POST,
        format!("/v1/enterprise/recordings/{recording}/media-access"),
        Some(json!({ "principalId": member })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let bytes = http
        .get(r.json["url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(
        cf_object_storage::sha256_hex(&bytes),
        stack
            .text(&format!(
                "SELECT sha256 FROM call_recordings WHERE id = '{recording}'"
            ))
            .await
    );
    // A principal who was not part of the Call is refused (404) and audited DENIED.
    let r = rec(
        Method::POST,
        format!("/v1/enterprise/recordings/{recording}/media-access"),
        Some(json!({ "principalId": Uuid::new_v4() })),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert_eq!(
        stack
            .text(&format!(
                "SELECT string_agg(operation || ':' || outcome, ',' ORDER BY seq) FROM call_recording_access_audit \
                  WHERE recording_id = '{recording}'"
            ))
            .await,
        "STOP:GRANTED,MEDIA_ACCESS:GRANTED,MEDIA_ACCESS:DENIED"
    );
    assert!(
        device.finished_ok(Duration::from_secs(40)).await,
        "{}\n{}",
        device.diagnostics(),
        stack.diagnostics().await
    );
    assert!(
        app_media.finished_ok(Duration::from_secs(20)).await,
        "{}",
        app_media.diagnostics()
    );
    assert_eq!(
        stack
            .text(&format!(
                "SELECT event_type FROM call_events WHERE call_id = '{call}' AND event_type LIKE 'RECORDING_%' ORDER BY sequence"
            ))
            .await,
        "RECORDING_STARTED,RECORDING_STOPPED,RECORDING_READY"
    );
    println!(
        "enterprise SIP recording certified: denied without consent, notice (edge tone + app signal), \
         bridge recording ({duration} ms), explicit stop, protected upload, audited retrieval"
    );
}
